use sched::{ParseError, Schedule, ZoneSpec, parse};

#[test]
fn simple_fixed_datetime_with_offset() {
  let s = parse("2026-03-08T12:30:45+08:00").unwrap();
  assert!(s.year.contains(2026));
  assert!(!s.year.contains(2027));
  assert_eq!(s.month.iter().collect::<Vec<_>>(), [3]);
  assert_eq!(s.day.iter().collect::<Vec<_>>(), [8]);
  assert_eq!(s.hour.iter().collect::<Vec<_>>(), [12]);
  assert_eq!(s.minute.iter().collect::<Vec<_>>(), [30]);
  assert_eq!(s.second.iter().collect::<Vec<_>>(), [45]);
  match s.zone {
    ZoneSpec::Fixed(offset) => {
      assert_eq!(offset.as_seconds(), 8 * 3600);
      assert_eq!(offset.to_string(), "+08:00");
    }
    other => panic!("expected fixed offset, got {other:?}"),
  }
}

#[test]
fn doc_example_year_and_hours() {
  let s = parse("2026-{*}-{*}T{8,12-14}:00:00").unwrap();
  assert!(s.year.contains(2026));
  assert!(!s.year.contains(2025));
  assert_eq!(s.month.len(), 12);
  assert!(s.month.contains(1) && s.month.contains(12));
  assert_eq!(s.day.len(), 31);
  assert_eq!(s.hour.iter().collect::<Vec<_>>(), [8, 12, 13, 14]);
  assert_eq!(s.minute.iter().collect::<Vec<_>>(), [0]);
  assert_eq!(s.second.iter().collect::<Vec<_>>(), [0]);
  assert_eq!(s.zone, ZoneSpec::Local);
}

#[test]
fn doc_example_cst_offset() {
  let s = parse("{*}-{*}-{*}T08:00:00+08:00").unwrap();
  assert!(s.year.contains(1900) && s.year.contains(9999));
  match s.zone {
    ZoneSpec::Fixed(offset) => assert_eq!(offset.as_seconds(), 28800),
    other => panic!("expected fixed offset, got {other:?}"),
  }
}

#[test]
fn doc_example_iana_zone() {
  let s = parse("{*}-{*}-{*}T08:00:00[America/Sao_Paulo]").unwrap();
  match s.zone {
    ZoneSpec::Iana(name) => assert_eq!(name.as_str(), "America/Sao_Paulo"),
    other => panic!("expected IANA zone, got {other:?}"),
  }
}

#[test]
fn zulu_suffix() {
  for input in ["2026-01-01T00:00:00Z", "2026-01-01T00:00:00z"] {
    match parse(input).unwrap().zone {
      ZoneSpec::Fixed(offset) => assert_eq!(offset.as_seconds(), 0),
      other => panic!("expected fixed offset, got {other:?}"),
    }
  }
}

#[test]
fn negative_offset_with_minutes() {
  let s = parse("2026-06-15T09:30:00-05:30").unwrap();
  match s.zone {
    ZoneSpec::Fixed(offset) => {
      assert_eq!(offset.as_seconds(), -(5 * 3600 + 30 * 60));
      assert_eq!(offset.hours(), -5);
      assert_eq!(offset.minutes(), -30);
      assert_eq!(offset.to_string(), "-05:30");
    }
    other => panic!("expected fixed offset, got {other:?}"),
  }
}

#[test]
fn single_digit_fields() {
  let s = parse("2026-1-2T3:4:5").unwrap();
  assert_eq!(s.month.iter().collect::<Vec<_>>(), [1]);
  assert_eq!(s.day.iter().collect::<Vec<_>>(), [2]);
  assert_eq!(s.hour.iter().collect::<Vec<_>>(), [3]);
  assert_eq!(s.minute.iter().collect::<Vec<_>>(), [4]);
  assert_eq!(s.second.iter().collect::<Vec<_>>(), [5]);
}

#[test]
fn year_ranges_and_lists() {
  let s = parse("{2024-2026,2030,2040-2042}-01-01T00:00:00").unwrap();
  assert_eq!(
    s.year.iter().collect::<Vec<_>>(),
    [2024, 2025, 2026, 2030, 2040, 2041, 2042]
  );
  assert!(s.year.contains(2025) && s.year.contains(2030) && s.year.contains(2041));
  assert!(!s.year.contains(2027) && !s.year.contains(2031));
}

