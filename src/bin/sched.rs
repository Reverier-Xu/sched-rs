//! The `sched` daemon — a cron-style daemon built on the `sched` library.
//!
//! This binary is built with the non-default `cli` feature:
//!
//! ```text
//! cargo run --features cli --bin sched -- [--check] [path-to-schedtab]
//! ```
//!
//! Reads a schedtab file and runs each entry's command via bash at its
//! scheduled times. Logs are written to stdout as structured records; under
//! systemd, stdout is captured into the journal automatically
//! (`journalctl -u sched`). Set `SCHEDD_LOG` (or `RUST_LOG`) to control
//! verbosity, e.g. `SCHEDD_LOG=debug`.
//!
//! Usage: `sched [--check] [path-to-schedtab]`
//!
//! schedtab format (crontab-style): one entry per line, `#` starts a
//! comment, the first whitespace run outside braces separates the schedule
//! expression from the command:
//!
//! ```text
//! {*}-{*}-{*}T08:00:00[Asia/Shanghai]   echo "good morning"
//! 2026-{*}-{*}T{8, 12-14}:00:00+08:00   /usr/local/bin/backup.sh
//! ```
//!
//! Each command receives `SCHED_SCHEDULED_AT` (the nominal trigger time) in
//! its environment. The command can get its actual execution time itself
//! (e.g. with `date`), which is the other quantity a scheduled task usually
//! cares about.

#![forbid(unsafe_code)]

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use sched::{Timestamp, Trigger};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
  init_logs();

  let mut check_only = false;
  let mut path = PathBuf::from("/etc/sched/schedtab");
  for arg in std::env::args().skip(1) {
    match arg.as_str() {
      "--check" => check_only = true,
      _ if !arg.starts_with('-') => path = PathBuf::from(arg),
      _ => {
        eprintln!("unknown argument: {arg}");
        return ExitCode::FAILURE;
      }
    }
  }

  match load(&path) {
    Err(error) => {
      error!(path = %path.display(), %error, "failed to read schedtab");
      ExitCode::FAILURE
    }
    Ok((trigger, errors)) if check_only => {
      if errors > 0 {
        error!(errors, "schedtab has invalid entries");
        ExitCode::FAILURE
      } else if trigger.is_empty() {
        error!(path = %path.display(), "schedtab has no valid entries");
        ExitCode::FAILURE
      } else {
        info!(tasks = trigger.len(), path = %path.display(), "schedtab ok");
        ExitCode::SUCCESS
      }
    }
    Ok((trigger, errors)) => {
      if errors > 0 {
        warn!(errors, "skipped invalid schedtab entries");
      }
      if trigger.is_empty() {
        error!(path = %path.display(), "schedtab has no valid entries");
        return ExitCode::FAILURE;
      }
      smol::block_on(drive(trigger));
      ExitCode::SUCCESS
    }
  }
}

fn init_logs() {
  let filter = EnvFilter::try_from_env("SCHEDD_LOG")
    .or_else(|_| EnvFilter::try_from_env("RUST_LOG"))
    .unwrap_or_else(|_| EnvFilter::new("info"));
  tracing_subscriber::fmt()
    .with_env_filter(filter)
    .with_writer(std::io::stdout)
    .init();
}

/// Reads a schedtab file and registers every valid entry. Returns the
/// trigger and the number of invalid lines.
fn load(path: &Path) -> io::Result<(Trigger, usize)> {
  let contents = std::fs::read_to_string(path)?;
  let mut trigger = Trigger::new();
  let mut errors = 0;
  for (index, raw) in contents.lines().enumerate() {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    let lineno = index + 1;
    let Some((expr, command)) = split_entry(line) else {
      error!(lineno, %line, "invalid schedtab entry");
      errors += 1;
      continue;
    };
    let command = command.to_string();
    let command_for_log = command.clone();
    match trigger.register(expr, move |scheduled_at| {
      let command = command.clone();
      async move { run_command(command, scheduled_at).await }
    }) {
      Ok(id) => info!(task_id = id.index(), %expr, command = %command_for_log, "task scheduled"),
      Err(error) => {
        error!(lineno, %expr, %error, "invalid schedtab entry");
        errors += 1;
      }
    }
  }
  Ok((trigger, errors))
}

