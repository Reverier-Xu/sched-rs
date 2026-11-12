//! Binding a schedule to a concrete time zone.

use jiff::tz::{AmbiguousOffset, Offset, TimeZone};
use jiff::{Timestamp, Zoned};

use crate::schedule::{FixedOffset, IanaName, Schedule, ZoneSpec};
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
  /// - [`ZoneSpec::Local`] → the system time zone
  /// - [`ZoneSpec::Fixed`] → a fixed offset
  /// - [`ZoneSpec::Iana`] → an IANA time zone from the database, with DST
  ///   rules
  pub fn bind(&self) -> Result<BoundSchedule, BindError> {
    let tz = match self.zone {
      ZoneSpec::Local => TimeZone::system(),
      ZoneSpec::Fixed(offset) => {
        let offset = Offset::from_seconds(offset.as_seconds())
          .map_err(|_| BindError::OffsetOutOfRange { offset })?;
        TimeZone::fixed(offset)
      }
      ZoneSpec::Iana(name) => {
        TimeZone::get(name.as_str()).map_err(|_| BindError::UnknownTimeZone { name })?
      }
    };
    Ok(BoundSchedule {
      schedule: *self,
      tz,
    })
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
  /// DST transitions: an occurrence inside a gap (spring forward) is
  /// skipped; an occurrence inside a fold (fall back) fires once, at its
  /// first occurrence.
  pub fn next_after(&self, after: Timestamp) -> Option<Zoned> {
    let mut probe = Cursor::start_after(after.to_zoned(self.tz.clone()).datetime())?;
    loop {
      let candidate = self.schedule.next_civil_from(probe)?;
      let datetime = candidate.to_datetime()?;
      let ambiguous = self.tz.to_ambiguous_zoned(datetime);
      match ambiguous.offset() {
        AmbiguousOffset::Unambiguous { .. } => return ambiguous.unambiguous().ok(),
        AmbiguousOffset::Fold { .. } => return ambiguous.earlier().ok(),
        AmbiguousOffset::Gap { .. } => probe = candidate.plus_second()?,
      }
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
  /// The fixed offset exceeds jiff's range of -25:59:59..=25:59:59.
  #[error("fixed offset {offset} is outside the supported range -25:59:59..=25:59:59")]
  OffsetOutOfRange {
    /// The offending offset.
    offset: FixedOffset,
  },
}
