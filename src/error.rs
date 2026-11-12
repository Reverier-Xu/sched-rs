//! Error types for schedule parsing.

/// An error that occurred while parsing a schedule expression.
///
/// Parsing validates syntax, field domains, ranges, and fixed offsets. A
/// month of `13`, an inverted range like `{14-8}`, and an offset of `+99:00`
/// are all parse errors. Calendar validity (February 30th, leap days) is
/// checked later, when trigger times are computed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("invalid schedule at byte offset {offset}")]
pub struct ParseError {
  /// Byte offset at which the parser gave up.
  pub offset: usize,
}

/// An error that occurred while computing trigger times.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum TriggerError {
  /// The schedule declares a time zone, but [`Schedule::next_after`][crate::Schedule#method.next_after]
  /// works on civil date-times only. Bind the schedule to a concrete time
  /// zone first with `Schedule::bind`.
  #[error("schedule declares a time zone; bind it first with `Schedule::bind`")]
  TimeZoneDeclared,
}
