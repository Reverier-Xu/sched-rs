# sched

A small, allocation-conscious cron alternative built on
[`jiff`](https://docs.rs/jiff). Schedules are written with IANA-style
date-time expressions; fields can be lists or ranges.

```text
2026-03-08T12:30:45+08:00
2026-{*}-{*}T{8,12-14}:00:00
{*}-{*}-{*}T08:00:00[Asia/Shanghai]
```

- `{*}` matches every value in the field's domain.
- `{8,12-14}` matches the listed values and inclusive ranges.
- A trailing `Z` means UTC; `+08:00` is a fixed offset; `[Asia/Shanghai]` is
  an IANA time zone with DST rules.

## Validation

Parsing is strict. Field values must be inside their domains (year
`1..=9999`, month `1..=12`, day `1..=31`, hour `0..=23`, minute and second
`0..=59`), ranges must be non-inverted, and fixed offsets must fit
`-25:59..=+25:59`. Anything else is a parse error.

Calendar validity is checked when trigger times are computed. For example,
`{*}-02-30T00:00:00` parses but never fires because February 30th does not
exist.

## Usage

```rust
use sched::{Schedule, Timestamp};

let schedule: Schedule = "{*}-{*}-{*}T08:00:00[Asia/Shanghai]".parse()?;
let bound = schedule.bind()?;
let next = bound.next_after(Timestamp::now()).expect("schedule is exhausted");
println!("next run: {next}");
# Ok::<(), Box<dyn std::error::Error>>(())
```

`Schedule::next_after` matches civil date-times and works without an
allocator. `Schedule::bind` resolves the time zone and requires the `alloc`
feature.

### Trigger

`Trigger` is runtime-agnostic: it produces futures, and the host runtime
spawns them.

```rust
use sched::{Timestamp, Trigger};

let mut trigger = Trigger::new();
trigger.register("{*}-{*}-{*}T08:00:00", || async {
  // do work
})?;

loop {
  let Some(at) = trigger.next(Timestamp::now()) else { break };
  sleep_until(at).await;
  for run in trigger.fire_due(Timestamp::now()) {
    spawn(run);
  }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

`next` may return an instant that is already in the past if the runtime was
late; `fire_due` advances the task and fires that overdue tick once. A tick
whose previous run is still in flight is skipped.

## Features

| Feature | Meaning |
| --- | --- |
| `std` (default) | Standard library support. |
| `alloc` | Time zone binding and `Trigger` in `no_std + alloc` environments. |
| `bundle-tzdb` | Always use the bundled time zone database. |
| `tzdb-platform` (default) | Use the bundled database only where the platform has none. |

For an embedded `no_std + alloc` target without a system time zone database:

```toml
[dependencies]
sched = { version = "0.1", default-features = false, features = ["bundle-tzdb"] }
```

The civil-only API (`Schedule::next_after`) works with no features at all.

## `sched` daemon

`crates/sched-cli` builds the `sched` binary, a small daemon that reads a
crontab-style file and runs commands through `bash -c`:

```text
{*}-{*}-{*}T08:00:00[Asia/Shanghai]   echo "good morning"
2026-{*}-{*}T{8, 12-14}:00:00+08:00   /usr/local/bin/backup.sh
```

Run `sched [--check] [path-to-schedtab]`; the default path is
`/etc/sched/schedtab`. From the workspace, use
`cargo run -p sched-cli --bin sched -- --check examples/schedtab`.

## Development

```sh
cargo test --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo doc --no-deps --all-features
cargo package --allow-dirty --no-verify
```

## License

Licensed under either of MIT or Apache-2.0, at your option.
