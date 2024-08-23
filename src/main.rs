use anyhow::Context;
use atty::Stream::Stdin;
use clap::{
    builder::styling::{AnsiColor, Color::Ansi, Style, Styles},
    Parser,
};
use owo_colors::OwoColorize;
use rustyline::{config::Configurer, highlight::Highlighter, Completer, Helper, Hinter, Validator};
use std::future::poll_fn;
use std::ops::DerefMut;
use std::task::Poll;
use std::{
    borrow::Cow::{self, Borrowed},
    io,
    process::exit,
    sync::Arc,
    time::Duration,
};
use std::io::Error;
use tokio::io::ReadBuf;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::RwLock,
};

type GenericResult = anyhow::Result<()>;

const HEADER_STYLE: Style = Style::new().bold().fg_color(Some(Ansi(AnsiColor::Green)));
const PLACEHOLDER_STYLE: Style = Style::new().fg_color(Some(Ansi(AnsiColor::Cyan)));

const STYLES: Styles = Styles::styled()
    .literal(AnsiColor::BrightCyan.on_default().bold())
    .header(HEADER_STYLE)
    .usage(HEADER_STYLE)
    .placeholder(PLACEHOLDER_STYLE);

/// A lightweight interactive SCPI client that handles basic commands and queries.
/// Also accepts piped input or input redirected from a file (one command per line).
#[derive(Parser, Debug)]
#[command(version, about, verbatim_doc_comment, styles = STYLES, name = "scpi")]
struct Args {
    /// The host to connect to.
    #[arg()]
    host: String,

    /// The port to use.
    #[arg(default_value = "9001")]
    port: u16,

    /// Number of seconds to wait for a query response.
    #[arg(short, default_value = "5")]
    timeout: u64,

    /// A command/query to run and immediately exit.
    #[arg(short)]
    command: Option<String>,
}

#[derive(Completer, Helper, Hinter, Validator)]
struct HighlightPrompt {
    colored_prompt: String,
}

impl Highlighter for HighlightPrompt {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        default: bool,
    ) -> Cow<'b, str> {
        if default {
            Borrowed(&self.colored_prompt)
        } else {
            Borrowed(prompt)
        }
    }
}

/// Determine if a command string is a query or not.
/// # Arguments
///
/// * `command`: The command string to check.
///
/// # Returns
///
/// True if the string contains a query command at the beginning, false if not.
fn is_query(command: &str) -> bool {
    if command.is_empty() {
        return false;
    }

    return command
        .split(" ")
        .collect::<Vec<_>>()
        .first()
        .unwrap()
        .trim()
        .ends_with("?");
}

/// Reads from the given stream until a newline is hit and returns the response, if any.
///
/// # Arguments
///
/// * `connection`: The stream to use.
/// * `timeout`: The time to wait before considering a response failed.
///
/// # Returns
/// An [`anyhow::Result<String>`] containing the read data.
async fn read_until_terminator<T>(connection: &mut T, timeout: u64) -> anyhow::Result<String>
where
    T: AsyncRead + Unpin,
{
    let mut buffer = String::new();
    let timeout_length = Duration::from_secs(timeout);
    let mut reader = BufReader::new(connection);

    tokio::time::timeout(timeout_length, reader.read_line(&mut buffer))
        .await
        .context("Timed out waiting for query response")??;

    return Ok(buffer);
}

/// Sends `command` to the supplied buffer and returns the query result, if any.
async fn write_cmd<T>(
    connection: &mut T,
    command: &str,
    timeout: u64,
) -> anyhow::Result<Option<String>>
where
    T: AsyncWrite + AsyncRead + Unpin,
{
    let is_query_cmd = is_query(command);
    let mut cmd_copy = command.to_owned();

    if !cmd_copy.ends_with('\n') {
        cmd_copy.push('\n');
    }

    connection.write_all(cmd_copy.as_bytes()).await?;

    if is_query_cmd {
        let response = read_until_terminator(connection, timeout).await;

        return match response {
            Ok(text) => Ok(Some(text.trim().to_owned())),
            Err(err) => {
                eprintln!("{}", err);
                Ok(None)
            }
        };
    }

    return Ok(None);
}

async fn try_peek(stream: &TcpStream, buf: &mut ReadBuf<'_>) -> Result<usize, Error> {
    let mut pending = true;

    return poll_fn(|cx| {
        let status = stream.poll_peek(cx, buf);

        pending = status.is_pending();

        // Lie to the poll function so it doesn't block.
        if pending {
            return Poll::Ready(Ok(1));
        }

        return status;
    })
    .await;
}

