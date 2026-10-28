//! Client commands: each invocation sends one request to a running host and
//! prints its reply. Nothing runs in this process.

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use crate::Args;
use crate::ipc::IpcStream;
use crate::wire::{self, Greeting, Line, Request, WireStream};

const EXIT_ERROR: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_NOT_DELIVERED: u8 = 3;
const EXIT_UNKNOWN: u8 = 4;
/// Reply bound for requests other than `wait`.
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_REPLY_BYTES: usize = 64 << 20;
/// Bound on code read from a file or stdin; the host enforces its own limit.
const MAX_CODE_READ_BYTES: u64 = 64 << 20;
const DEFAULT_WAIT_SECS: f64 = 30.0;

struct Invocation {
    socket: PathBuf,
    request: Request,
    reply_timeout: Duration,
}

pub fn main(args: Vec<String>) -> ExitCode {
    let mut args = args.into_iter();
    let command = args.next().unwrap_or_default();
    let invocation = match parse(&command, Args::new(args.collect())) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("rho-core {command}: {message}\nRun `rho-core help` for usage.");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let contract = matches!(invocation.request, Request::Capability { contract: true });
    match exchange(
        &invocation.socket,
        &invocation.request,
        invocation.reply_timeout,
    ) {
        Ok(reply) => {
            let ok = reply["ok"] == json!(true);
            match reply["value"]["contract"].as_str() {
                Some(text) if ok && contract => {
                    print(text);
                    if let Some(activity) = reply["value"]["activity_contract"].as_str() {
                        print(activity);
                    }
                }
                _ => print(&reply.to_string()),
            }
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(EXIT_ERROR)
            }
        }
        Err((code, error)) => {
            print(&json!({"ok": false, "error": error}).to_string());
            ExitCode::from(code)
        }
    }
}

