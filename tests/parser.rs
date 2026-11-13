use sched::{ParseError, Schedule, ZoneSpec, parse};

#[test]
fn simple_fixed_datetime_with_offset() {
  let s = parse("2026-03-08T12:30:45+08:00").unwrap();
  assert!(s.year().contains(2026));
  assert!(!s.year().contains(2027));
  assert_eq!(s.month().iter().collect::<Vec<_>>(), [3]);
  assert_eq!(s.day().iter().collect::<Vec<_>>(), [8]);
  assert_eq!(s.hour().iter().collect::<Vec<_>>(), [12]);
  assert_eq!(s.minute().iter().collect::<Vec<_>>(), [30]);
  assert_eq!(s.second().iter().collect::<Vec<_>>(), [45]);
  match s.zone() {
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
  assert!(s.year().contains(2026));
  assert!(!s.year().contains(2025));
  assert_eq!(s.month().len(), 12);
  assert!(s.month().contains(1) && s.month().contains(12));
  assert_eq!(s.day().len(), 31);
  assert_eq!(s.hour().iter().collect::<Vec<_>>(), [8, 12, 13, 14]);
  assert_eq!(s.minute().iter().collect::<Vec<_>>(), [0]);
  assert_eq!(s.second().iter().collect::<Vec<_>>(), [0]);
  assert_eq!(s.zone(), ZoneSpec::Unspecified);
}

#[test]
fn doc_example_cst_offset() {
  let s = parse("{*}-{*}-{*}T08:00:00+08:00").unwrap();
  assert!(s.year().contains(1900) && s.year().contains(9999));
  match s.zone() {
    ZoneSpec::Fixed(offset) => assert_eq!(offset.as_seconds(), 28800),
    other => panic!("expected fixed offset, got {other:?}"),
  }
}

#[test]
fn doc_example_iana_zone() {
  let s = parse("{*}-{*}-{*}T08:00:00[America/Sao_Paulo]").unwrap();
  match s.zone() {
    ZoneSpec::Iana(name) => assert_eq!(name.as_str(), "America/Sao_Paulo"),
    other => panic!("expected IANA zone, got {other:?}"),
  }
}

#[test]
fn zulu_suffix() {
  for input in ["2026-01-01T00:00:00Z", "2026-01-01T00:00:00z"] {
    match parse(input).unwrap().zone() {
      ZoneSpec::Fixed(offset) => assert_eq!(offset.as_seconds(), 0),
      other => panic!("expected fixed offset, got {other:?}"),
    }
  }
}

#[test]
fn negative_offset_with_minutes() {
  let s = parse("2026-06-15T09:30:00-05:30").unwrap();
  match s.zone() {
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
  assert_eq!(s.month().iter().collect::<Vec<_>>(), [1]);
  assert_eq!(s.day().iter().collect::<Vec<_>>(), [2]);
  assert_eq!(s.hour().iter().collect::<Vec<_>>(), [3]);
  assert_eq!(s.minute().iter().collect::<Vec<_>>(), [4]);
  assert_eq!(s.second().iter().collect::<Vec<_>>(), [5]);
}

#[test]
fn year_ranges_and_lists() {
  let s = parse("{2024-2026,2030,2040-2042}-01-01T00:00:00").unwrap();
  assert_eq!(
    s.year().iter().collect::<Vec<_>>(),
    [2024, 2025, 2026, 2030, 2040, 2041, 2042]
  );
  assert!(s.year().contains(2025) && s.year().contains(2030) && s.year().contains(2041));
  assert!(!s.year().contains(2027) && !s.year().contains(2031));
}

// ---------------------------------------------------------------------------
// Strict validation: field domains, ranges, and offsets are checked at parse
// ---------------------------------------------------------------------------

#[test]
fn february_30th_is_a_valid_expression() {
  // Day 30 is inside the day domain; whether the calendar has it is checked
  // when computing trigger times.
  let s = parse("2026-02-30T00:00:00").unwrap();
  assert_eq!(s.day().iter().collect::<Vec<_>>(), [30]);
}

#[test]
fn out_of_domain_values_are_rejected() {
  for input in [
    "2026-13-01T00:00:00",
    "2026-00-01T00:00:00",
    "2026-01-00T00:00:00",
    "2026-01-32T00:00:00",
    "2026-01-01T24:00:00",
    "2026-01-01T00:60:00",
    "2026-01-01T00:00:60",
    "2026-01-01T{24,25}:00:00",
    "2026-01-01T00:{45-75}:00",
    "0000-01-01T00:00:00",
    "10000-01-01T00:00:00",
  ] {
    assert!(parse(input).is_err(), "input {input:?} should be rejected");
  }
}

#[test]
fn inverted_ranges_are_rejected() {
  for input in [
    "2026-01-01T{14-8}:00:00",
    "2026-01-01T00:{300-100}:00",
    "{99999-1}-01-01T00:00:00",
  ] {
    assert!(parse(input).is_err(), "input {input:?} should be rejected");
  }
}

#[test]
fn invalid_offsets_are_rejected() {
  for input in [
    "2026-01-01T00:00:00+26:00",
    "2026-01-01T00:00:00+25:60",
    "2026-01-01T00:00:00+99:59",
    "2026-01-01T00:00:00+00:60",
    "2026-01-01T00:00:00+00:99",
    "2026-01-01T00:00:00-26:00",
  ] {
    assert!(parse(input).is_err(), "input {input:?} should be rejected");
  }
  // The boundary is inclusive: +25:59 is jiff's maximum whole-minute offset.
  assert!(parse("2026-01-01T00:00:00+25:59").is_ok());
}

#[test]
fn values_outside_field_domains_are_rejected() {
  for input in [
    "2026-01-01T00:{0,200}:00",
    "{2026,99999}-01-01T00:00:00",
    "2026-01-01T00:{0-200}:00",
    "2026-01-01T00:{58-200}:00",
    "{2020-99999}-01-01T00:00:00",
    "{70000-80000}-01-01T00:00:00",
  ] {
    assert!(parse(input).is_err(), "input {input:?} should be rejected");
  }
}

#[test]
fn full_bitmap_range() {
  let s = parse("2026-01-01T00:00:{0-59}").unwrap();
  assert_eq!(s.second().len(), 60);
  assert!(s.second().contains(0) && s.second().contains(59));
}

// ---------------------------------------------------------------------------
// Errors: syntax, field domains, and ranges
// ---------------------------------------------------------------------------

#[test]
fn whitespace_and_trailing_comma_in_braces() {
  let s = parse("2026-01-01T{ 8 , 12 - 14 }:00:00").unwrap();
  assert_eq!(s.hour().iter().collect::<Vec<_>>(), [8, 12, 13, 14]);

  let s = parse("2026-01-01T{9,}:00:00").unwrap();
  assert_eq!(s.hour().iter().collect::<Vec<_>>(), [9]);

  let s = parse("2026-01-01T{9 , }:00:00").unwrap();
  assert_eq!(s.hour().iter().collect::<Vec<_>>(), [9]);

  let s = parse("{2026,}-01-01T00:00:00").unwrap();
  assert!(s.year().contains(2026));

  let s = parse("2026-{ * }-{ * }T00:00:00").unwrap();
  assert_eq!(s.month().len(), 12);
  assert_eq!(s.day().len(), 31);

  // Tabs and newlines count as whitespace too.
  let s = parse("2026-01-01T{\t8,\n12\r}:00:00").unwrap();
  assert_eq!(s.hour().iter().collect::<Vec<_>>(), [8, 12]);
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
  // The month field `01x` is not valid: the error points at the stray
  // character, where the field parse stopped.
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
    match parse(input).unwrap().zone() {
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
  assert_eq!(s.year().len(), 10);
  assert!(s.year().contains(2022) && s.year().contains(2038));
  assert!(!s.year().contains(2021));
}

#[test]
fn from_str_roundtrip() {
  let s: Schedule = "{*}-{*}-{*}T08:00:00[Asia/Shanghai]".parse().unwrap();
  match s.zone() {
    ZoneSpec::Iana(name) => assert_eq!(name.as_str(), "Asia/Shanghai"),
    other => panic!("expected IANA zone, got {other:?}"),
  }
}

#[test]
fn display_is_canonical() {
  let schedule: Schedule = "2026-3-8T3:4:5".parse().unwrap();
  assert_eq!(schedule.to_string(), "2026-03-08T03:04:05");

  let schedule: Schedule = "{2024-2026,2030}-{1,3}-01T00:00:00Z".parse().unwrap();
  assert_eq!(
    schedule.to_string(),
    "{2024-2026,2030}-{01,03}-01T00:00:00+00:00"
  );
}

#[test]
fn display_roundtrips() {
  for input in [
    "2026-03-08T12:30:45+08:00",
    "2026-{*}-{*}T{8,12-14}:00:00",
    "{*}-{*}-{*}T08:00:00[America/Sao_Paulo]",
    "{2024-2026,2030,2040-2042}-01-01T00:00:00Z",
    "2026-1-2T3:4:5",
    "{*}-{*}-{*}T{*}:{*}:{*}",
    "2026-01-01T00:00:00-05:30",
  ] {
    let schedule = parse(input).unwrap();
    let rendered = schedule.to_string();
    let reparsed: Schedule = rendered
      .parse()
      .unwrap_or_else(|error| panic!("{input:?} rendered as {rendered:?}: {error}"));
    assert_eq!(
      reparsed, schedule,
      "input {input:?} rendered as {rendered:?}"
    );
  }
}
