//! Fuzz-style property tests: the parser must never panic, whatever the
//! input, and must only fail with the documented error kinds.
//!
//! Three input generators are mixed:
//!
//! 1. random strings over a grammar-weighted alphabet,
//! 2. random mutations of a seed corpus of (mostly valid) expressions,
//! 3. grammar-aware structured generation with occasional garbage.
//!
//! The iteration count can be raised via `SCHED_FUZZ_ITERS`.

use std::panic::{AssertUnwindSafe, catch_unwind};

use sched::{ParseError, parse};

struct Rng(u64);

impl Rng {
  fn new(seed: u64) -> Self {
    Self(seed.max(1))
  }

  fn next(&mut self) -> u64 {
    let mut x = self.0;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    self.0 = x;
    x
  }

  fn below(&mut self, n: usize) -> usize {
    (self.next() % n as u64) as usize
  }

  fn chance(&mut self, percent: u64) -> bool {
    self.next() % 100 < percent
  }
}

/// Grammar-heavy alphabet so generated strings often come close to valid.
const CHARSET: &[u8] = b"00112233445566778899-::Tt{},*+[]Zz/.eE _~\x00\xc3\xa9";

const SEEDS: &[&str] = &[
  "2026-03-08T12:30:45+08:00",
  "2026-{*}-{*}T{8,12-14}:00:00",
  "{*}-{*}-{*}T08:00:00+08:00",
  "{*}-{*}-{*}T08:00:00[America/Sao_Paulo]",
  "{2024-2030,2035}-01-01T00:00:00Z",
  "2026-13-01T00:00:00",
  "2026-01-01T{14-8}:00:00",
];

fn random_string(rng: &mut Rng) -> String {
  let len = rng.below(96);
  let mut s = String::with_capacity(len);
  for _ in 0..len {
    let b = CHARSET[rng.below(CHARSET.len())];
    s.push(b as char);
  }
  s
}

fn mutate(rng: &mut Rng) -> String {
  let mut s = SEEDS[rng.below(SEEDS.len())].to_string();
  for _ in 0..=rng.below(4) {
    let pos = rng.below(s.len() + 1);
    // Keep mutations on char boundaries: operate on bytes of an ASCII-heavy
    // corpus and re-check UTF-8 validity below.
    let mut bytes = s.into_bytes();
    match rng.below(4) {
      0 if !bytes.is_empty() => {
        bytes.remove(pos.min(bytes.len() - 1));
      }
      1 => bytes.insert(pos.min(bytes.len()), CHARSET[rng.below(CHARSET.len())]),
      2 if !bytes.is_empty() => {
        let i = pos.min(bytes.len() - 1);
        bytes[i] = CHARSET[rng.below(CHARSET.len())];
      }
      _ => {
        let other = SEEDS[rng.below(SEEDS.len())].as_bytes();
        let take = rng.below(other.len());
        bytes.splice(
          pos.min(bytes.len())..pos.min(bytes.len()),
          other[..take].iter().copied(),
        );
      }
    }
    match String::from_utf8(bytes) {
      Ok(valid) => s = valid,
      Err(err) => s = String::from_utf8_lossy(err.as_bytes()).into_owned(),
    }
  }
  s
}

fn brace_item(rng: &mut Rng, max: u32) -> String {
  let start = rng.below(max as usize) as u32;
  if rng.chance(40) {
    let end = rng.below(max as usize) as u32;
    format!("{start}-{end}")
  } else {
    format!("{start}")
  }
}

fn brace(rng: &mut Rng, max: u32) -> String {
  if rng.chance(20) {
    return "{*}".to_string();
  }
  let items: Vec<String> = (0..=rng.below(4)).map(|_| brace_item(rng, max)).collect();
  format!("{{{}}}", items.join(","))
}

fn structured(rng: &mut Rng) -> String {
  // Small values keep outputs valid; occasional large ones probe the
  // lazy-validation and storage-limit paths.
  let field_max = if rng.chance(10) { 100_000 } else { 130 };
  let field = |rng: &mut Rng| {
    if rng.chance(40) {
      brace(rng, field_max)
    } else {
      format!("{:02}", rng.below(100))
    }
  };
  let year = if rng.chance(30) {
    brace(rng, 100_000)
  } else {
    format!("{:04}", rng.below(10_000))
  };
  let mut s = format!(
    "{}-{}-{}T{}:{}:{}",
    year,
    field(rng),
    field(rng),
    field(rng),
    field(rng),
    field(rng)
  );
  match rng.below(6) {
    0 => {}
    1 => s.push('Z'),
    2 => s.push_str(&format!("+{:02}:{:02}", rng.below(100), rng.below(100))),
    3 => s.push_str(&format!("-{:02}:{:02}", rng.below(100), rng.below(100))),
    4 => {
      let names = [
        "Asia/Shanghai",
        "America/Sao_Paulo",
        "UTC",
        "a",
        "Not/AZone",
        "../etc",
      ];
      s.push_str(&format!("[{}]", names[rng.below(names.len())]));
    }
    _ => s.push_str(&random_string(rng)),
  }
  s
}

#[test]
fn fuzz_parser_never_panics() {
  let iters: usize = std::env::var("SCHED_FUZZ_ITERS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(200_000);
  let mut rng = Rng::new(0x5EED_5EED);

  let mut ok = 0usize;
  let mut syntax = 0usize;
  for i in 0..iters {
    let input = match rng.below(3) {
      0 => random_string(&mut rng),
      1 => mutate(&mut rng),
      _ => structured(&mut rng),
    };
    let result = catch_unwind(AssertUnwindSafe(|| parse(&input)));
    let Ok(result) = result else {
      panic!("parser panicked on iteration {i}, input: {input:?}");
    };
    match result {
      Ok(_) => ok += 1,
      Err(ParseError { .. }) => syntax += 1,
    }
  }
  eprintln!("fuzz: {iters} iters -> {ok} ok, {syntax} syntax errors");
}