/// Splits a schedtab line into schedule expression and command. The first
/// whitespace run outside braces ends the expression.
fn split_entry(line: &str) -> Option<(&str, &str)> {
  let bytes = line.as_bytes();
  let mut in_brace = false;
  for (i, &b) in bytes.iter().enumerate() {
    match b {
      b'{' => in_brace = true,
      b'}' => in_brace = false,
      b' ' | b'\t' if !in_brace => {
        let command = line[i..].trim_start();
        return (!command.is_empty()).then(|| (line[..i].trim_end(), command));
      }
      _ => {}
    }
  }
  None
}

/// Executes a command via bash, logging its output line by line.
///
/// The nominal trigger time is exposed as `SCHED_SCHEDULED_AT`; the command
/// can get its actual execution time itself.
async fn run_command(command: String, scheduled_at: Timestamp) {
  info!(
    command = %command,
    scheduled_at = %scheduled_at,
    "command started"
  );
  let result = async_process::Command::new("bash")
    .arg("-c")
    .arg(&command)
    .stdin(std::process::Stdio::null())
    .env("SCHED_SCHEDULED_AT", scheduled_at.to_string())
    .output()
    .await;
  match result {
    Ok(output) => {
      for line in String::from_utf8_lossy(&output.stdout).lines() {
        info!(command = %command, "{line}");
      }
      for line in String::from_utf8_lossy(&output.stderr).lines() {
        warn!(command = %command, "{line}");
      }
      if !output.status.success() {
        warn!(command = %command, status = ?output.status.code(), "command exited unsuccessfully");
      }
    }
    Err(error) => warn!(command = %command, %error, "failed to spawn command"),
  }
}

/// The drive loop: sleep until the soonest trigger time, then spawn every
/// due run onto the smol executor.
///
/// The dispatch start time is captured exactly once per round and passed to
/// `fire_due`, so every callback and command sees the same instant.
async fn drive(mut trigger: Trigger) {
  info!(tasks = trigger.len(), "sched started");
  let mut running: Vec<smol::Task<()>> = Vec::new();
  loop {
    let decision_time = Timestamp::now();
    let Some(scheduled_at) = trigger.next(decision_time) else {
      // No future ticks: let in-flight commands finish before exiting.
      for task in running {
        task.await;
      }
      info!("no task will fire again; exiting");
      return;
    };
    if scheduled_at > decision_time {
      smol::Timer::after(duration_between(decision_time, scheduled_at)).await;
    }
    let started_at = Timestamp::now();
    for run in trigger.fire_due(scheduled_at, started_at) {
      running.push(smol::spawn(run));
    }
    // Reap completed runs so the handle list does not grow forever.
    running.retain(|task| !task.is_finished());
  }
}

fn duration_between(now: Timestamp, at: Timestamp) -> Duration {
  let nanos = (at.as_second() - now.as_second()) as i128 * 1_000_000_000
    + (at.subsec_nanosecond() - now.subsec_nanosecond()) as i128;
  if nanos <= 0 {
    Duration::ZERO
  } else {
    Duration::from_nanos(nanos.min(u64::MAX as i128) as u64)
  }
}

#[cfg(test)]
mod tests {
  use super::split_entry;

  #[test]
  fn split_entry_basics() {
    assert_eq!(
      split_entry("{*}-{*}-{*}T08:00:00[Asia/Shanghai]   echo hi"),
      Some(("{*}-{*}-{*}T08:00:00[Asia/Shanghai]", "echo hi"))
    );
    // Whitespace inside braces doesn't end the expression.
    assert_eq!(
      split_entry("2026-{*}-{*}T{8, 12-14}:00:00  echo hi"),
      Some(("2026-{*}-{*}T{8, 12-14}:00:00", "echo hi"))
    );
    // Missing command.
    assert_eq!(split_entry("{*}-{*}-{*}T08:00:00"), None);
    assert_eq!(split_entry("{*}-{*}-{*}T08:00:00  "), None);
  }
}