// ---------------------------------------------------------------------------
// Lazy validation: invalid-but-representable values parse and never match
// ---------------------------------------------------------------------------

#[test]
fn february_30th_is_a_valid_expression() {
  // Nonexistent calendar dates are skipped at trigger time, not at parse time.
  let s = parse("2026-02-30T00:00:00").unwrap();
  assert_eq!(s.day.iter().collect::<Vec<_>>(), [30]);
}

#[test]
fn out_of_domain_values_parse_but_never_match() {
  // Month 13 is not a real month: parses fine, matches nothing.
  let s = parse("2026-13-01T00:00:00").unwrap();
  assert_eq!(s.month.iter().collect::<Vec<_>>(), [13]);

  // Hours 24 and 25, minutes clamped to the 64-bit bitmap: same story.
  let s = parse("2026-01-01T{24,25}:{45-75}:00").unwrap();
  assert_eq!(s.hour.iter().collect::<Vec<_>>(), [24, 25]);
  assert_eq!(s.minute.min(), Some(45));
  assert_eq!(s.minute.max(), Some(63));
  assert_eq!(s.minute.len(), 19);
}

#[test]
fn inverted_range_expands_to_empty_set() {
  let s = parse("2026-01-01T{14-8}:00:00").unwrap();
  assert!(s.hour.is_empty());
}

#[test]
fn offsets_beyond_jiff_range_parse() {
  // jiff offsets are limited to ±25:59:59; binding is where this gets
  // rejected. Parsing stays lazy.
  for input in ["2026-01-01T00:00:00+25:00", "2026-01-01T00:00:00+99:59"] {
    assert!(parse(input).is_ok(), "input {input:?}");
  }
}

#[test]
fn unrepresentable_values_are_skipped() {
  // 200 exceeds the 64-bit field bitmap; skipping it preserves semantics
  // since it could never match a real minute anyway.
  let s = parse("2026-01-01T00:{0,200}:00").unwrap();
  assert_eq!(s.minute.iter().collect::<Vec<_>>(), [0]);

  // 99999 exceeds the year bitmap; skipped as well.
  let s = parse("{2026,99999}-01-01T00:00:00").unwrap();
  assert!(s.year.contains(2026));
  assert!(!s.year.contains(9999));
  assert_eq!(s.year.len(), 1);
}

#[test]
fn full_bitmap_range() {
  let s = parse("2026-01-01T00:00:{0-99}").unwrap();
  assert_eq!(s.second.len(), 64);
  assert!(s.second.contains(0) && s.second.contains(63));
}

#[test]
fn out_of_storage_ranges_are_clamped_not_skipped() {
  // {0-200} covers every real minute: clamped to the bitmap, not dropped.
  let s = parse("2026-01-01T00:{0-200}:00").unwrap();
  assert!(s.minute.contains(0) && s.minute.contains(59));
  // {58-200} still matches 58 and 59.
  let s = parse("2026-01-01T00:{58-200}:00").unwrap();
  assert_eq!(s.minute.min(), Some(58));
  assert!(s.minute.contains(59));
  // {2020-99999} covers every real year from 2020 on.
  let s = parse("{2020-99999}-01-01T00:00:00").unwrap();
  assert!(s.year.contains(9999));
  // Ranges starting beyond storage never match anything.
  let s = parse("{70000-80000}-01-01T00:00:00").unwrap();
  assert!(s.year.is_empty());
}

#[test]
fn inverted_range_with_unrepresentable_start_stays_empty() {
  // Regression: 300 must not truncate to a u8 (44) and resurrect the range.
  let s = parse("2026-01-01T00:{300-100}:00").unwrap();
  assert!(s.minute.is_empty());
  let s = parse("{99999-1}-01-01T00:00:00").unwrap();
  assert!(!s.year.contains(2026));
}

// ---------------------------------------------------------------------------
// Errors: syntax and fixed-capacity storage only
// ---------------------------------------------------------------------------

