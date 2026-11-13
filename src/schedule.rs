//! Core schedule types produced by the parser.
//!
//! All types in this module are fixed-size and allocation-free, so they are
//! usable in `no_std` environments without a global allocator.

use core::fmt;

/// Maximum length of an IANA time zone name, in bytes.
pub(crate) const MAX_TZ_NAME_LEN: usize = 64;

/// A schedule field. Used by the parser for value validation and by
/// `Display` to decide when a set covers the whole domain and renders as
/// `{*}`.
#[derive(Clone, Copy)]
pub(crate) enum Field {
  Year,
  Month,
  Day,
  Hour,
  Minute,
  Second,
}

impl Field {
  /// The field's full domain, i.e. the values `{*}` expands to and the only
  /// values an explicit item may contain.
  pub(crate) fn domain(self) -> (u32, u32) {
    match self {
      Self::Year => (1, 9999),
      Self::Month => (1, 12),
      Self::Day => (1, 31),
      Self::Hour => (0, 23),
      Self::Minute | Self::Second => (0, 59),
    }
  }

  /// The number of values in the domain.
  fn len(self) -> u32 {
    let (start, end) = self.domain();
    end - start + 1
  }
}

/// A set of field values, stored as a bitmap of `WORDS * 64` bits.
///
/// Every schedule field is one of these; only the capacity differs:
///
/// The month, day, hour, minute and second fields use `FieldSet<1>` (64 bits
/// cover every value in their domains); the year field uses `FieldSet<157>`
/// (10048 bits cover 0..=9999, every real year).
///
/// Bit `v` is set when value `v` is part of the set. Sets produced by the
/// parser are always non-empty and contain only values inside the field's
/// domain.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldSet<const WORDS: usize> {
  words: [u64; WORDS],
}

impl<const WORDS: usize> FieldSet<WORDS> {
  /// An empty set.
  pub(crate) const EMPTY: Self = Self { words: [0; WORDS] };

  /// The largest value storable in the bitmap.
  const MAX: u32 = WORDS as u32 * 64 - 1;

  /// A set containing a single value. The parser guarantees the value is
  /// inside the field's domain.
  pub(crate) fn single(value: u32) -> Self {
    Self::range(value, value)
  }

  /// A set covering `start..=end`. The parser guarantees `start <= end` and
  /// that both endpoints are inside the field's domain.
  pub(crate) fn range(start: u32, end: u32) -> Self {
    debug_assert!(start <= end);
    debug_assert!(end <= Self::MAX);
    let mut set = Self::EMPTY;
    set.insert_range(start, end);
    set
  }

  /// The union of two sets.
  pub(crate) fn union(self, other: Self) -> Self {
    let mut words = [0; WORDS];
    for (i, word) in words.iter_mut().enumerate() {
      *word = self.words[i] | other.words[i];
    }
    Self { words }
  }

  fn insert_range(&mut self, start: u32, end: u32) {
    if start > end {
      return;
    }
    debug_assert!(end <= Self::MAX);
    let (start_word, start_bit) = ((start / 64) as usize, start % 64);
    let (end_word, end_bit) = ((end / 64) as usize, end % 64);
    if start_word == end_word {
      self.words[start_word] |= (u64::MAX << start_bit) & (u64::MAX >> (63 - end_bit));
      return;
    }
    self.words[start_word] |= u64::MAX << start_bit;
    for word in &mut self.words[start_word + 1..end_word] {
      *word = u64::MAX;
    }
    self.words[end_word] |= u64::MAX >> (63 - end_bit);
  }

  /// Returns `true` if `value` is part of the set.
  pub fn contains(&self, value: u32) -> bool {
    if value > Self::MAX {
      return false;
    }
    self.words[(value / 64) as usize] & (1 << (value % 64)) != 0
  }

  /// Number of values in the set.
  pub fn len(&self) -> u32 {
    self.words.iter().map(|word| word.count_ones()).sum()
  }

  /// Returns `true` if the set is empty.
  pub fn is_empty(&self) -> bool {
    self.words.iter().all(|&word| word == 0)
  }

  /// The smallest value in the set, if any.
  pub fn min(&self) -> Option<u32> {
    for (i, &word) in self.words.iter().enumerate() {
      if word != 0 {
        return Some(i as u32 * 64 + word.trailing_zeros());
      }
    }
    None
  }

  /// The largest value in the set, if any.
  pub fn max(&self) -> Option<u32> {
    for (i, &word) in self.words.iter().enumerate().rev() {
      if word != 0 {
        return Some(i as u32 * 64 + (63 - word.leading_zeros()));
      }
    }
    None
  }

