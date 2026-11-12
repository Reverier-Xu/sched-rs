#![cfg_attr(not(feature = "std"), no_std)]
//! # sched
//!
//! A cron alternative using IANA-style schedule expressions:
//!
//! ```text
//! yyyy-MM-ddThh:mm:ss+08:00
//! yyyy-MM-ddThh:mm:ss[Asia/Shanghai]
//! ```
//!
//! Every block except the time zone declaration can be replaced with a brace
//! expression. `{*}` matches every value, and `{8,12-14}` matches the listed
//! values and inclusive ranges:
//!
//! - `2026-{*}-{*}T{8,12-14}:00:00` — every day of 2026 at 08:00, 12:00,
//!   13:00 and 14:00, in the device's local time zone.
//! - `{*}-{*}-{*}T08:00:00+08:00` — every day at 08:00 at the fixed `+08:00`
//!   offset.
//! - `{*}-{*}-{*}T08:00:00[America/Sao_Paulo]` — every day at 08:00 in
//!   São Paulo, following daylight saving time.
//!
//! # Validation policy
//!
//! Parsing is lazy: anything that can never match a real date-time simply
//! parses and never triggers — a month of `13`, February 30th, an inverted
//! range like `{14-8}` (expanding to an empty set), an offset of `+99:00`.
//! Malformed syntax is the only parse error; calendrical validity is jiff's
//! job, enforced when computing trigger times.
//!
//! # Computing trigger times
//!
//! - [`Schedule::next_after`] matches civil date-times (no time zone), and
//!   works without an allocator. A schedule that declares a time zone is an
//!   error here — bind it first.
//! - With `alloc`, [`Schedule::bind`] resolves the zone declaration into a
//!   [`BoundSchedule`], whose `next_after` returns absolute [`Zoned`] times.
//!   DST gaps are skipped; folds fire once (at the first occurrence).
//! - [`Trigger`] is a runtime-agnostic task table: register async callbacks,
//!   then drive it from any executor — sleep until [`Trigger::next`], call
//!   [`Trigger::fire_due`], and spawn the returned [`TaskRun`] futures. A
//!   tick whose previous run is still in flight is skipped.
//!
//! # Feature flags
//!
//! - `std` (default): full standard library support.
//! - `alloc`: time zone binding and the task trigger in `no_std`
//!   environments.
//! - `bundle-tzdb`: always use the bundled time zone database.
//! - `tzdb-platform` (default): use the bundled time zone database only on
//!   platforms without a system database (e.g. Windows).

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
mod bind;
mod error;
mod parser;
mod schedule;
mod search;
#[cfg(feature = "alloc")]
mod trigger;

use core::str::FromStr;

pub use error::{ParseError, TriggerError};
pub use jiff::civil::DateTime;
pub use jiff::{SignedDuration, Timestamp};
pub use parser::parse;
pub use schedule::{
  FieldSet, FieldSetIter, FixedOffset, IanaName, MAX_TZ_NAME_LEN, Schedule, ZoneSpec,
};

#[cfg(feature = "alloc")]
pub use bind::{BindError, BoundSchedule};
#[cfg(feature = "alloc")]
pub use jiff::Zoned;
#[cfg(feature = "alloc")]
pub use trigger::{RegisterError, TaskId, TaskRun, Trigger};

impl Schedule {
  /// Parses a schedule expression; see [`parse`].
  pub fn parse(input: &str) -> Result<Self, ParseError> {
    parser::parse(input)
  }
}

impl FromStr for Schedule {
  type Err = ParseError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    parser::parse(s)
  }
}
