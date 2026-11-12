fn main() {
  let inputs = [
    // canonical & reasonable variations
    "2026-03-08T12:30:45+08:00",
    "2026-1-2T3:4:5",
    "2026-01-01t00:00:00",
    "2026-01-01T00:00:00z",
    "0000-01-01T00:00:00",
    "9999-12-31T23:59:59",
    // lazy: parse ok, never triggers
    "2026-13-01T00:00:00",
    "2026-00-01T00:00:00",
    "2026-01-00T00:00:00",
    "2026-01-01T24:00:00",
    "2026-01-01T00:00:60",
    "2026-02-29T00:00:00", // 2026 is not a leap year
    "2026-01-01T{14-8}:00:00",
    "2026-01-01T00:{0,200}:00",
    "2026-01-01T00:{0-200}:00",
    "2026-01-01T00:{300-100}:00",
    "{2020-99999}-01-01T00:00:00",
    "{70000-80000}-01-01T00:00:00",
    "2026-01-01T00:00:00+99:59",
    // iana names
    "2026-01-01T00:00:00[Asia/Shanghai]",
    "2026-01-01T00:00:00[..]",
    "2026-01-01T00:00:00[/]",
    "2026-01-01T00:00:00[a]",
    // syntax errors
    "2026-01-01 00:00:00",
    " 2026-01-01T00:00:00",
    "2026-01-01T00:00:00 ",
    "2026-01-01T00:00:00.123",
    "2026-01-01T00:00:00+08:00:30",
    "2026-01-01T00:00:00+8:00",
    "2026-01-01T00:00:00+0800",
    "2026-01-01T00:00:00Z+08:00",
    "2026-01-01T00:00:00[]",
    "10000-01-01T00:00:00",
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
        if s.hour.is_empty() || s.minute.is_empty() || s.second.is_empty() {
          notes.push("time set EMPTY -> never triggers");
        }
        if s.month.is_empty() || s.day.is_empty() {
          notes.push("date set EMPTY -> never triggers");
        }
        if s.year.is_empty() {
          notes.push("year set EMPTY -> never triggers");
        }
        if let sched::ZoneSpec::Iana(name) = &s.zone {
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