  /// The smallest value in the set greater than or equal to `value`.
  pub(crate) fn next_ge(&self, value: u32) -> Option<u32> {
    if value > Self::MAX {
      return None;
    }
    let mut word = (value / 64) as usize;
    let mut bits = self.words[word] & (u64::MAX << (value % 64));
    loop {
      if bits != 0 {
        return Some(word as u32 * 64 + bits.trailing_zeros());
      }
      word += 1;
      bits = *self.words.get(word)?;
    }
  }

  /// The smallest value in the set strictly greater than `value`.
  pub(crate) fn next_gt(&self, value: u32) -> Option<u32> {
    self.next_ge(value.checked_add(1)?)
  }

  /// Iterates over the values in ascending order.
  pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
    FieldSetIter {
      words: &self.words,
      word: 0,
      bits: self.words[0],
    }
  }

  /// Iterates over maximal runs of consecutive values, e.g. `{8,12-14}`
  /// yields `(8, 8)` and `(12, 14)`.
  fn runs(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
    let mut iter = self.iter().peekable();
    core::iter::from_fn(move || {
      let start = iter.next()?;
      let mut end = start;
      while let Some(&next) = iter.peek() {
        if next != end + 1 {
          break;
        }
        end = next;
        iter.next();
      }
      Some((start, end))
    })
  }
}

impl<const WORDS: usize> fmt::Debug for FieldSet<WORDS> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{{")?;
    for (i, (start, end)) in self.runs().enumerate() {
      if i > 0 {
        write!(f, ", ")?;
      }
      if start == end {
        write!(f, "{start}")?;
      } else {
        write!(f, "{start}..={end}")?;
      }
    }
    write!(f, "}}")
  }
}

/// Iterator over the values of a [`FieldSet`], in ascending order.
struct FieldSetIter<'a, const WORDS: usize> {
  words: &'a [u64; WORDS],
  word: usize,
  bits: u64,
}

impl<const WORDS: usize> Iterator for FieldSetIter<'_, WORDS> {
  type Item = u32;

  fn next(&mut self) -> Option<u32> {
    loop {
      if self.bits != 0 {
        let bit = self.bits.trailing_zeros();
        self.bits &= self.bits - 1;
        return Some(self.word as u32 * 64 + bit);
      }
      self.word += 1;
      self.bits = *self.words.get(self.word)?;
    }
  }
}

/// A parsed IANA time zone name, e.g. `Asia/Shanghai`.
///
/// Only the character set and length are validated at parse time; existence
/// in the time zone database is checked when the schedule is bound to a
/// [`jiff::tz::TimeZone`].
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct IanaName {
  buf: [u8; MAX_TZ_NAME_LEN],
  len: u8,
}

impl IanaName {
  pub(crate) fn from_str(name: &str) -> Option<Self> {
    if name.is_empty() || name.len() > MAX_TZ_NAME_LEN {
      return None;
    }
    let mut buf = [0u8; MAX_TZ_NAME_LEN];
    buf[..name.len()].copy_from_slice(name.as_bytes());
    Some(Self {
      buf,
      len: name.len() as u8,
    })
  }

  /// The name as a string slice.
  pub fn as_str(&self) -> &str {
    // Invariant: the buffer is only ever filled from a valid `&str`.
    core::str::from_utf8(&self.buf[..self.len as usize]).expect("IANA name must be valid UTF-8")
  }
}

impl fmt::Debug for IanaName {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    fmt::Debug::fmt(self.as_str(), f)
  }
}

impl fmt::Display for IanaName {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// A fixed UTC offset, e.g. `+08:00`.
///
/// Offsets are validated when parsed: hours are at most 25 and minutes at
/// most 59, matching jiff's `-25:59:59..=25:59:59` range.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FixedOffset {
  seconds: i32,
}

impl FixedOffset {
  /// The zero offset (`+00:00` / `Z`).
  pub const UTC: Self = Self { seconds: 0 };

  /// Creates an offset from a total number of seconds.
  ///
  /// The parser validates the range before calling this.
  pub(crate) const fn from_seconds(seconds: i32) -> Self {
    Self { seconds }
  }

  /// The total offset in seconds, e.g. `28800` for `+08:00`.
  pub fn as_seconds(&self) -> i32 {
    self.seconds
  }

  /// The signed whole-hour part, e.g. `8` for `+08:00`, `-5` for `-05:30`.
  pub fn hours(&self) -> i32 {
    self.seconds / 3600
  }

