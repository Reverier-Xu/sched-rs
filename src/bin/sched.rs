//! The `sched` daemon — a cron-style daemon built on the `sched` library.
//!
//! This binary is built with the non-default `cli` feature:
//!
//! ```text
//! cargo run --features cli --bin sched -- [--check] [path-to-schedtab]
//! ```
//!
//! Reads a schedtab file and runs each entry's command via the platform
//! shell (`bash -c` on Unix, `cmd /C` on Windows) at its scheduled times.
//! Logs are written to stdout as structured records; under systemd, stdout is
//! captured into the journal automatically (`journalctl -u sched`). Set
//! `SCHEDD_LOG` (or `RUST_LOG`) to control verbosity, e.g.
//! `SCHEDD_LOG=debug`.
//!
//! On SIGINT, SIGTERM or SIGHUP (Ctrl-C / console close on Windows) the
//! daemon stops scheduling new commands and waits for in-flight commands to
//! finish before exiting.
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
//!
//! On Unix, the schedtab must not be writable by group or others; otherwise
//! an unprivileged user could inject commands into a daemon running as root.

#![forbid(unsafe_code)]

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use sched::{Timestamp, Trigger};
use smol::channel::{Receiver, bounded};
use smol::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

/// The platform shell used to run schedtab commands.
#[cfg(windows)]
const SHELL: (&str, &str) = ("cmd", "/C");
#[cfg(not(windows))]
const SHELL: (&str, &str) = ("bash", "-c");

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
  // Open first and check the opened handle, so replacing the path between
  // the permission check and the read cannot bypass it.
  let mut file = std::fs::File::open(path)?;
  check_permissions(&file)?;
  let mut contents = String::new();
  file.read_to_string(&mut contents)?;
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

/// Rejects a schedtab that group or other users can modify. A daemon running
/// as root would otherwise execute commands injected by an unprivileged user.
#[cfg(unix)]
fn check_permissions(file: &std::fs::File) -> io::Result<()> {
  use std::os::unix::fs::PermissionsExt;

  let mode = file.metadata()?.permissions().mode();
  if mode & 0o022 != 0 {
    return Err(io::Error::new(
      io::ErrorKind::PermissionDenied,
      format!(
        "schedtab is writable by group or others (mode {:03o}); run `chmod go-w`",
        mode & 0o777
      ),
    ));
  }
  Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_file: &std::fs::File) -> io::Result<()> {
  Ok(())
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

/// Executes a command via the platform shell, logging its output line by
/// line as it arrives.
///
/// The nominal trigger time is exposed as `SCHED_SCHEDULED_AT`; the command
/// can get its actual execution time itself.
async fn run_command(command: String, scheduled_at: Timestamp) {
  info!(
    command = %command,
    scheduled_at = %scheduled_at,
    "command started"
  );
  let child = async_process::Command::new(SHELL.0)
    .arg(SHELL.1)
    .arg(&command)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .env("SCHED_SCHEDULED_AT", scheduled_at.to_string())
    .spawn();
  let mut child = match child {
    Ok(child) => child,
    Err(error) => {
      warn!(command = %command, %error, "failed to spawn command");
      return;
    }
  };
  let stdout = child.stdout.take().expect("stdout is piped");
  let stderr = child.stderr.take().expect("stderr is piped");
  // Read both pipes concurrently: waiting for the child before draining them
  // would deadlock once a pipe buffer fills up.
  let ((), ()) = smol::future::zip(
    log_lines(stdout, false, &command),
    log_lines(stderr, true, &command),
  )
  .await;
  match child.status().await {
    Ok(status) if !status.success() => {
      warn!(command = %command, status = ?status.code(), "command exited unsuccessfully");
    }
    Ok(_) => {}
    Err(error) => warn!(command = %command, %error, "failed to wait for command"),
  }
}

/// Logs one command output stream line by line without buffering the whole
/// output. Invalid UTF-8 is replaced lossily.
async fn log_lines<R>(reader: R, stderr: bool, command: &str)
where
  R: AsyncRead + Unpin,
{
  let mut reader = BufReader::new(reader);
  let mut buf = Vec::new();
  loop {
    buf.clear();
    match reader.read_until(b'\n', &mut buf).await {
      Ok(0) => break,
      Ok(_) => {
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\r', '\n']);
        if stderr {
          warn!(command = %command, "{line}");
        } else {
          info!(command = %command, "{line}");
        }
      }
      Err(error) => {
        warn!(command = %command, %error, "failed to read command output");
        break;
      }
    }
  }
}

/// Why the scheduler woke up from its sleep.
enum Wake {
  Tick,
  Shutdown,
}

/// The drive loop: sleep until the soonest trigger time, then spawn every
/// due run onto the smol executor.
///
/// The dispatch start time is captured exactly once per round and passed to
/// `fire_due`, so every callback and command sees the same instant. On a
/// shutdown signal, stop scheduling and wait for in-flight commands.
async fn drive(mut trigger: Trigger) {
  info!(tasks = trigger.len(), "sched started");
  let shutdown = shutdown_receiver();
  let mut running: Vec<smol::Task<()>> = Vec::new();
  loop {
    let decision_time = Timestamp::now();
    let Some(scheduled_at) = trigger.next(decision_time) else {
      // No future ticks: let in-flight commands finish before exiting.
      wait_for_running(running).await;
      info!("no task will fire again; exiting");
      return;
    };
    if scheduled_at > decision_time {
      let wake = smol::future::or(
        async {
          smol::Timer::after(duration_between(decision_time, scheduled_at)).await;
          Wake::Tick
        },
        async {
          match shutdown.recv().await {
            Ok(()) => Wake::Shutdown,
            Err(_) => core::future::pending::<Wake>().await,
          }
        },
      )
      .await;
      if matches!(wake, Wake::Shutdown) {
        info!(
          running = running.len(),
          "shutdown requested; waiting for running commands"
        );
        wait_for_running(running).await;
        return;
      }
    }
    let started_at = Timestamp::now();
    for run in trigger.fire_due(scheduled_at, started_at) {
      running.push(smol::spawn(run));
    }
    // Reap completed runs so the handle list does not grow forever.
    running.retain(|task| !task.is_finished());
  }
}

/// Installs a Ctrl-C / SIGTERM handler and returns the channel it signals.
///
/// If the handler cannot be installed, the sender is dropped and `recv`
/// returns an error; `drive` then waits only on the timer.
fn shutdown_receiver() -> Receiver<()> {
  let (tx, rx) = bounded(1);
  match ctrlc::set_handler(move || {
    let _ = tx.try_send(());
  }) {
    Ok(()) => {}
    Err(error) => warn!(%error, "failed to install shutdown signal handler"),
  }
  rx
}

/// Awaits every in-flight command. The service manager can still SIGKILL the
/// daemon if a command hangs past its stop timeout.
async fn wait_for_running(running: Vec<smol::Task<()>>) {
  for task in running {
    task.await;
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

  #[cfg(unix)]
  #[test]
  fn writable_schedtab_is_rejected() {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::temp_dir().join(format!(
      "sched-permissions-{}-{}.schedtab",
      std::process::id(),
      line!()
    ));
    std::fs::write(&path, "").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    assert!(super::check_permissions(&file).is_ok());

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(super::check_permissions(&file).is_err());

    std::fs::remove_file(&path).unwrap();
  }
}
