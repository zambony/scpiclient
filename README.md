A small command-line tool to send SCPI commands to T&M instruments over ethernet.

## Why?
You could use `nc`, but it doesn't have entry history, so re-running commands is a pain.  
You could use NI-MAX, but then you have to install a ton of stuff.  
You could make a Python script, but then you need to make a Python script.

A quick command-line tool that you know is dedicated for this purpose just feels nicer to me, personally, and I plan to add
more helpful features as I need them. An important feature for me was being able to pipe in a file of SCPI commands to run to easily replicate
customer errors.

Also, as this is statically-compiled and produces a small binary, it can be easily put on a USB drive and transferred to
a computer that needs a way to quickly run SCPI commands.

## Install
Requires [cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html).

With cargo, run:
```sh
cargo install --git https://github.com/zambony/scpiclient
```

Done!

Run `scpi --help` in a terminal to see how to use it.

## Note
There's nothing really SCPI-specific about this utility, except that it only receives incoming data when a query command is entered.  
You could technically use this to send (not receive) data to any TCP socket, if you wanted.

I may add some kind of SCPI validation to warn of ill-formed commands before sending them, or maybe syntax highlighting of some kind,
but for now it's just a quick utility. Feel free to try it yourself if you'd like.