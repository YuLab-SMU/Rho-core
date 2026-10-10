//! Test-only stdio caller for evals/claude-code; no production protocol adapter.
use std::error::Error;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use rho_core::{Core, CoreConfig, Executor, Limits, RunRequest, RunView, Stream};
use serde_json::{Value, json};

const CALLER: &str = "eval-agent";

fn request(id: &str, job: &str) -> Result<RunRequest, String> {
    let code = match job {
        "sum" => {
            "printf 'sum\\n' >> dispatches.txt; seq 1 100 > numbers.txt; awk '{s+=$1} END {print s}' numbers.txt"
        }
        "loud" => {
            "printf 'loud\\n' >> dispatches.txt; i=0; while [ $i -lt 200 ]; do printf x; i=$((i+1)); done"
        }
        "hold" => {
            "exec </dev/null; printf 'hold\\n' >> dispatches.txt; printf '%s\\n' $$ > held.pid; while [ ! -f release ]; do sleep 0.05; done; printf done"
        }
        _ => return Err("unknown fixture job; choose sum, loud or hold".into()),
    };
    Ok(RunRequest {
        request_id: id.into(),
        executor: "sh".into(),
        workdir: "analysis".into(),
        code: code.into(),
        args: Vec::new(),
    })
}

fn view(v: RunView) -> Value {
    json!({"run_id":v.run_id,"terminal":v.is_terminal(),"held":v.held,
        "status":v.status,"unsaved":v.unsaved,"operation":v.operation})
}

fn text<'a>(v: &'a Value, name: &str) -> Result<&'a str, String> {
    v.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string {name}"))
}

fn call(core: &Core, v: &Value) -> Result<Value, String> {
    let id = || text(v, "request_id");
    match text(v, "op")? {
        "capability" => Ok(json!({"caller":CALLER,"jobs":["sum","loud","hold"],
            "max_stream_bytes":core.capability().limits.max_stream_bytes,
            "contract":rho_core::CONTRACT})),
        "submit" => {
            let s = core
                .submit(CALLER, request(id()?, text(v, "job")?)?)
                .map_err(|e| e.to_string())?;
            Ok(json!({"disposition":format!("{:?}",s.disposition),"run":view(s.run)}))
        }
        "lookup" => core
            .lookup(CALLER, id()?)
            .map(view)
            .map_err(|e| e.to_string()),
        "list_operations" => core
            .list_operations(CALLER)
            .map(|xs| json!(xs.into_iter().map(view).collect::<Vec<_>>()))
            .map_err(|e| e.to_string()),
        "wait" => core
            .wait(
                CALLER,
                id()?,
                Duration::from_millis(
                    v.get("timeout_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(1000)
                        .min(5000),
                ),
            )
            .map(view)
            .map_err(|e| e.to_string()),
        "cancel" => core
            .cancel(CALLER, id()?)
            .map(view)
            .map_err(|e| e.to_string()),
        "read_output" => {
            let stream = match v.get("stream").and_then(Value::as_str).unwrap_or("stdout") {
                "stdout" => Stream::Stdout,
                "stderr" => Stream::Stderr,
                _ => return Err("stream must be stdout or stderr".into()),
            };
            let c = core
                .read_output(
                    CALLER,
                    id()?,
                    stream,
                    v.get("offset").and_then(Value::as_u64).unwrap_or(0),
                    4096,
                )
                .map_err(|e| e.to_string())?;
            Ok(
                json!({"text":String::from_utf8_lossy(&c.bytes),"offset":c.offset,"info":c.info,"run_terminal":c.run_terminal}),
            )
        }
        _ => Err("unknown fixture operation".into()),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(std::env::args_os().nth(1).ok_or("fixture root required")?);
    fs::create_dir_all(root.join("project/analysis"))?;
    let config = CoreConfig::new(root.join("project"), root.join("state"))
        .executor(Executor::new("sh", "/bin/sh"))
        .limits(Limits {
            max_stream_bytes: 64,
            stop_grace: Duration::from_millis(100),
            ..Limits::default()
        });
    let mut core = Core::open(config.clone())?;
    let input = io::stdin();
    let mut output = io::stdout().lock();
    loop {
        let mut line = String::new();
        let n = input.lock().take(32 * 1024 + 1).read_line(&mut line)?;
        if n == 0 {
            break;
        }
        if n > 32 * 1024 {
            return Err("fixture request exceeds 32 KiB".into());
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(v) if v.get("op").and_then(Value::as_str) == Some("reopen") => {
                drop(core);
                core = Core::open(config.clone())?;
                json!({"ok":true,"value":"reopened"})
            }
            Ok(v) => match call(&core, &v) {
                Ok(value) => json!({"ok":true,"value":value}),
                Err(error) => json!({"ok":false,"error":error}),
            },
            Err(error) => json!({"ok":false,"error":error.to_string()}),
        };
        writeln!(output, "{reply}")?;
        output.flush()?;
    }
    core.shutdown();
    Ok(())
}