fn start_heartbeat(connection: Arc<RwLock<TcpStream>>, interval: Duration) {
    tokio::spawn(async move {
        let mut buf = [0u8; 1];
        let mut rb = ReadBuf::new(&mut buf);

        loop {
            // Inner scope to unlock the stream before sleeping.
            {
                let conn = connection.read().await;

                let size = try_peek(&conn, &mut rb).await;

                // If we were ready and saw a 0 byte read, connection closed or socket keepalive failed.
                if size.unwrap_or(1) == 0 {
                    println!("\nConnection lost.");
                    crossterm::terminal::disable_raw_mode().expect("Failed to disable raw mode");
                    exit(1);
                }
            }

            // Only poll every 5 seconds to avoid extra work.
            tokio::time::sleep(interval).await;
        }
    });
}

async fn run(hostname: &str, port: u16, command: Option<&str>, timeout: u64) -> GenericResult {
    let connection: TcpStream = TcpStream::connect((hostname, port)).await?;

    // Ugly hack to set the keepalive property of the tokio TcpStream.
    let connection = connection.into_std()?;
    connection.set_nonblocking(false)?;
    let socket = socket2::Socket::from(connection);
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(4))
        .with_interval(Duration::from_secs(1));
    
    #[cfg(!windows)]
    let keepalive = keepalive.with_retries(4);

    socket.set_tcp_keepalive(&keepalive)?;
    let connection: std::net::TcpStream = socket.into();

    // Turn the std connection back into a tokio stream now that keepalive is enabled.
    let connection = TcpStream::from_std(connection)?;

    let mut wrapped = Arc::new(RwLock::new(connection));

    // If a command was passed in from the -c option, process it and exit.
    if let Some(cmd) = command {
        for line in cmd.lines() {
            let response = write_cmd(
                Arc::get_mut(&mut wrapped).unwrap().get_mut(),
                &line,
                timeout,
            )
            .await?;

            if let Some(resp) = response {
                println!("{}", resp);
            };
        }

        return Ok(());
    }

    // Set up the prompt styling.
    let default_prompt = format!("{}> ", hostname);
    let helper = HighlightPrompt {
        colored_prompt: format!("{}> ", hostname.green()),
    };
    let mut rl = rustyline::Editor::new()?;
    rl.set_history_ignore_space(true);
    rl.set_helper(Some(helper));

    // Spawn a separate task that will poll the stream for whether it's closed.
    // Do this since the main task is stuck waiting for a readline.
    start_heartbeat(wrapped.clone(), Duration::from_secs(5));

    // Enter the input loop.
    loop {
        let read = rl.readline(&default_prompt);

        if read.is_err() {
            println!("Exiting.");
            exit(0);
        }

        let input = read.unwrap();

        rl.add_history_entry(&input)?;

        let response = write_cmd(wrapped.write().await.deref_mut(), &input, timeout).await?;

        if let Some(resp) = response {
            println!("{}", resp);
        };
    }
}

#[tokio::main]
async fn main() -> GenericResult {
    let mut args = Args::parse();

    // We're receiving piped or redirected data.
    if !atty::is(Stdin) {
        let lines: Vec<String> = io::stdin().lines().map(|x| x.unwrap()).collect();

        args.command = lines.join("\n").into();
    }

    // Release mode needs special error handling to not print backtraces for minor errors.
    #[cfg(not(debug_assertions))]
    {
        let res = crate::run(&args.host, args.port, args.command.as_deref(), args.timeout).await;

        if let Err(ref inner) = res {
            eprintln!("ERROR: {}", inner.to_string());
            exit(1);
        }
    }

    // Debug mode will pass errors straight to the return so we get a full backtrace.
    #[cfg(debug_assertions)]
    {
        run(&args.host, args.port, args.command.as_deref(), args.timeout).await?;
    }

    return Ok(());
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_test::io::Builder;

    #[test]
    fn query_format() {
        assert!(is_query("DIAG:DEB:REG?"));
        assert!(is_query("DIAG:DEB:REG? 0x200"));
        assert!(is_query("DIAG:DEB:REG? 0x200\n"));
        assert!(is_query("*IDN?"));
        assert!(is_query("*IDN?\n"));

        assert!(!is_query(""));
        assert!(!is_query("*RST"));
        assert!(!is_query("*SAV\n"));
        assert!(!is_query("HELLO:WORLD \"GOODBYE\""));
        assert!(!is_query("HELLO:WORLD \"GOODBYE\"\n"));
    }

    #[tokio::test]
    async fn response() {
        let mut mock_stream = Builder::new().write(b"QUERY?\n").read(b"123\n").build();

        let response = write_cmd(&mut mock_stream, "QUERY?", 5)
            .await
            .expect("Failed to write test query")
            .expect("Did not get test query response");

        assert_eq!(response, "123");
    }
}
