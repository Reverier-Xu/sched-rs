//! Trigger computation and task execution tests.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

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

fn poll_once(future: Pin<&mut impl Future<Output = ()>>) -> Poll<()> {
  future.poll(&mut Context::from_waker(Waker::noop()))
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
fn bound_iana_dst_fold_fires_once_and_skips_second_pass() {
  if jiff::tz::TimeZone::get("America/New_York").is_err() {
    eprintln!("skipping: no tzdb available");
    return;
  }
  // 2026-11-01 01:30 happens twice in New York (fall back); the first
  // occurrence (EDT, -04:00) wins.
  let bound = "{*}-{*}-{*}T01:30:00[America/New_York]"
    .parse::<Schedule>()
    .unwrap()
    .bind()
    .unwrap();
  let next = bound.next_after(ts("2026-11-01T00:00:00Z")).unwrap();
  assert_eq!(
    next.to_string(),
    "2026-11-01T01:30:00-04:00[America/New_York]"
  );

  // Starting inside the second pass must not return the first occurrence in
  // the past. The fold's second pass is skipped entirely.
  let after = ts("2026-11-01T06:15:00Z");
  let next = bound.next_after(after).unwrap();
  assert!(next.timestamp() > after);
  assert_eq!(
    next.to_string(),
    "2026-11-02T01:30:00-05:00[America/New_York]"
  );
}

#[test]
fn bound_next_after_is_strictly_after_across_dst() {
  if jiff::tz::TimeZone::get("America/New_York").is_err() {
    eprintln!("skipping: no tzdb available");
    return;
  }
  let cases = [
    (
      "{*}-{*}-{*}T01:30:00[America/New_York]",
      "2026-11-01T04:00:00Z",
    ),
    (
      "{*}-{*}-{*}T01:30:00[America/New_York]",
      "2026-11-01T06:15:00Z",
    ),
    (
      "{*}-{*}-{*}T01:30:00[America/New_York]",
      "2026-11-01T07:00:00Z",
    ),
    (
      "{*}-{*}-{*}T02:30:00[America/New_York]",
      "2026-03-08T06:00:00Z",
    ),
    (
      "{*}-{*}-{*}T02:30:00[America/New_York]",
      "2026-03-08T07:30:00Z",
    ),
  ];
  for (expr, after) in cases {
    let bound = expr.parse::<Schedule>().unwrap().bind().unwrap();
    let after = ts(after);
    let next = bound.next_after(after).unwrap();
    assert!(
      next.timestamp() > after,
      "{expr}: next {next} must be strictly after {after}"
    );
  }
}

#[test]
fn bind_errors() {
  let s: Schedule = "{*}-{*}-{*}T08:00:00[Not/AZone]".parse().unwrap();
  assert!(matches!(s.bind(), Err(BindError::UnknownTimeZone { .. })));

  let s: Schedule = "{*}-{*}-{*}T08:00:00+25:59".parse().unwrap();
  assert!(s.bind().is_ok());
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

  // Missed ticks fire once, with no catch-up burst. Polling `next` first must
  // not swallow the overdue tick.
  let late = ts("2026-01-01T00:01:30Z");
  assert_eq!(trigger.next(late), Some(ts("2026-01-01T00:00:02Z")));
  let runs = trigger.fire_due(late);
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

// ---------------------------------------------------------------------------
// Regression: field-carry must reset every smaller field
// ---------------------------------------------------------------------------

/// Steps a bound schedule `n` times and renders each hit as
/// `YYYY-MM-DDTHH:MM:SS`, so a whole sequence can be asserted at once.
/// Single-point assertions miss carry bugs — see `civil_sequence_*` below.
fn sequence(expr: &str, from: &str, n: usize) -> Vec<String> {
  let bound = parse(expr).unwrap().bind().unwrap();
  let mut at = ts(from);
  let mut out = Vec::with_capacity(n);
  for _ in 0..n {
    let hit = bound.next_after(at).expect("schedule exhausted");
    out.push(hit.strftime("%Y-%m-%dT%H:%M:%S").to_string());
    at = hit.timestamp();
  }
  out
}

#[test]
fn civil_sequence_carry_resets_smaller_fields() {
  // `bump_minute` / `bump_hour` used to leave the smaller fields at their
  // previous values, so any schedule with a wildcard hour or minute jumped
  // to midnight of the following day instead of the next tick.
  assert_eq!(
    sequence("{*}-{*}-{*}T{*}:{*}:00Z", "2026-01-01T00:01:00Z", 3),
    [
      "2026-01-01T00:02:00",
      "2026-01-01T00:03:00",
      "2026-01-01T00:04:00",
    ]
  );
  assert_eq!(
    sequence("{*}-{*}-{*}T{*}:{00,30}:00Z", "2026-01-01T00:00:00Z", 4),
    [
      "2026-01-01T00:30:00",
      "2026-01-01T01:00:00",
      "2026-01-01T01:30:00",
      "2026-01-01T02:00:00",
    ]
  );
  assert_eq!(
    sequence("{*}-{*}-{*}T{*}:{*}:30Z", "2026-01-01T00:00:00Z", 3),
    [
      "2026-01-01T00:00:30",
      "2026-01-01T00:01:30",
      "2026-01-01T00:02:30",
    ]
  );
  // The cron-equivalent hourly schedule (`0 * * * *`).
  assert_eq!(
    sequence("{*}-{*}-{*}T{*}:00:00Z", "2026-01-01T00:00:00Z", 3),
    [
      "2026-01-01T01:00:00",
      "2026-01-01T02:00:00",
      "2026-01-01T03:00:00",
    ]
  );
}

#[test]
fn civil_sequence_rolls_over_day_and_year() {
  // Minute carry crossing midnight must reset to 00:00:00 of the next day.
  assert_eq!(
    sequence("{*}-{*}-{*}T{*}:{00,30}:00Z", "2026-01-01T23:30:00Z", 2),
    ["2026-01-02T00:00:00", "2026-01-02T00:30:00"]
  );
  // Hour carry crossing midnight.
  assert_eq!(
    sequence("{*}-{*}-{*}T{*}:00:00Z", "2026-12-31T23:00:00Z", 2),
    ["2027-01-01T00:00:00", "2027-01-01T01:00:00"]
  );
}

#[test]
fn civil_sequence_matches_cron_equivalents() {
  // `*/15 * * * *`
  assert_eq!(
    sequence(
      "{*}-{*}-{*}T{*}:{00,15,30,45}:00Z",
      "2026-01-01T00:00:00Z",
      5
    ),
    [
      "2026-01-01T00:15:00",
      "2026-01-01T00:30:00",
      "2026-01-01T00:45:00",
      "2026-01-01T01:00:00",
      "2026-01-01T01:15:00",
    ]
  );
  // `30 9 * * *`
  assert_eq!(
    sequence("{*}-{*}-{*}T09:30:00Z", "2026-01-01T00:00:00Z", 3),
    [
      "2026-01-01T09:30:00",
      "2026-01-02T09:30:00",
      "2026-01-03T09:30:00",
    ]
  );
}

// ---------------------------------------------------------------------------
// Regression: huge offsets must be rejected, not panic or wrap
// ---------------------------------------------------------------------------

#[test]
fn huge_offsets_are_rejected_at_parse_time() {
  // `number` saturates at u32::MAX; the offset parser then rejects the value
  // before any multiplication can overflow.
  for expr in [
    "2026-01-01T00:00:00+4294967295:00",
    "2026-01-01T00:00:00+4294967:00",
    "2026-01-01T00:00:00+99999999999:00",
    "2026-01-01T00:00:00-9999999:99",
    "2026-01-01T00:00:00+999999999999999999999:00",
    "2026-01-01T00:00:00+00:99",
  ] {
    assert!(parse(expr).is_err(), "{expr} should be rejected");
  }

  // A large-but-representable offset still binds.
  let ok = parse("2026-01-01T00:00:00+25:59").unwrap();
  assert!(ok.bind().is_ok());
}

// ---------------------------------------------------------------------------
// Regression: stale TaskIds must not address reused slots
// ---------------------------------------------------------------------------

#[test]
fn stale_task_id_does_not_deregister_reused_slot() {
  let mut trigger = Trigger::new();
  let a = trigger
    .register("{*}-{*}-{*}T00:00:00Z", || async {})
    .unwrap();
  let b = trigger
    .register("{*}-{*}-{*}T00:00:00Z", || async {})
    .unwrap();
  assert_eq!(trigger.len(), 2);

  assert!(trigger.deregister(a));
  assert_eq!(trigger.len(), 1);

  // Registering again reuses `a`'s slot, with a fresh generation.
  let c = trigger
    .register("{*}-{*}-{*}T00:00:00Z", || async {})
    .unwrap();
  assert_eq!(c.index(), a.index(), "slot should be reused");
  assert_ne!(c.generation(), a.generation(), "generation must advance");
  assert_eq!(trigger.len(), 2);

  // The stale handle no longer matches: it must not remove `c`.
  assert!(!trigger.deregister(a), "stale id must be rejected");
  assert_eq!(trigger.len(), 2, "the reused slot must survive");

  // The current handle still works.
  assert!(trigger.deregister(c));
  assert_eq!(trigger.len(), 1);
  assert!(trigger.deregister(b));
  assert_eq!(trigger.len(), 0);
}

// ---------------------------------------------------------------------------
// Regression: `next` must not swallow an overdue tick
// ---------------------------------------------------------------------------

#[test]
fn next_may_report_overdue_and_fire_due_catches_up() {
  let mut trigger = Trigger::new();
  trigger
    .register("{*}-{*}-{*}T{*}:00:00Z", || async {})
    .unwrap();

  let first = trigger.next(ts("2026-01-01T00:00:00Z")).unwrap();
  assert_eq!(first, ts("2026-01-01T01:00:00Z"));

  // Time jumps far ahead without `fire_due` in between. `next` reports the
  // pending (now overdue) tick and does not advance past it.
  let late = ts("2026-06-01T00:00:00Z");
  assert_eq!(trigger.next(late), Some(ts("2026-01-01T01:00:00Z")));

  // `fire_due` fires the overdue tick once and advances from `late`.
  assert_eq!(trigger.fire_due(late).len(), 1);
  assert_eq!(trigger.next(late), Some(ts("2026-06-01T01:00:00Z")));
}

#[test]
fn next_at_exact_tick_does_not_skip_it() {
  let mut trigger = Trigger::new();
  trigger
    .register("{*}-{*}-{*}T{*}:{*}:{*}Z", || async {})
    .unwrap();

  let t0 = ts("2026-01-01T00:00:00Z");
  assert_eq!(trigger.next(t0), Some(ts("2026-01-01T00:00:01Z")));
  // Polling again exactly at the tick must not advance past it.
  assert_eq!(
    trigger.next(ts("2026-01-01T00:00:01Z")),
    Some(ts("2026-01-01T00:00:01Z"))
  );
  assert_eq!(trigger.fire_due(ts("2026-01-01T00:00:01Z")).len(), 1);
}
