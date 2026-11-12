//! Error types for schedule parsing.

/// An error that occurred while parsing a schedule expression.
///
/// sched is deliberately lazy about validity: anything that can never match
/// a real date-time simply parses and never triggers — a month of `13`, an
/// inverted range like `{14-8}` (expanding to an empty set), an offset of
/// `+99:00`. Expressions are short and declarative, so mistakes are visible
/// at a glance rather than reported eagerly. Malformed syntax is the only
/// parse error.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("invalid schedule syntax at byte offset {offset}")]
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
