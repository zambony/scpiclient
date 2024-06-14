use std::borrow::Cow::{self, Borrowed};
use std::io;
use std::io::prelude::*;
use std::net::ToSocketAddrs;
use std::process::exit;
use std::time::Duration;

use anyhow::Context;
use atty::Stream::Stdin;
use clap::builder::styling::{AnsiColor, Color::Ansi};
use clap::builder::styling::{Style, Styles};
use clap::Parser;
use dns_lookup::lookup_addr;
use owo_colors::OwoColorize;
use rustyline::config::Configurer;
use rustyline::highlight::Highlighter;
use rustyline::{Completer, Helper, Hinter, Validator};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[cfg(not(debug_assertions))]
use regex::Regex;

type GenericResult = anyhow::Result<()>;

/// A lightweight interactive SCPI client that handles basic commands and queries.
/// Also accepts piped input or input redirected from a file (one command per line).
#[derive(Parser, Debug)]
#[command(version, about, verbatim_doc_comment, styles = STYLES, name = "scpi")]
struct Args {
    /// The host to connect to.
    #[arg()]
    host: String,

    /// The port to use.
    #[arg()]
    port: u16,

    /// A command/query to run and immediately exit.
    #[arg(short)]
    command: Option<String>,
}

const HEADER_STYLE: Style = Style::new().bold().fg_color(Some(Ansi(AnsiColor::Green)));
const PLACEHOLDER_STYLE: Style = Style::new().fg_color(Some(Ansi(AnsiColor::Cyan)));

const STYLES: Styles = Styles::styled()
    .literal(AnsiColor::BrightCyan.on_default().bold())
    .header(HEADER_STYLE)
    .usage(HEADER_STYLE)
    .placeholder(PLACEHOLDER_STYLE);

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

async fn read_until_terminator(connection: &mut TcpStream) -> anyhow::Result<String> {
    let mut buffer = String::new();
    let timeout_length = Duration::from_secs(5);
    let mut reader = BufReader::new(connection);

    tokio::time::timeout(timeout_length, reader.read_line(&mut buffer))
        .await
        .context("Timed out waiting for query response")??;

    return Ok(buffer);
}

fn get_host(destination: &str) -> anyhow::Result<String> {
    let mut iter = format!("{}:0", destination)
        .to_socket_addrs()
        .context("Failed to parse host for hostname lookup")?;
    let hostname =
        lookup_addr(&iter.next().unwrap().ip()).context("Failed to lookup hostname of address")?;

    return Ok(hostname);
}

async fn write_cmd(connection: &mut TcpStream, command: &str) -> anyhow::Result<Option<String>> {
    let is_query_cmd = is_query(command);
    let mut cmd_copy = command.to_owned();

    if !cmd_copy.ends_with('\n') {
        cmd_copy.push('\n');
    }

    connection.write_all(cmd_copy.as_bytes()).await?;

    if is_query_cmd {
        let response = read_until_terminator(connection).await;

        return match response {
            Ok(ref text) => Ok(Some(text.trim().to_owned())),
            Err(err) => {
                eprintln!("{}", err);
                Ok(None)
            }
        };
    }

    return Ok(None);
}

async fn run(hostname: &str, port: u16, command: Option<&String>) -> GenericResult {
    let mut connection: TcpStream = TcpStream::connect((hostname, port)).await?;

    // If a command was passed in from the -c option, process it and exit.
    if let Some(cmd) = command {
        for line in cmd.lines() {
            let response = write_cmd(&mut connection, &line).await?;

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

    // Enter the input loop.
    loop {
        let read = rl.readline(&default_prompt);

        if read.is_err() {
            println!("Exiting.");
            exit(0);
        }

        let input = read.unwrap();

        rl.add_history_entry(&input)?;

        let response = write_cmd(&mut connection, &input).await?;

        if let Some(resp) = response {
            println!("{}", resp);
        };
    }
}

#[tokio::main(flavor = "current_thread")]
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
        let res = crate::run(&args.hostname, args.port, args.command.as_ref()).await;

        if let Err(ref inner) = res {
            let error_strip: Regex = Regex::new(r"\s*\(os error \d+\)").unwrap();
            eprintln!("ERROR: {}", error_strip.replace(&inner.to_string(), ""));
            exit(1);
        }
    }

    // Debug mode will pass errors straight to the return so we get a full backtrace.
    #[cfg(debug_assertions)]
    {
        run(&args.host, args.port, args.command.as_ref()).await?;
    }

    return Ok(());
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
