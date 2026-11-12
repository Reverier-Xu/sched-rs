//! Parser for schedule expressions.
//!
//! Grammar (case-insensitive `T`/`Z` separators):
//!
//! ```text
//! schedule := field "-" field "-" field ("T"|"t") field ":" field ":" field zone?
//! field    := digits | brace
//! brace    := "{" ws ("*" | item (ws "," ws item)* (ws ",")?) ws "}"
//! item     := digits (ws "-" ws digits)?
//! zone     := ("Z"|"z") | ("+"|"-") digits ":" digits | "[" name "]"
//! ```
//!
//! Numbers are digit runs of any width: `2026-1-2T3:4:5`, `+8:00` and even
//! `001` (January) are all fine. ASCII whitespace and one trailing comma
//! are allowed inside brace expressions (`{ 8, 12-14, }`); everywhere else
//! the grammar is strict. Every list position requires an item: `{}`,
//! `{,}` and `{9,,}` are syntax errors.
//!
//! Validation is strict. Field values must be inside their calendrical
//! domain, ranges must be non-inverted, and fixed offsets must fit jiff's
//! `-25:59..=+25:59` range; anything else is a parse error. Calendar
//! validity (February 30th, February 29th in a non-leap year) is checked
//! when computing trigger times, so such expressions parse but never match.

use winnow::ascii::{digit1, multispace0};
use winnow::combinator::{
  alt, cut_err, delimited, fail, opt, peek, preceded, separated_foldl1, terminated,
};
use winnow::dispatch;
use winnow::error::{ContextError, ErrMode};
use winnow::token::{any, one_of, take_while};
use winnow::{ModalResult, Parser};

use crate::error::ParseError;
use crate::schedule::{FieldSet, FixedOffset, IanaName, MAX_TZ_NAME_LEN, Schedule, ZoneSpec};

/// A schedule field. Used to look up each field's calendrical domain for
/// `{*}` expansion and for range validation.
#[derive(Clone, Copy)]
enum Field {
  Year,
  Month,
  Day,
  Hour,
  Minute,
  Second,
}

impl Field {
  /// The field's full calendrical domain, i.e. the values `{*}` expands to
  /// and the only values an explicit item may contain.
  fn domain(self) -> (u32, u32) {
    match self {
      Self::Year => (1, 9999),
      Self::Month => (1, 12),
      Self::Day => (1, 31),
      Self::Hour => (0, 23),
      Self::Minute | Self::Second => (0, 59),
    }
  }
}

fn in_domain(value: u32, domain: (u32, u32)) -> bool {
  (domain.0..=domain.1).contains(&value)
}

/// Parses a complete schedule expression.
///
/// The whole input must be consumed; trailing characters are an error.
pub fn parse(input: &str) -> Result<Schedule, ParseError> {
  schedule
    .parse(input)
    .map_err(|e| ParseError { offset: e.offset() })
}

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// A run of ASCII digits as a `u32`, saturating on overflow. Field and offset
/// parsers reject values outside their domain, so saturation never silently
/// changes a valid schedule.
fn number(input: &mut &str) -> ModalResult<u32> {
  digit1
    .map(|digits: &str| {
      digits.bytes().fold(0u32, |value, b| {
        value.saturating_mul(10).saturating_add(u32::from(b - b'0'))
      })
    })
    .parse_next(input)
}

/// Wraps a parser in optional ASCII whitespace.
fn ws<'a, O, P>(parser: P) -> impl Parser<&'a str, O, ErrMode<ContextError>>
where
  P: Parser<&'a str, O, ErrMode<ContextError>>,
{
  delimited(multispace0, parser, multispace0)
}

/// A single number that must be inside `domain`. On failure the input is
/// rewound to the number's start so the reported offset points at the value.
fn checked_number<'a>(domain: (u32, u32)) -> impl Parser<&'a str, u32, ErrMode<ContextError>> {
  move |input: &mut &'a str| {
    let start = *input;
    let value = number.parse_next(input)?;
    if !in_domain(value, domain) {
      *input = start;
      return Err(ErrMode::Cut(ContextError::new()));
    }
    Ok(value)
  }
}

/// `digits (ws "-" ws digits)?` — a single value or an inclusive range, both
/// inside `domain`. On failure the input is rewound to the offending item so
/// the reported offset points at the value.
fn item<'a>(domain: (u32, u32)) -> impl Parser<&'a str, (u32, u32), ErrMode<ContextError>> {
  move |input: &mut &'a str| {
    let start_pos = *input;
    let start = number.parse_next(input)?;
    if !in_domain(start, domain) {
      *input = start_pos;
      return Err(ErrMode::Cut(ContextError::new()));
    }
    let end = if opt(ws('-')).parse_next(input)?.is_some() {
      let end_pos = *input;
      let end = number.parse_next(input)?;
      if !in_domain(end, domain) || start > end {
        *input = if start > end { start_pos } else { end_pos };
        return Err(ErrMode::Cut(ContextError::new()));
      }
      end
    } else {
      start
    };
    Ok((start, end))
  }
}

