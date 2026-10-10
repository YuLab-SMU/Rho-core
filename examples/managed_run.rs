//! A plain program using Core's programmatic boundary in a temporary project.
//!
//! Run with `cargo run --example managed_run`. It submits a task, finds it again
//! as a second waiter, reads the retained output and the real product file, and
//! shows a duplicate, a conflicting request and a rejected object.

use std::error::Error;
use std::fs;
use std::time::Duration;

use rho_core::{Core, CoreConfig, Executor, RunRequest, RunStatus, Stream};

const CODE: &str = r#"n="$1"; i=1; : > numbers.txt
while [ "$i" -le "$n" ]; do echo "$i" >> numbers.txt; i=$((i + 1)); done
awk '{ s += $1 } END { print "sum=" s }' numbers.txt
"#;

fn main() -> Result<(), Box<dyn Error>> {
    let tmp = tempfile::tempdir()?;
    let project = tmp.path().join("project");
    fs::create_dir_all(project.join("analysis"))?;
    let core = Core::open(
        CoreConfig::new(&project, tmp.path().join("state"))
            .executor(Executor::new("sh", "/bin/sh")),
    )?;
    let capability = core.capability();
    println!(
        "executors: {:?}",
        capability
            .executors
            .iter()
            .map(|e| &e.name)
            .collect::<Vec<_>>()
    );
    println!("limits: {:?}", capability.limits);

    let request = RunRequest {
        request_id: "sum-100".into(),
        executor: "sh".into(),
        workdir: "analysis".into(),
        code: CODE.into(),
        args: vec!["100".into()],
    };
    let submitted = core.submit("agent-a", request.clone())?;
    println!(
        "submit: {:?} run {}",
        submitted.disposition, submitted.run.run_id
    );

    // Any waiter holding the identity can find the run; this one waits until it ends.
    let view = core.wait("agent-a", "sum-100", Duration::from_secs(30))?;
    match &view.status {
        RunStatus::Finished {
            pid,
            exit,
            group_released,
            ..
        } => {
            println!("finished: pid {pid}, exit {exit:?}, process group released {group_released}");
        }
        other => println!("not finished yet: {other:?}"),
    }
    let output = core.read_output("agent-a", "sum-100", Stream::Stdout, 0, 4096)?;
    print!(
        "stdout ({} of {} bytes retained): {}",
        output.bytes.len(),
        output.info.observed_bytes,
        String::from_utf8_lossy(&output.bytes)
    );
    let numbers = fs::read_to_string(project.join("analysis/numbers.txt"))?;
    println!("product numbers.txt: {} lines", numbers.lines().count());
    println!(
        "original request: {:?} code sha256 {}",
        view.request.args, view.request.code_sha256
    );

    let duplicate = core.submit("agent-a", request.clone())?;
    println!(
        "same identity again: {:?} run {}",
        duplicate.disposition, duplicate.run.run_id
    );

    let changed = RunRequest {
        args: vec!["200".into()],
        ..request.clone()
    };
    if let Err(error) = core.submit("agent-a", changed) {
        println!("rejected: {error}");
    }
    let outside = RunRequest {
        request_id: "escape".into(),
        workdir: "../".into(),
        ..request
    };
    if let Err(error) = core.submit("agent-a", outside) {
        println!("rejected: {error}");
    }
    if let Err(error) = core.lookup("agent-b", "sum-100") {
        println!("other caller: {error}");
    }
    Ok(())
}
