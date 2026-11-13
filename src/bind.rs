//! Binding a schedule to a concrete time zone.

use jiff::tz::{AmbiguousOffset, Offset, TimeZone};
use jiff::{Timestamp, Zoned};

use crate::schedule::{IanaName, Schedule, ZoneSpec};
use crate::search::Cursor;

/// A schedule bound to a concrete time zone, ready to compute absolute
/// trigger times.
#[derive(Clone, Debug)]
pub struct BoundSchedule {
  schedule: Schedule,
  tz: TimeZone,
}

impl Schedule {
  /// Binds the schedule's zone declaration to a concrete time zone:
  ///
  /// - [`ZoneSpec::Unspecified`] → the system time zone
  /// - [`ZoneSpec::Fixed`] → a fixed offset
  /// - [`ZoneSpec::Iana`] → an IANA time zone from the database, with DST
  ///   rules
  ///
  /// Fixed offsets are validated at parse time, so only an unknown IANA name
  /// can fail here.
  pub fn bind(&self) -> Result<BoundSchedule, BindError> {
    let tz = self.resolve_zone()?;
    Ok(BoundSchedule {
      schedule: self.clone(),
      tz,
    })
  }

  /// Like [`Schedule::bind`], but consumes the schedule instead of cloning
  /// it. Used when the caller hands ownership to the trigger.
  pub(crate) fn bind_owned(self) -> Result<BoundSchedule, BindError> {
    let tz = self.resolve_zone()?;
    Ok(BoundSchedule { schedule: self, tz })
  }

  fn resolve_zone(&self) -> Result<TimeZone, BindError> {
    match self.zone {
      ZoneSpec::Unspecified => Ok(TimeZone::system()),
      ZoneSpec::Fixed(offset) => {
        let offset = Offset::from_seconds(offset.as_seconds())
          .expect("fixed offsets are validated at parse time");
        Ok(TimeZone::fixed(offset))
      }
      ZoneSpec::Iana(name) => {
        TimeZone::get(name.as_str()).map_err(|_| BindError::UnknownTimeZone { name })
      }
    }
  }
}

impl BoundSchedule {
  /// The underlying schedule.
  pub fn schedule(&self) -> &Schedule {
    &self.schedule
  }

  /// The bound time zone.
  pub fn time_zone(&self) -> &TimeZone {
    &self.tz
  }

  /// Computes the next trigger time strictly after `after`.
  ///
  /// Returns `None` if the schedule never matches again (e.g. its year set
  /// is exhausted, or it names a nonexistent date like February 30th).
  ///
  /// Each civil match fires at most once, at its earliest absolute occurrence.
  /// If that instant is not strictly after `after`, the match is skipped: a
  /// fold's second occurrence is never returned, and a civil time inside a
  /// gap (spring forward) has no occurrence at all.
  pub fn next_after(&self, after: Timestamp) -> Option<Zoned> {
    let mut probe = Cursor::start_after(after.to_zoned(self.tz.clone()).datetime())?;
    loop {
      let candidate = self.schedule.next_civil_from(probe)?;
      let datetime = candidate.to_datetime()?;
      let ambiguous = self.tz.to_ambiguous_zoned(datetime);
      let earliest = match ambiguous.offset() {
        AmbiguousOffset::Unambiguous { .. } => ambiguous.unambiguous().ok(),
        AmbiguousOffset::Fold { .. } => ambiguous.earlier().ok(),
        AmbiguousOffset::Gap { .. } => None,
      };
      if let Some(zoned) = earliest
        && zoned.timestamp() > after
      {
        return Some(zoned);
      }
      probe = candidate.plus_second()?;
    }
  }
}

/// An error that occurred while binding a schedule to a time zone.
#[derive(Clone, Debug, thiserror::Error)]
pub enum BindError {
  /// The IANA time zone name doesn't exist in the database.
  #[error("unknown time zone `{name}`")]
  UnknownTimeZone {
    /// The offending name.
    name: IanaName,
  },
}