fn print(text: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

fn parse(command: &str, mut args: Args) -> Result<Invocation, String> {
    let known = [
        "submit",
        "wait",
        "read",
        "lookup",
        "list",
        "cancel",
        "forget",
        "capability",
        "activity-open",
        "activity-append",
        "activity-read",
        "activity-list",
        "activity-forget",
    ];
    if !known.contains(&command) {
        return Err(format!("unknown command `{command}`"));
    }
    let (mut socket, mut caller, mut id) = (None, None, None);
    let (mut executor, mut workdir, mut code, mut code_file) = (None, None, None, None);
    let (mut timeout, mut stream, mut offset, mut max_bytes) =
        (DEFAULT_WAIT_SECS, WireStream::Stdout, 0, usize::MAX);
    let (mut record, mut contract) = (false, false);
    let (mut source_file, mut events_file, mut expected_cursor) = (None, None, None);
    let (mut after, mut max_events) = (0, usize::MAX);
    while let Some(flag) = args.next_flag()? {
        match (command, flag.as_str()) {
            (_, "--socket") => socket = Some(args.value(&flag)?),
            (_, "--caller") => caller = Some(args.value(&flag)?),
            (
                "submit" | "wait" | "read" | "lookup" | "cancel" | "forget" | "activity-append"
                | "activity-read" | "activity-forget",
                "--id",
            ) => id = Some(args.value(&flag)?),
            ("submit", "--executor") => executor = Some(args.value(&flag)?),
            ("submit", "--workdir") => workdir = Some(args.value(&flag)?),
            ("submit", "--code") => code = Some(args.value(&flag)?),
            ("submit", "--code-file") => code_file = Some(args.value(&flag)?),
            ("wait", "--timeout") => timeout = crate::number(&flag, &args.value(&flag)?)?,
            ("read", "--stream") => {
                stream = match args.value(&flag)?.as_str() {
                    "stdout" => WireStream::Stdout,
                    "stderr" => WireStream::Stderr,
                    other => {
                        return Err(format!("--stream must be stdout or stderr, not `{other}`"));
                    }
                }
            }
            ("read", "--offset") => offset = crate::number(&flag, &args.value(&flag)?)?,
            ("read" | "activity-read", "--max-bytes") => {
                max_bytes = crate::number(&flag, &args.value(&flag)?)?
            }
            ("activity-open", "--source-file") => source_file = Some(args.value(&flag)?),
            ("activity-append", "--events-file") => events_file = Some(args.value(&flag)?),
            ("activity-read", "--after") => after = crate::number(&flag, &args.value(&flag)?)?,
            ("activity-read", "--limit") => max_events = crate::number(&flag, &args.value(&flag)?)?,
            ("activity-forget", "--expected-cursor") => {
                expected_cursor = Some(crate::number(&flag, &args.value(&flag)?)?)
            }
            ("lookup", "--record") => record = true,
            ("capability", "--contract") => contract = true,
            _ => return Err(format!("unknown option `{flag}` for {command}")),
        }
    }
    let run_args = args.rest();
    if command != "submit" && !run_args.is_empty() {
        return Err(format!("{command} takes no arguments after --"));
    }
    let socket = socket
        .or_else(|| std::env::var("RHO_CORE_SOCKET").ok())
        .ok_or("--socket PATH (or RHO_CORE_SOCKET) is required")?;
    let mut reply_timeout = REPLY_TIMEOUT;
    let request = if command == "capability" {
        Request::Capability { contract }
    } else {
        let caller = caller
            .or_else(|| std::env::var("RHO_CORE_CALLER").ok())
            .ok_or("--caller NAME (or RHO_CORE_CALLER) is required")?;
        let id = || id.clone().ok_or_else(|| "--id ID is required".to_owned());
        match command {
            "activity-open" => Request::ActivityOpen {
                caller,
                source: serde_json::from_str(&read_code(
                    None,
                    Some(source_file.ok_or("--source-file PATH|- is required")?),
                )?)
                .map_err(|e| format!("invalid activity source: {e}"))?,
            },
            "activity-append" => Request::ActivityAppend {
                caller,
                stream: id()?,
                events: serde_json::from_str(&read_code(
                    None,
                    Some(events_file.ok_or("--events-file PATH|- is required")?),
                )?)
                .map_err(|e| format!("invalid activity events: {e}"))?,
            },
            "activity-read" => Request::ActivityRead {
                caller,
                stream: id()?,
                after,
                max_events,
                max_bytes,
            },
            "activity-list" => Request::ActivityList { caller },
            "activity-forget" => Request::ActivityForget {
                caller,
                stream: id()?,
                expected_cursor: expected_cursor.ok_or("--expected-cursor N is required")?,
            },
            "submit" => Request::Submit {
                caller,
                request_id: id()?,
                executor: executor.ok_or("--executor NAME is required")?,
                workdir: workdir.unwrap_or_else(|| ".".into()),
                code: read_code(code, code_file)?,
                args: run_args,
            },
            "wait" => {
                if !(0.0..=wire::MAX_WAIT.as_secs_f64()).contains(&timeout) {
                    return Err(format!(
                        "--timeout must be between 0 and {} seconds",
                        wire::MAX_WAIT.as_secs()
                    ));
                }
                let timeout = Duration::from_secs_f64(timeout);
                reply_timeout = timeout + wire::IO_TIMEOUT;
                Request::Wait {
                    caller,
                    request_id: id()?,
                    timeout_ms: timeout.as_millis() as u64,
                }
            }
            "read" => Request::Read {
                caller,
                request_id: id()?,
                stream,
                offset,
                max_bytes,
            },
            "lookup" => Request::Lookup {
                caller,
                request_id: id()?,
                record,
            },
            "list" => Request::List { caller },
            "cancel" => Request::Cancel {
                caller,
                request_id: id()?,
            },
            _ => Request::Forget {
                caller,
                request_id: id()?,
            },
        }
    };
    Ok(Invocation {
        socket: socket.into(),
        request,
        reply_timeout,
    })
}

fn read_code(code: Option<String>, file: Option<String>) -> Result<String, String> {
    let path = match (code, file) {
        (Some(code), None) => return Ok(code),
        (None, Some(path)) => path,
        _ => return Err("give exactly one of --code TEXT or --code-file PATH|-".into()),
    };
    let mut bytes = Vec::new();
    let read = if path == "-" {
        io::stdin()
            .lock()
            .take(MAX_CODE_READ_BYTES + 1)
            .read_to_end(&mut bytes)
    } else {
        File::open(&path)
            .and_then(|file| file.take(MAX_CODE_READ_BYTES + 1).read_to_end(&mut bytes))
    };
    read.map_err(|e| format!("cannot read code from `{path}`: {e}"))?;
    if bytes.len() as u64 > MAX_CODE_READ_BYTES {
        return Err(format!(
            "code from `{path}` exceeds {MAX_CODE_READ_BYTES} bytes"
        ));
    }
    String::from_utf8(bytes).map_err(|_| format!("code from `{path}` is not UTF-8 text"))
}

fn failure(kind: &str, message: String) -> Value {
    json!({"kind": kind, "message": message})
}

/// One request and its reply. The error carries the exit status: whether the
/// request certainly did nothing, or may have been carried out.
fn exchange(
    socket: &Path,
    request: &Request,
    reply_timeout: Duration,
) -> Result<Value, (u8, Value)> {
    let not_delivered = |kind: &str, message: String| {
        (
            EXIT_NOT_DELIVERED,
            failure(kind, format!("{message}; nothing was sent")),
        )
    };
    let mut stream = IpcStream::connect(socket).map_err(|e| {
        not_delivered(
            "host_unavailable",
            format!("no host is listening at `{}` ({e})", socket.display()),
        )
    })?;
    let _ = stream.set_read_timeout(Some(wire::IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(wire::IO_TIMEOUT));
    let mut reader = BufReader::new(&stream);
    let greeting = match wire::read_line(&mut reader, wire::GREETING_MAX_BYTES) {
        Ok(Line::Complete(line)) => serde_json::from_slice::<Greeting>(&line).ok(),
        Ok(_) | Err(_) => {
            return Err(not_delivered(
                "host_unavailable",
                "the host closed or did not answer the connection".into(),
            ));
        }
    };
    let max_request_bytes = match greeting {
        Some(Greeting::Ready {
            protocol: wire::PROTOCOL,
            max_request_bytes,
        }) => max_request_bytes,
        Some(Greeting::Busy {
            protocol: wire::PROTOCOL,
            max_connections,
        }) => {
            return Err(not_delivered(
                "busy",
                format!("all {max_connections} host connections are in use; try again shortly"),
            ));
        }
        Some(Greeting::Refused {
            protocol: wire::PROTOCOL,
            reason,
        }) => {
            return Err(not_delivered(
                "refused",
                format!("the host refused this client: {reason}"),
            ));
        }
        _ => {
            return Err(not_delivered(
                "incompatible_host",
                format!(
                    "the host does not speak this client's protocol {}",
                    wire::PROTOCOL
                ),
            ));
        }
    };
    let mut line = serde_json::to_vec(request)
        .map_err(|e| (EXIT_ERROR, failure("internal", e.to_string())))?;
    line.push(b'\n');
    if line.len() > max_request_bytes {
        return Err((
            EXIT_ERROR,
            failure(
                "request_too_large",
                format!(
                    "the request takes {} bytes, more than the host's {max_request_bytes}; nothing was sent",
                    line.len()
                ),
            ),
        ));
    }
    // The newline is written last, and the host ignores a request without one.
    if let Err(error) = (&stream).write_all(&line) {
        return Err((
            EXIT_NOT_DELIVERED,
            failure(
                "host_unavailable",
                format!(
                    "the request could not be sent completely ({error}); the host ignores a partial request, so nothing was done"
                ),
            ),
        ));
    }
    let _ = stream.set_read_timeout(Some(reply_timeout));
    let unknown = |detail: String| {
        (
            EXIT_UNKNOWN,
            failure(
                "reply_unknown",
                format!(
                    "the request was sent but {detail}; it may have been carried out. Repeat it with \
                     the same ID, or look the ID up, to find out"
                ),
            ),
        )
    };
    let reply = match wire::read_line(&mut reader, MAX_REPLY_BYTES) {
        Ok(Line::Complete(line)) => serde_json::from_slice::<Value>(&line)
            .map_err(|e| unknown(format!("the reply is not JSON ({e})")))?,
        Ok(Line::TooLarge) => {
            return Err(unknown(format!(
                "the reply exceeds {MAX_REPLY_BYTES} bytes"
            )));
        }
        Ok(Line::Closed) => {
            return Err(unknown(
                "the host closed the connection before replying".into(),
            ));
        }
        Err(error) => return Err(unknown(format!("no reply arrived ({error})"))),
    };
    if !reply["ok"].is_boolean() {
        return Err(unknown("the reply has no outcome".into()));
    }
    Ok(reply)
}
