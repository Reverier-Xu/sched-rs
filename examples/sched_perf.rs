//! Scheduling performance harness.
//!
//! Registers N tasks that all fire once per second, then prints the nominal
//! trigger time and the actual execution time for a sample of runs, followed
//! by aggregate dispatch-delay statistics.
//!
//! Run with:
//!
//! ```text
//! cargo run --release --features cli --example sched_perf -- <tasks> <seconds>
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

use sched::{Timestamp, Trigger};

fn main() {
  let mut args = std::env::args().skip(1);
  let tasks: usize = args
    .next()
    .and_then(|arg| arg.parse().ok())
    .unwrap_or(1_000);
  let seconds: usize = args.next().and_then(|arg| arg.parse().ok()).unwrap_or(5);

  let capacity = tasks.saturating_mul(seconds.saturating_add(2));
  let delays: Arc<Vec<AtomicI64>> = Arc::new((0..capacity).map(|_| AtomicI64::new(0)).collect());
  let count = Arc::new(AtomicUsize::new(0));
  let print_all = tasks <= 20;

  let mut trigger = Trigger::new();
  for task_index in 0..tasks {
    let delays = Arc::clone(&delays);
    let count = Arc::clone(&count);
    trigger
      .register("{*}-{*}-{*}T{*}:{*}:{*}Z", move |scheduled_at| {
        let delays = Arc::clone(&delays);
        let count = Arc::clone(&count);
        async move {
          let actual = Timestamp::now();
          let delay_ns = actual.as_nanosecond() - scheduled_at.as_nanosecond();
          let slot = count.fetch_add(1, Ordering::Relaxed);
          if let Some(slot) = delays.get(slot) {
            slot.store(delay_ns as i64, Ordering::Relaxed);
          }
          // Print every run for small task counts; otherwise print a sample
          // so the scheduled/actual pair is visible without flooding the
          // terminal.
          if print_all || task_index < 5 {
            println!(
              "task={task_index:05} scheduled={scheduled_at} actual={actual} delay={:.3}ms",
              delay_ns as f64 / 1_000_000.0
            );
          }
        }
      })
      .expect("schedule must be valid");

    // Give the scheduler a chance to register everything before the first
    // tick. This is not part of the measured dispatch path.
    if task_index % 10_000 == 9_999 {
      std::thread::yield_now();
    }
  }

  let start = Timestamp::now();
  let deadline = start.as_nanosecond() + seconds as i128 * 1_000_000_000;

  smol::block_on(async {
    let mut running = Vec::new();
    loop {
      let decision_time = Timestamp::now();
      let Some(scheduled_at) = trigger.next(decision_time) else {
        break;
      };
      if scheduled_at.as_nanosecond() >= deadline {
        break;
      }
      if scheduled_at > decision_time {
        smol::Timer::after(duration_between(decision_time, scheduled_at)).await;
      }
      let started_at = Timestamp::now();
      let runs = trigger.fire_due(scheduled_at, started_at);
      println!(
        "dispatch scheduled={scheduled_at} started={started_at} runs={}",
        runs.len()
      );
      for run in runs {
        running.push(smol::spawn(run));
      }
      running.retain(|task| !task.is_finished());
    }
    for task in running {
      task.await;
    }
  });

  let runs = count.load(Ordering::Relaxed).min(capacity);
  let mut values: Vec<i64> = delays
    .iter()
    .take(runs)
    .map(|delay| delay.load(Ordering::Relaxed))
    .collect();
  values.sort_unstable();

  let expected = tasks.saturating_mul(seconds);
  let min = values.first().copied().unwrap_or(0);
  let max = values.last().copied().unwrap_or(0);
  let avg = if values.is_empty() {
    0.0
  } else {
    values.iter().map(|&value| value as f64).sum::<f64>() / values.len() as f64
  };
  let p50 = percentile(&values, 50);
  let p90 = percentile(&values, 90);
  let p99 = percentile(&values, 99);

  println!();
  println!("tasks={tasks} seconds={seconds} runs={runs} expected={expected}");
  println!(
    "delay: min={:.3}ms p50={:.3}ms p90={:.3}ms p99={:.3}ms max={:.3}ms avg={:.3}ms",
    min as f64 / 1_000_000.0,
    p50 as f64 / 1_000_000.0,
    p90 as f64 / 1_000_000.0,
    p99 as f64 / 1_000_000.0,
    max as f64 / 1_000_000.0,
    avg / 1_000_000.0,
  );
}

fn percentile(values: &[i64], percent: usize) -> i64 {
  if values.is_empty() {
    return 0;
  }
  let index = (values.len() - 1) * percent / 100;
  values[index]
}

fn duration_between(now: Timestamp, at: Timestamp) -> Duration {
  let nanos = at.as_nanosecond() - now.as_nanosecond();
  if nanos <= 0 {
    Duration::ZERO
  } else {
    Duration::from_nanos(nanos.min(u64::MAX as i128) as u64)
  }
}
