//! `rho-core`: a long-lived host that holds one Core for a project, and a thin
//! client so that separate invocations, such as an Agent's shell calls, can
//! submit, wait for, read and cancel runs the host keeps holding. It uses only
//! the library's public API; see `docs/HOST-CLI.md`.

mod client;
mod host;
mod wire;

use std::collections::VecDeque;
use std::process::ExitCode;
use std::str::FromStr;

const USAGE: &str = "\
rho-core: a host that holds managed runs for one project, and a client for it.

Start the host (stays in the foreground; SIGTERM or Ctrl-C stops it and its runs):
  rho-core serve --project DIR --state-dir DIR --executor NAME=/absolute/program
                 [--executor-arg NAME=ARG]... [--limit NAME=VALUE]... [--max-connections N]
  When ready it prints one JSON line with the socket path.

Client commands (one request each; the reply is one JSON line on stdout):
  rho-core submit --id ID --executor NAME [--workdir DIR]
                  (--code TEXT | --code-file PATH | --code-file -) [-- ARG...]
  rho-core wait --id ID [--timeout SECONDS]       (default 30, at most 600)
  rho-core read --id ID [--stream stdout|stderr] [--offset N] [--max-bytes N]
  rho-core lookup --id ID [--record]
  rho-core list
  rho-core cancel --id ID
  rho-core forget --id ID
  rho-core capability [--contract]

Client commands need --socket PATH (or RHO_CORE_SOCKET); all but capability also
need --caller NAME (or RHO_CORE_CALLER). IDs and callers are 1-128 characters of
[A-Za-z0-9._:@-]. An ID names one request of that caller: submitting it again with
the same content returns the original run and starts nothing; other content is
a conflict. To run again, use a new ID. The workdir is relative to the project.

Exit status:
  0  the host carried out the request (a wait may still end before the run does)
  1  the host answered with an error; see error.kind and error.message
  2  usage error; nothing was sent
  3  not delivered: no host, host busy or incompatible; nothing was done
  4  sent, but no reply arrived: the request may have been carried out. Repeat it
     with the same ID, or look the ID up, to find out; do not retry under a new ID.

Details: docs/HOST-CLI.md";

fn main() -> ExitCode {
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(arg) => {
                eprintln!("rho-core: argument {arg:?} is not UTF-8");
                return ExitCode::from(2);
            }
        }
    }
    match args.first().map(String::as_str) {
        None | Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("serve") => host::main(args.split_off(1)),
        Some(_) => client::main(args),
    }
}

/// Command-line options in `--flag value` or `--flag=value` form.
pub struct Args {
    items: VecDeque<String>,
    pending: Option<String>,
}

impl Args {
    pub fn new(items: Vec<String>) -> Self {
        Self {
            items: items.into(),
            pending: None,
        }
    }

    /// The next `--flag`, or `None` at the end or at `--`.
    pub fn next_flag(&mut self) -> Result<Option<String>, String> {
        if let Some(value) = self.pending.take() {
            return Err(format!("unexpected value `{value}`"));
        }
        match self.items.pop_front() {
            None => Ok(None),
            Some(item) if item == "--" => Ok(None),
            Some(item) if item.starts_with("--") => match item.split_once('=') {
                Some((flag, value)) => {
                    self.pending = Some(value.to_owned());
                    Ok(Some(flag.to_owned()))
                }
                None => Ok(Some(item)),
            },
            Some(item) => Err(format!("unexpected argument `{item}`")),
        }
    }

    pub fn value(&mut self, flag: &str) -> Result<String, String> {
        self.pending
            .take()
            .or_else(|| self.items.pop_front())
            .ok_or_else(|| format!("{flag} needs a value"))
    }

    /// Whatever follows `--`.
    pub fn rest(self) -> Vec<String> {
        self.items.into()
    }
}

pub fn number<T: FromStr>(flag: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{flag}: `{value}` is not a valid number"))
}
