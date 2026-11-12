//! Prints how the parser classifies a set of representative inputs.
//!
//! Run with `cargo run --example probe`. This is a manual behavior probe, not
//! part of the library API.

fn main() {
  let inputs = [
    // canonical & reasonable variations
    "2026-03-08T12:30:45+08:00",
    "2026-1-2T3:4:5",
    "2026-01-01t00:00:00",
    "2026-01-01T00:00:00z",
    "9999-12-31T23:59:59",
    "2026-01-01T00:00:00+8:0",
    "2026-01-01T00:00:00-5:3",
    "2026-01-01T00:00:00+25:59",
    "2026-01-01T{8, 12}:00:00",
    "2026-01-01T{8,}:00:00",
    // valid field domains, calendar-impossible dates (parse, never fire)
    "2026-02-29T00:00:00", // 2026 is not a leap year
    "2026-02-30T00:00:00",
    "2026-04-31T00:00:00",
    // IANA names (existence is checked at bind time)
    "2026-01-01T00:00:00[Asia/Shanghai]",
    "2026-01-01T00:00:00[..]",
    "2026-01-01T00:00:00[/]",
    "2026-01-01T00:00:00[a]",
    // field-domain / range / offset errors
    "2026-13-01T00:00:00",
    "2026-00-01T00:00:00",
    "2026-01-00T00:00:00",
    "2026-01-32T00:00:00",
    "2026-01-01T24:00:00",
    "2026-01-01T00:60:00",
    "2026-01-01T00:00:60",
    "2026-01-01T{14-8}:00:00",
    "2026-01-01T00:{0,200}:00",
    "2026-01-01T00:{0-200}:00",
    "2026-01-01T00:{300-100}:00",
    "{2020-99999}-01-01T00:00:00",
    "{70000-80000}-01-01T00:00:00",
    "0000-01-01T00:00:00",
    "10000-01-01T00:00:00",
    "2026-01-01T00:00:00+26:00",
    "2026-01-01T00:00:00+99:59",
    "2026-01-01T00:00:00+00:99",
    // syntax errors
    "2026-01-01 00:00:00",
    " 2026-01-01T00:00:00",
    "2026-01-01T00:00:00 ",
    "2026-01-01T00:00:00.123",
    "2026-01-01T00:00:00+08:00:30",
    "2026-01-01T00:00:00+0800",
    "2026-01-01T00:00:00Z+08:00",
    "2026-01-01T00:00:00[]",
    "2026-01-01T{}:00:00",
    "2026-01-01T{8,}:00:00",
    "2026-01-01T{,8}:00:00",
    "2026-01-01T{8, 12}:00:00",
    "2026-01-01T{1-2-3}:00:00",
    "2026-01-01T{-5}:00:00",
    "２０２６-01-01T00:00:00", // full-width digits
  ];
  for input in inputs {
    let behavior = match sched::parse(input) {
      Ok(s) => {
        let mut notes = vec![];
        if let sched::ZoneSpec::Iana(name) = s.zone() {
          notes.push("iana");
          if name.as_str().chars().any(|c| c == '.') {
            notes.push("dotty name");
          }
        }
        if notes.is_empty() {
          "OK".to_string()
        } else {
          format!("OK ({})", notes.join(", "))
        }
      }
      Err(e) => format!("ERR {e}"),
    };
    println!("{input:<44} => {behavior}");
  }
}
