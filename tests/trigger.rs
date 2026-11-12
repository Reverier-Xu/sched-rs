//! Trigger computation and task execution tests.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use sched::{
  BindError, DateTime, RegisterError, Schedule, Timestamp, Trigger, TriggerError, parse,
};

fn init_logs() {
  let _ = tracing_subscriber::fmt()
    .with_test_writer()
    .with_max_level(tracing::Level::TRACE)
    .try_init();
}

fn dt(input: &str) -> DateTime {
  input.parse().unwrap()
}

fn ts(input: &str) -> Timestamp {
  input.parse().unwrap()
}

fn noop_waker() -> Waker {
  unsafe fn clone(_: *const ()) -> RawWaker {
    RawWaker::new(std::ptr::null(), &VTABLE)
  }
  unsafe fn noop(_: *const ()) {}
  static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
  unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

fn poll_once(future: Pin<&mut impl Future<Output = ()>>) -> Poll<()> {
  future.poll(&mut Context::from_waker(&noop_waker()))
}

// ---------------------------------------------------------------------------
// Civil search (no time zone)
// ---------------------------------------------------------------------------

#[test]
fn civil_search_basics() {
  let s = parse("2026-{*}-{*}T{8,12-14}:00:00").unwrap();
  assert_eq!(
    s.next_after(dt("2026-01-01T09:00:00")).unwrap(),
    Some(dt("2026-01-01T12:00:00"))
  );
  // Wraps to the next day.
  assert_eq!(
    s.next_after(dt("2026-01-01T14:00:00")).unwrap(),
    Some(dt("2026-01-02T08:00:00"))
  );
  // Strictly after: an exact match doesn't count.
  assert_eq!(
    s.next_after(dt("2026-01-01T08:00:00")).unwrap(),
    Some(dt("2026-01-01T12:00:00"))
  );
  // Sub-second precision is floored, so 09:00:00.5 still catches 12:00.
  assert_eq!(
    s.next_after(dt("2026-01-01T11:59:59.999")).unwrap(),
    Some(dt("2026-01-01T12:00:00"))
  );
}

#[test]
fn civil_search_every_second() {
  let s = parse("{*}-{*}-{*}T{*}:{*}:{*}").unwrap();
  assert_eq!(
    s.next_after(dt("2026-12-31T23:59:59")).unwrap(),
    Some(dt("2027-01-01T00:00:00"))
  );
}

#[test]
fn civil_search_exhaustion() {
  // Year 2026 only: after the last second of 2026 there is nothing left.
  let s = parse("2026-{*}-{*}T00:00:00").unwrap();
  assert_eq!(s.next_after(dt("2026-12-31T00:00:00")).unwrap(), None);

  // February 30th never exists.
  let s = parse("{*}-02-30T00:00:00").unwrap();
  assert_eq!(s.next_after(dt("2020-01-01T00:00:00")).unwrap(), None);

  // An empty set (inverted range) never matches.
  let s = parse("{*}-{*}-{*}T{14-8}:00:00").unwrap();
  assert_eq!(s.next_after(dt("2020-01-01T00:00:00")).unwrap(), None);

  // Leap day fires only in leap years.
  let s = parse("{*}-02-29T12:00:00").unwrap();
  assert_eq!(
    s.next_after(dt("2027-01-01T00:00:00")).unwrap(),
    Some(dt("2028-02-29T12:00:00"))
  );
}

#[test]
fn civil_search_rejects_zoned_schedules() {
  let s = parse("{*}-{*}-{*}T08:00:00+08:00").unwrap();
  assert_eq!(
    s.next_after(dt("2026-01-01T00:00:00")),
    Err(TriggerError::TimeZoneDeclared)
  );
}

// ---------------------------------------------------------------------------
// Bound schedules (time zone aware)
// ---------------------------------------------------------------------------

#[test]
fn bound_fixed_offset() {
  let s: Schedule = "{*}-{*}-{*}T08:00:00+08:00".parse().unwrap();
  let bound = s.bind().unwrap();
  let next = bound.next_after(ts("2025-12-31T23:00:00Z")).unwrap();
  assert_eq!(next.timestamp().to_string(), "2026-01-01T00:00:00Z");
  // Strictly after: the candidate equal to `after` doesn't count.
  let next = bound.next_after(ts("2026-01-01T00:00:00Z")).unwrap();
  assert_eq!(next.timestamp().to_string(), "2026-01-02T00:00:00Z");
}

#[test]
fn bound_iana_dst_gap_is_skipped() {
  if jiff::tz::TimeZone::get("America/New_York").is_err() {
    eprintln!("skipping: no tzdb available");
    return;
  }
  // 2026-03-08 02:30 doesn't exist in New York (spring forward).
  // 2027's DST transition is on 03-14, so 2027-03-08 02:30 exists (EST).
  let s: Schedule = "{*}-03-08T02:30:00[America/New_York]".parse().unwrap();
  let next = s
    .bind()
    .unwrap()
    .next_after(ts("2026-03-01T00:00:00Z"))
    .unwrap();
  assert_eq!(next.timestamp().to_string(), "2027-03-08T07:30:00Z");
  assert_eq!(
    next.to_string(),
    "2027-03-08T02:30:00-05:00[America/New_York]"
  );
}

#[test]
fn bound_iana_dst_fold_fires_once() {
  if jiff::tz::TimeZone::get("America/New_York").is_err() {
    eprintln!("skipping: no tzdb available");
    return;
  }
  // 2026-11-01 01:30 happens twice in New York (fall back); the first
  // occurrence (EDT, -04:00) wins.
  let s: Schedule = "{*}-11-01T01:30:00[America/New_York]".parse().unwrap();
  let next = s
    .bind()
    .unwrap()
    .next_after(ts("2026-10-01T00:00:00Z"))
    .unwrap();
  assert_eq!(
    next.to_string(),
    "2026-11-01T01:30:00-04:00[America/New_York]"
  );
}

#[test]
fn bind_errors() {
  let s: Schedule = "{*}-{*}-{*}T08:00:00[Not/AZone]".parse().unwrap();
  assert!(matches!(s.bind(), Err(BindError::UnknownTimeZone { .. })));

  let s: Schedule = "{*}-{*}-{*}T08:00:00+99:59".parse().unwrap();
  assert!(matches!(s.bind(), Err(BindError::OffsetOutOfRange { .. })));
}

// ---------------------------------------------------------------------------
// Trigger
// ---------------------------------------------------------------------------

#[test]
fn trigger_register_errors() {
  init_logs();
  let mut trigger = Trigger::new();
  let err = trigger.register("not a schedule", || async {}).unwrap_err();
  assert!(matches!(err, RegisterError::Parse(_)));
  let err = trigger
    .register("{*}-{*}-{*}T00:00:00[Not/AZone]", || async {})
    .unwrap_err();
  assert!(matches!(err, RegisterError::Bind(_)));
}

#[test]
fn trigger_fires_due_tasks_once_per_tick() {
  init_logs();
  let counter = Arc::new(AtomicUsize::new(0));
  let mut trigger = Trigger::new();
  let counter2 = Arc::clone(&counter);
  trigger
    .register("{*}-{*}-{*}T{*}:{*}:{*}Z", move || {
      let counter = Arc::clone(&counter2);
      async move {
        counter.fetch_add(1, Ordering::Relaxed);
      }
    })
    .unwrap();

  let t0 = ts("2026-01-01T00:00:00Z");
  // First next() establishes the baseline: next tick is t0 + 1s.
  assert_eq!(trigger.next(t0), Some(ts("2026-01-01T00:00:01Z")));
  // Not yet due.
  assert!(trigger.fire_due(t0).is_empty());
  // Due: the run is produced, and the async work happens when polled.
  let mut runs = trigger.fire_due(ts("2026-01-01T00:00:01Z"));
  assert_eq!(runs.len(), 1);
  assert_eq!(counter.load(Ordering::Relaxed), 0);
  assert!(poll_once(Pin::new(&mut runs[0])).is_ready());
  assert_eq!(counter.load(Ordering::Relaxed), 1);

  // Missed ticks fire once, with no catch-up burst.
  let runs = trigger.fire_due(ts("2026-01-01T00:01:30Z"));
  assert_eq!(runs.len(), 1);
}

#[test]
fn trigger_skips_overlapping_runs() {
  init_logs();
  let mut trigger = Trigger::new();
  // The task runs forever once started.
  trigger
    .register("{*}-{*}-{*}T{*}:{*}:{*}Z", core::future::pending)
    .unwrap();

  let t0 = ts("2026-01-01T00:00:00Z");
  trigger.next(t0);
  let runs = trigger.fire_due(ts("2026-01-01T00:00:01Z"));
  assert_eq!(runs.len(), 1);
  std::mem::forget(runs.into_iter().next()); // keep the run alive (leak it)

  // The next tick fires while the previous run is still in flight: skipped.
  let runs = trigger.fire_due(ts("2026-01-01T00:00:02Z"));
  assert!(runs.is_empty(), "overlapping run must be skipped");
}

#[test]
fn trigger_dropped_run_frees_the_slot() {
  init_logs();
  let mut trigger = Trigger::new();
  trigger
    .register("{*}-{*}-{*}T{*}:{*}:{*}Z", core::future::pending)
    .unwrap();

  let t0 = ts("2026-01-01T00:00:00Z");
  trigger.next(t0);
  let runs = trigger.fire_due(ts("2026-01-01T00:00:01Z"));
  assert_eq!(runs.len(), 1);
  drop(runs); // dropped without being polled: the in-flight flag is cleared

  let runs = trigger.fire_due(ts("2026-01-01T00:00:02Z"));
  assert_eq!(runs.len(), 1);
}

#[test]
fn trigger_deregister() {
  init_logs();
  let mut trigger = Trigger::new();
  let id = trigger
    .register("{*}-{*}-{*}T{*}:{*}:{*}Z", || async {})
    .unwrap();
  assert_eq!(trigger.len(), 1);
  assert!(trigger.deregister(id));
  assert!(!trigger.deregister(id));
  assert!(trigger.is_empty());
  assert_eq!(trigger.next(ts("2026-01-01T00:00:00Z")), None);
}

#[test]
fn trigger_next_picks_the_soonest() {
  init_logs();
  let mut trigger = Trigger::new();
  trigger
    .register("{*}-{*}-{*}T{*}:0:0Z", || async {})
    .unwrap();
  trigger
    .register("{*}-{*}-{*}T{*}:{*}:{30}Z", || async {})
    .unwrap();
  assert_eq!(
    trigger.next(ts("2026-01-01T00:00:00Z")),
    Some(ts("2026-01-01T00:00:30Z"))
  );
}