#[test]
fn whitespace_and_trailing_comma_in_braces() {
  let s = parse("2026-01-01T{ 8 , 12 - 14 }:00:00").unwrap();
  assert_eq!(s.hour.iter().collect::<Vec<_>>(), [8, 12, 13, 14]);

  let s = parse("2026-01-01T{9,}:00:00").unwrap();
  assert_eq!(s.hour.iter().collect::<Vec<_>>(), [9]);

  let s = parse("2026-01-01T{9 , }:00:00").unwrap();
  assert_eq!(s.hour.iter().collect::<Vec<_>>(), [9]);

  let s = parse("{2026,}-01-01T00:00:00").unwrap();
  assert!(s.year.contains(2026));

  let s = parse("2026-{ * }-{ * }T00:00:00").unwrap();
  assert_eq!(s.month.len(), 12);
  assert_eq!(s.day.len(), 31);

  // Tabs and newlines count as whitespace too.
  let s = parse("2026-01-01T{\t8,\n12\r}:00:00").unwrap();
  assert_eq!(s.hour.iter().collect::<Vec<_>>(), [8, 12]);
}

#[test]
fn empty_list_items_are_rejected() {
  // Every list position requires an item.
  for input in [
    "2026-01-01T{}:00:00",
    "2026-01-01T{ }:00:00",
    "2026-01-01T{,}:00:00",
    "2026-01-01T{,9}:00:00",
    "2026-01-01T{9,,}:00:00",
    "2026-01-01T{9,,8}:00:00",
  ] {
    let err = parse(input).unwrap_err();
    assert!(
      matches!(err, ParseError { .. }),
      "input {input:?}: got {err:?}"
    );
  }
}

#[test]
fn error_offsets_point_at_the_problem() {
  // The month segment `01x` is not a valid field: the error points at the
  // stray character, where content parsing failed.
  assert_eq!(parse("2026-01x-01T00:00:00"), Err(ParseError { offset: 7 }));
  // Missing delimiter after the month.
  assert_eq!(parse("2026-01"), Err(ParseError { offset: 7 }));
  // Trailing garbage after the zone.
  assert_eq!(
    parse("2026-01-01T00:00:00+08:00 extra"),
    Err(ParseError { offset: 25 })
  );
  // A stray character inside a brace expression.
  assert_eq!(
    parse("2026-01-01T{8,x}:00:00"),
    Err(ParseError { offset: 14 })
  );
}

#[test]
fn lenient_offsets() {
  // Offset components accept 1-2 digits, like time fields.
  for (input, seconds) in [
    ("2026-01-01T00:00:00+8:00", 8 * 3600),
    ("2026-01-01T00:00:00+8:0", 8 * 3600),
    ("2026-01-01T00:00:00-5:3", -(5 * 3600 + 3 * 60)),
  ] {
    match parse(input).unwrap().zone {
      ZoneSpec::Fixed(offset) => assert_eq!(offset.as_seconds(), seconds, "input {input:?}"),
      other => panic!("expected fixed offset, got {other:?}"),
    }
  }
}

#[test]
fn syntax_errors() {
  for input in [
    "",
    "2026",
    "2026-01",
    "2026-01-01",
    "2026-01-01T00:00",
    "2026-01-01 00:00:00",             // missing T separator
    "2026-01-01T00:00:00+",            // dangling sign
    "2026-01-01T00:00:00[]",           // empty IANA name
    "2026-01-01T00:00:00+08:00 extra", // trailing garbage
    "2026-01-01T00:00:00+0800",        // missing offset colon
    "2026-01-01T{1-2-3}:00:00",        // double range
    "2026-01-01T{-5}:00:00",           // negative value
    "2026-01-01T00:00:00.123",         // fractional seconds
  ] {
    let err = parse(input).unwrap_err();
    assert!(
      matches!(err, ParseError { .. }),
      "input {input:?}: got {err:?}"
    );
  }
}

#[test]
fn year_lists_have_no_item_limit() {
  // The year bitmap has no item cap: long lists just work.
  let s = parse("{2020,2022,2024,2026,2028,2030,2032,2034,2036,2038}-01-01T00:00:00").unwrap();
  assert_eq!(s.year.len(), 10);
  assert!(s.year.contains(2022) && s.year.contains(2038));
  assert!(!s.year.contains(2021));
}

#[test]
fn from_str_roundtrip() {
  let s: Schedule = "{*}-{*}-{*}T08:00:00[Asia/Shanghai]".parse().unwrap();
  match s.zone {
    ZoneSpec::Iana(name) => assert_eq!(name.as_str(), "Asia/Shanghai"),
    other => panic!("expected IANA zone, got {other:?}"),
  }
}
