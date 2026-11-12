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
//! Validity is not checked at parse time: values that can never match a
//! real date-time (a month of `13`, February 30th, an inverted range like
//! `{14-8}`, an offset of `+99:00`) parse fine but simply never trigger.
//! The only parse errors are malformed syntax and year lists exceeding
//! [`MAX_YEAR_RANGES`].

use winnow::ascii::{digit1, multispace0};
use winnow::combinator::{
  alt, cut_err, delimited, fail, opt, peek, preceded, repeat, separated_foldl1, terminated,
};
use winnow::dispatch;
use winnow::error::{ContextError, ErrMode};
use winnow::token::{any, one_of, take_till, take_while};
use winnow::{ModalResult, Parser};

use crate::error::ParseError;
use crate::schedule::{FieldSet, FixedOffset, IanaName, MAX_TZ_NAME_LEN, Schedule, ZoneSpec};

/// A schedule field. Only used to look up each field's calendrical domain
/// for `{*}` expansion.
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
  /// The field's full calendrical domain, i.e. the values `{*}` expands to.
  ///
  /// This is not a validity check: values outside the domain parse fine but
  /// simply never match when computing trigger times (validity is jiff's
  /// job, enforced when civil date-times are constructed).
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

/// A run of ASCII digits as a `u32`, saturating on overflow. A saturated
/// value exceeds every storage domain and simply never matches.
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

// ---------------------------------------------------------------------------
// Brace expressions
// ---------------------------------------------------------------------------

/// `digits (ws "-" ws digits)?` — a single value or an inclusive range.
fn item(input: &mut &str) -> ModalResult<(u32, u32)> {
  let start = number.parse_next(input)?;
  let end = opt(preceded(ws('-'), number))
    .parse_next(input)?
    .unwrap_or(start);
  Ok((start, end))
}

/// The result of a brace expression: the wildcard or a list of items.
enum Brace<T> {
  Any,
  Items(T),
}

/// The body of a brace expression: `"*" | items ","?`.
///
/// Whitespace is allowed around every token, and a single trailing comma is
/// permitted (`{9,}`). Every list position requires an item, so `{}`,
/// `{,}` and `{9,,}` fail here.
fn brace_body<'a, T, I, M>(
  item: I, mut merge: M,
) -> impl Parser<&'a str, Brace<T>, ErrMode<ContextError>>
where
  I: Parser<&'a str, T, ErrMode<ContextError>>,
  M: FnMut(T, T) -> T,
{
  preceded(
    '{',
    cut_err(terminated(
      alt((
        ws('*').map(|_| Brace::Any),
        terminated(
          separated_foldl1(ws(item), ws(','), move |acc, _, item| merge(acc, item)),
          ws(opt(',')),
        )
        .map(Brace::Items),
      )),
      '}',
    )),
  )
}

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

/// A field: digits or a brace expression. `domain` is the field's
/// calendrical domain, used for `{*}` expansion.
fn field<const WORDS: usize>(
  domain: (u32, u32),
) -> impl FnMut(&mut &str) -> ModalResult<FieldSet<WORDS>> {
  move |input| {
    dispatch! {peek(any);
      '{' => brace_body(
        item.map(|(start, end)| FieldSet::<WORDS>::range(start, end)),
        |acc, item| acc.union(item),
      )
      .map(move |brace| match brace {
        Brace::Any => FieldSet::full(domain.0, domain.1),
        Brace::Items(set) => set,
      }),
      _ => number.map(FieldSet::<WORDS>::single),
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
fn fixed_offset(input: &mut &str) -> ModalResult<ZoneSpec> {
  let negative = one_of(['+', '-']).parse_next(input)? == '-';
  let (hours, _, minutes) = cut_err((number, ':', number)).parse_next(input)?;
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
// Structural segmentation
// ---------------------------------------------------------------------------

/// Consumes a field segment: everything up to the next `stop` character
/// outside braces (the delimiter itself is left unconsumed). `{...}` spans
/// count as opaque content. Empty segments are rejected.
fn segment<'a>(
  stop: impl Fn(char) -> bool,
) -> impl Parser<&'a str, &'a str, ErrMode<ContextError>> {
  repeat(
    0..,
    alt((
      ('{', take_till(0.., '}'), '}').void(),
      take_while(1.., move |c: char| !stop(c) && c != '{' && c != '}').void(),
    )),
  )
  .fold(|| (), |(), _| ())
  .take()
  .verify(|s: &str| !s.is_empty())
}

/// Parses a segment's content to completion. On failure, the outer input
/// is rewound to the failure point so the top-level [`parse`] reports the
/// absolute offset.
fn subparse<'a, T>(
  mut parser: impl FnMut(&mut &'a str) -> ModalResult<T>, seg: &'a str, origin: &'a str,
  input: &mut &'a str,
) -> ModalResult<T> {
  match parser.parse(seg) {
    Ok(value) => Ok(value),
    Err(err) => {
      let at = (seg.as_ptr() as usize - origin.as_ptr() as usize) + err.offset();
      *input = &origin[at..];
      Err(ErrMode::Cut(ContextError::new()))
    }
  }
}

// ---------------------------------------------------------------------------
// Top level
// ---------------------------------------------------------------------------

/// Parses the next field segment: locate its extent, then parse the
/// content to completion (see [`segment`] and [`subparse`]).
fn next_segment<'a, T>(
  parser: impl FnMut(&mut &'a str) -> ModalResult<T>, stop: impl Fn(char) -> bool, origin: &'a str,
  input: &mut &'a str,
) -> ModalResult<T> {
  subparse(parser, segment(stop).parse_next(input)?, origin, input)
}

fn schedule(input: &mut &str) -> ModalResult<Schedule> {
  let origin = *input;
  let year = next_segment(field(Field::Year.domain()), |c| c == '-', origin, input)?;
  let _ = cut_err('-').parse_next(input)?;
  let month = next_segment(field(Field::Month.domain()), |c| c == '-', origin, input)?;
  let _ = cut_err('-').parse_next(input)?;
  let day = next_segment(
    field(Field::Day.domain()),
    |c| c == 'T' || c == 't',
    origin,
    input,
  )?;
  let _ = cut_err(one_of(['T', 't'])).parse_next(input)?;
  let hour = next_segment(field(Field::Hour.domain()), |c| c == ':', origin, input)?;
  let _ = cut_err(':').parse_next(input)?;
  let minute = next_segment(field(Field::Minute.domain()), |c| c == ':', origin, input)?;
  let _ = cut_err(':').parse_next(input)?;
  // The second field ends where the time zone declaration starts.
  let second = next_segment(
    field(Field::Second.domain()),
    |c| matches!(c, 'Z' | 'z' | '+' | '-' | '['),
    origin,
    input,
  )?;
  let rest = *input;
  let zone = if rest.is_empty() {
    ZoneSpec::Local
  } else {
    let parsed = subparse(zone_decl, rest, origin, input)?;
    // The zone parser consumed all of `rest` (subparse requires complete
    // consumption); move the outer cursor past it.
    *input = &rest[rest.len()..];
    parsed
  };
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
