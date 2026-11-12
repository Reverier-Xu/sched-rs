//! The calendrical search at the heart of trigger computation.
//!
//! Fields are advanced from largest to smallest; advancing a field resets
//! all smaller fields, which are then re-checked. The search is pure
//! calendar arithmetic — time zones and DST are handled one layer up (see
//! [`crate::BoundSchedule`]).

use jiff::civil::{Date, DateTime};

use crate::error::TriggerError;
use crate::schedule::{Schedule, ZoneSpec};

/// A civil date-time decomposed into fields.
///
/// Invariant: the fields always form a valid calendar date-time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Cursor {
  year: i32,
  month: u8,
  day: u8,
  hour: u8,
  minute: u8,
  second: u8,
}

impl Cursor {
  /// Floors `dt` to whole seconds and steps one second forward, giving the
  /// smallest candidate strictly after `dt`.
  pub(crate) fn start_after(dt: DateTime) -> Option<Self> {
    Self {
      year: dt.year() as i32,
      month: dt.month() as u8,
      day: dt.day() as u8,
      hour: dt.hour() as u8,
      minute: dt.minute() as u8,
      second: dt.second() as u8,
    }
    .plus_second()
  }

  pub(crate) fn plus_second(mut self) -> Option<Self> {
    self.bump_second().map(|()| self)
  }

  pub(crate) fn to_datetime(self) -> Option<DateTime> {
    DateTime::new(
      self.year as i16,
      self.month as i8,
      self.day as i8,
      self.hour as i8,
      self.minute as i8,
      self.second as i8,
      0,
    )
    .ok()
  }

  // The `bump_*` family increments one field by one unit, carrying into
  // larger fields and resetting all smaller fields. All return `None` past
  // the year 9999 (jiff's civil ceiling).

  fn bump_second(&mut self) -> Option<()> {
    self.second += 1;
    if self.second > 59 {
      self.second = 0;
      return self.bump_minute();
    }
    Some(())
  }

  fn bump_minute(&mut self) -> Option<()> {
    self.minute += 1;
    if self.minute > 59 {
      self.minute = 0;
      return self.bump_hour();
    }
    Some(())
  }

  fn bump_hour(&mut self) -> Option<()> {
    self.hour += 1;
    if self.hour > 23 {
      self.hour = 0;
      return self.bump_day();
    }
    Some(())
  }

  fn bump_day(&mut self) -> Option<()> {
    self.day += 1;
    if self.day as u32 > days_in_month(self.year, self.month)? {
      return self.bump_month();
    }
    self.hour = 0;
    self.minute = 0;
    self.second = 0;
    Some(())
  }

  fn bump_month(&mut self) -> Option<()> {
    self.month += 1;
    if self.month > 12 {
      return self.bump_year();
    }
    self.day = 1;
    self.hour = 0;
    self.minute = 0;
    self.second = 0;
    Some(())
  }

  fn bump_year(&mut self) -> Option<()> {
    self.year += 1;
    if self.year > 9999 {
      return None;
    }
    self.month = 1;
    self.day = 1;
    self.hour = 0;
    self.minute = 0;
    self.second = 0;
    Some(())
  }
}

fn days_in_month(year: i32, month: u8) -> Option<u32> {
  Date::new(year as i16, month as i8, 1)
    .ok()
    .map(|date| date.days_in_month() as u32)
}

impl Schedule {
  /// Computes the next civil trigger time strictly after `after`.
  ///
  /// Pure calendrical matching: no time zone is applied. Errors with
  /// [`TriggerError::TimeZoneDeclared`] if the schedule declares one —
  /// civil matching would silently ignore it, so bind the schedule first
  /// with `Schedule::bind`.
  pub fn next_after(&self, after: DateTime) -> Result<Option<DateTime>, TriggerError> {
    if !matches!(self.zone, ZoneSpec::Local) {
      return Err(TriggerError::TimeZoneDeclared);
    }
    let Some(start) = Cursor::start_after(after) else {
      return Ok(None);
    };
    Ok(self.next_civil_from(start).and_then(Cursor::to_datetime))
  }

  /// The smallest matching civil date-time at or after `cursor`.
  pub(crate) fn next_civil_from(&self, mut cursor: Cursor) -> Option<Cursor> {
    // An empty field set can never match.
    if self.year.is_empty()
      || self.month.is_empty()
      || self.day.is_empty()
      || self.hour.is_empty()
      || self.minute.is_empty()
      || self.second.is_empty()
    {
      return None;
    }
    loop {
      if !self.year.contains(cursor.year as u32) {
        let year = self.year.next_ge(cursor.year.max(0) as u32)?;
        if year > 9999 {
          return None;
        }
        cursor = Cursor {
          year: year as i32,
          month: 1,
          day: 1,
          hour: 0,
          minute: 0,
          second: 0,
        };
        continue;
      }
      if !self.month.contains(cursor.month as u32) {
        match self.month.next_gt(cursor.month as u32) {
          Some(month) => {
            cursor.month = month as u8;
            cursor.day = 1;
            cursor.hour = 0;
            cursor.minute = 0;
            cursor.second = 0;
          }
          None => cursor.bump_year()?,
        }
        continue;
      }
      let dim = days_in_month(cursor.year, cursor.month)?;
      if cursor.day as u32 > dim || !self.day.contains(cursor.day as u32) {
        match self
          .day
          .next_gt(cursor.day as u32)
          .filter(|&day| day <= dim)
        {
          Some(day) => {
            cursor.day = day as u8;
            cursor.hour = 0;
            cursor.minute = 0;
            cursor.second = 0;
          }
          None => cursor.bump_month()?,
        }
        continue;
      }
      if !self.hour.contains(cursor.hour as u32) {
        match self.hour.next_gt(cursor.hour as u32) {
          Some(hour) => {
            cursor.hour = hour as u8;
            cursor.minute = 0;
            cursor.second = 0;
          }
          None => cursor.bump_day()?,
        }
        continue;
      }
      if !self.minute.contains(cursor.minute as u32) {
        match self.minute.next_gt(cursor.minute as u32) {
          Some(minute) => {
            cursor.minute = minute as u8;
            cursor.second = 0;
          }
          None => cursor.bump_hour()?,
        }
        continue;
      }
      if !self.second.contains(cursor.second as u32) {
        match self.second.next_gt(cursor.second as u32) {
          Some(second) => cursor.second = second as u8,
          None => cursor.bump_minute()?,
        }
        continue;
      }
      return Some(cursor);
    }
  }
}