  /// The signed minute remainder, e.g. `30` for `+08:30`, `-30` for `-05:30`.
  pub fn minutes(&self) -> i32 {
    (self.seconds % 3600) / 60
  }
}

impl fmt::Display for FixedOffset {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let sign = if self.seconds < 0 { '-' } else { '+' };
    let abs = self.seconds.unsigned_abs();
    write!(f, "{}{:02}:{:02}", sign, abs / 3600, (abs % 3600) / 60)
  }
}

/// The time zone declaration of a schedule.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ZoneSpec {
  /// No declaration: [`Schedule::next_after`] matches civil time, and
  /// `Schedule::bind` resolves to the system time zone.
  Unspecified,
  /// A fixed UTC offset, e.g. `+08:00`, `-05:00` or `Z`.
  Fixed(FixedOffset),
  /// An IANA time zone name, e.g. `[Asia/Shanghai]`. DST rules apply.
  Iana(IanaName),
}

/// A parsed schedule expression.
///
/// Example: `2026-{*}-{*}T{8,12-14}:00:00[Asia/Shanghai]`.
///
/// Field domains and ranges are validated at parse time. Calendar validity
/// (e.g. February 30th, or February 29th in a non-leap year) is checked when
/// computing trigger times.
///
/// A schedule is a fixed-size value of roughly 1.3 KiB, so it is [`Clone`]
/// but deliberately not `Copy`: share it behind an `Arc` when several tasks
/// need the same schedule instead of copying it implicitly.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Schedule {
  pub(crate) year: FieldSet<157>,
  pub(crate) month: FieldSet<1>,
  pub(crate) day: FieldSet<1>,
  pub(crate) hour: FieldSet<1>,
  pub(crate) minute: FieldSet<1>,
  pub(crate) second: FieldSet<1>,
  pub(crate) zone: ZoneSpec,
}

impl Schedule {
  /// The year set, e.g. `{2024-2030}` or `{*}`.
  pub fn year(&self) -> &FieldSet<157> {
    &self.year
  }

  /// The month set.
  pub fn month(&self) -> &FieldSet<1> {
    &self.month
  }

  /// The day-of-month set.
  pub fn day(&self) -> &FieldSet<1> {
    &self.day
  }

  /// The hour set.
  pub fn hour(&self) -> &FieldSet<1> {
    &self.hour
  }

  /// The minute set.
  pub fn minute(&self) -> &FieldSet<1> {
    &self.minute
  }

  /// The second set.
  pub fn second(&self) -> &FieldSet<1> {
    &self.second
  }

  /// The time zone declaration.
  pub fn zone(&self) -> ZoneSpec {
    self.zone
  }
}

/// Writes one field as a valid schedule expression: a bare value, `{*}` for a
/// full domain, or a brace list of values and inclusive ranges. Values are
/// zero-padded to `width` so the output is canonical.
fn write_field<const WORDS: usize>(
  f: &mut fmt::Formatter<'_>, set: &FieldSet<WORDS>, field: Field, width: usize,
) -> fmt::Result {
  let domain_len = field.len();
  if set.len() == 1 {
    return write!(
      f,
      "{:0width$}",
      set.min().expect("parsed field sets are non-empty"),
      width = width
    );
  }
  // The parser guarantees every value is inside the domain, so a set of the
  // domain's size must cover it exactly.
  if set.len() == domain_len {
    return f.write_str("{*}");
  }
  f.write_str("{")?;
  for (i, (start, end)) in set.runs().enumerate() {
    if i > 0 {
      f.write_str(",")?;
    }
    if start == end {
      write!(f, "{start:0width$}", width = width)?;
    } else {
      write!(
        f,
        "{start:0width$}-{end:0width$}",
        width = width,
        start = start,
        end = end
      )?;
    }
  }
  f.write_str("}")
}

/// Renders the schedule as an expression that parses back to the same value.
impl fmt::Display for Schedule {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write_field(f, &self.year, Field::Year, 4)?;
    f.write_str("-")?;
    write_field(f, &self.month, Field::Month, 2)?;
    f.write_str("-")?;
    write_field(f, &self.day, Field::Day, 2)?;
    f.write_str("T")?;
    write_field(f, &self.hour, Field::Hour, 2)?;
    f.write_str(":")?;
    write_field(f, &self.minute, Field::Minute, 2)?;
    f.write_str(":")?;
    write_field(f, &self.second, Field::Second, 2)?;
    match self.zone {
      ZoneSpec::Unspecified => Ok(()),
      ZoneSpec::Fixed(offset) => write!(f, "{offset}"),
      ZoneSpec::Iana(name) => write!(f, "[{name}]"),
    }
  }
}