// ---------------------------------------------------------------------------
// Brace expressions
// ---------------------------------------------------------------------------

/// The body of a brace expression: `"*" | items ","?`.
///
/// Whitespace is allowed around every token, and a single trailing comma is
/// permitted (`{9,}`). Every list position requires an item, so `{}`,
/// `{,}` and `{9,,}` fail here.
fn brace<'a, const WORDS: usize>(
  domain: (u32, u32),
) -> impl Parser<&'a str, FieldSet<WORDS>, ErrMode<ContextError>> {
  preceded(
    '{',
    cut_err(terminated(
      alt((
        ws('*').map(move |_| FieldSet::<WORDS>::range(domain.0, domain.1)),
        terminated(
          separated_foldl1(
            ws(item(domain)).map(|(start, end)| FieldSet::<WORDS>::range(start, end)),
            ws(','),
            |acc, _, item| acc.union(item),
          ),
          ws(opt(',')),
        ),
      )),
      '}',
    )),
  )
}

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

/// A field: digits or a brace expression. `field` supplies the calendrical
/// domain used for `{*}` expansion and range validation.
fn field<'a, const WORDS: usize>(
  field: Field,
) -> impl FnMut(&mut &'a str) -> ModalResult<FieldSet<WORDS>> {
  let domain = field.domain();
  move |input| {
    dispatch! {peek(any);
      '{' => brace::<WORDS>(domain),
      _ => checked_number(domain).map(FieldSet::<WORDS>::single),
    }
    .parse_next(input)
  }
}

// ---------------------------------------------------------------------------
// Time zone declaration
// ---------------------------------------------------------------------------

fn is_zone_char(c: char) -> bool {
  c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+' | '.')
}

/// `("+"|"-") digits ":" digits` — a fixed UTC offset, e.g. `+08:00`.
///
/// Hours are limited to `0..=25` and minutes to `0..=59`, matching jiff's
/// `-25:59:59..=25:59:59` range. Larger components are parse errors, not
/// silently normalized (`+00:99` is not `+01:39`).
fn fixed_offset(input: &mut &str) -> ModalResult<ZoneSpec> {
  let negative = one_of(['+', '-']).parse_next(input)? == '-';
  let hours_pos = *input;
  let hours = cut_err(number).parse_next(input)?;
  if hours > 25 {
    *input = hours_pos;
    return Err(ErrMode::Cut(ContextError::new()));
  }
  let _ = cut_err(':').parse_next(input)?;
  let minutes_pos = *input;
  let minutes = cut_err(number).parse_next(input)?;
  if minutes > 59 {
    *input = minutes_pos;
    return Err(ErrMode::Cut(ContextError::new()));
  }
  let total = (hours * 3600 + minutes * 60) as i32;
  Ok(ZoneSpec::Fixed(FixedOffset::from_seconds(if negative {
    -total
  } else {
    total
  })))
}

/// `"[" name "]"` — an IANA time zone name. Only the character set and
/// length are checked here; existence is checked when the schedule is bound.
fn iana_zone(input: &mut &str) -> ModalResult<ZoneSpec> {
  preceded(
    '[',
    cut_err(terminated(
      take_while(1..=MAX_TZ_NAME_LEN, is_zone_char).map(|name: &str| {
        ZoneSpec::Iana(IanaName::from_str(name).expect("length checked by take_while"))
      }),
      ']',
    )),
  )
  .parse_next(input)
}

/// The optional trailing time zone declaration.
fn zone_decl(input: &mut &str) -> ModalResult<ZoneSpec> {
  dispatch! {peek(any);
    'Z' | 'z' => any.map(|_| ZoneSpec::Fixed(FixedOffset::UTC)),
    '+' | '-' => fixed_offset,
    '[' => iana_zone,
    _ => fail,
  }
  .parse_next(input)
}

// ---------------------------------------------------------------------------
// Top level
// ---------------------------------------------------------------------------

fn schedule(input: &mut &str) -> ModalResult<Schedule> {
  let year: FieldSet<157> = field(Field::Year).parse_next(input)?;
  let _ = cut_err('-').parse_next(input)?;
  let month: FieldSet<1> = field(Field::Month).parse_next(input)?;
  let _ = cut_err('-').parse_next(input)?;
  let day: FieldSet<1> = field(Field::Day).parse_next(input)?;
  let _ = cut_err(one_of(['T', 't'])).parse_next(input)?;
  let hour: FieldSet<1> = field(Field::Hour).parse_next(input)?;
  let _ = cut_err(':').parse_next(input)?;
  let minute: FieldSet<1> = field(Field::Minute).parse_next(input)?;
  let _ = cut_err(':').parse_next(input)?;
  let second: FieldSet<1> = field(Field::Second).parse_next(input)?;
  let zone = opt(zone_decl)
    .parse_next(input)?
    .unwrap_or(ZoneSpec::Unspecified);
  Ok(Schedule {
    year,
    month,
    day,
    hour,
    minute,
    second,
    zone,
  })
}
