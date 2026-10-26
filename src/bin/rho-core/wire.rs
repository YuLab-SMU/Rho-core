//! The line exchange between this binary's client and host. It is internal to
//! one build of `rho-core`; callers use the CLI, not this format.
//!
//! One request per connection: the host first sends a greeting line, the
//! client then sends one request line, and the host answers with one reply
//! line. A client that has not received a `ready` greeting has sent nothing.

use std::io::{self, BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use rho_core::{ActivityEvent, ActivitySource, Limits};
use serde::{Deserialize, Serialize};

pub const PROTOCOL: u64 = 1;
/// The socket's name inside the host's state directory.
pub const SOCKET_NAME: &str = "host.sock";
/// Bound on each greeting, request and reply transfer, and on an idle client.
pub const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest wait one request may ask for.
pub const MAX_WAIT: Duration = Duration::from_secs(600);
pub const GREETING_MAX_BYTES: usize = 4 << 10;
/// Room for identities, the workdir, field names and JSON punctuation.
const REQUEST_OVERHEAD: usize = 64 << 10;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Greeting {
    Ready {
        protocol: u64,
        max_request_bytes: usize,
    },
    /// Every connection slot is in use; the request was not read.
    Busy {
        protocol: u64,
        max_connections: usize,
    },
    /// The peer is not this host's user; the request was not read.
    Refused { protocol: u64, reason: String },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Capability {
        #[serde(default)]
        contract: bool,
    },
    ActivityOpen {
        caller: String,
        source: ActivitySource,
    },
    ActivityAppend {
        caller: String,
        stream: String,
        events: Vec<ActivityEvent>,
    },
    ActivityRead {
        caller: String,
        stream: String,
        after: u64,
        max_events: usize,
        max_bytes: usize,
    },
    ActivityList {
        caller: String,
    },
    ActivityForget {
        caller: String,
        stream: String,
        expected_cursor: u64,
    },
    Submit {
        caller: String,
        request_id: String,
        executor: String,
        workdir: String,
        code: String,
        args: Vec<String>,
    },
    Lookup {
        caller: String,
        request_id: String,
        #[serde(default)]
        record: bool,
    },
    List {
        caller: String,
    },
    Wait {
        caller: String,
        request_id: String,
        timeout_ms: u64,
    },
    Cancel {
        caller: String,
        request_id: String,
    },
    Read {
        caller: String,
        request_id: String,
        stream: WireStream,
        offset: u64,
        max_bytes: usize,
    },
    Forget {
        caller: String,
        request_id: String,
    },
}

/// Largest request line, newline included, that can carry a request within
/// `limits`: JSON escaping takes up to six bytes per byte of code or argument.
pub fn max_request_bytes(limits: &Limits) -> usize {
    limits
        .max_code_bytes
        .saturating_add(limits.max_arg_bytes)
        .saturating_mul(6)
        .saturating_add(limits.max_args.saturating_mul(3))
        .saturating_add(REQUEST_OVERHEAD)
        .max(
            limits
                .activity
                .max_batch_bytes
                .saturating_add(REQUEST_OVERHEAD),
        )
}

pub enum Line {
    Complete(Vec<u8>),
    /// `max_bytes` arrived without a newline.
    TooLarge,
    /// The peer closed the connection before a newline.
    Closed,
}

/// Reads one line of at most `max_bytes`, newline included. Reads no more than
/// that from `reader`, so a peer that sent exactly `max_bytes` sees the reply.
pub fn read_line(reader: &mut impl BufRead, max_bytes: usize) -> io::Result<Line> {
    let mut line = Vec::new();
    Read::take(reader, max_bytes as u64).read_until(b'\n', &mut line)?;
    Ok(if line.last() == Some(&b'\n') {
        line.pop();
        Line::Complete(line)
    } else if line.len() >= max_bytes {
        Line::TooLarge
    } else {
        Line::Closed
    })
}

pub fn send(mut stream: &UnixStream, message: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    stream.flush()
}
