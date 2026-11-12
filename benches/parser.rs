use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};

const SIMPLE: &str = "2026-03-08T12:30:45+08:00";
const WILDCARD_IANA: &str = "{*}-{*}-{*}T08:00:00[America/Sao_Paulo]";
const COMPLEX: &str =
  "{2024-2030,2035,2040-2042}-{1,3,5-7}-{1,15,28}T{6,12,18}:{0,15,30,45}:{0,30}-05:30";

fn bench_parse(c: &mut Criterion) {
  let mut group = c.benchmark_group("parse");
  for (name, input) in [
    ("simple", SIMPLE),
    ("wildcard_iana", WILDCARD_IANA),
    ("complex", COMPLEX),
  ] {
    group.bench_function(name, |b| {
      b.iter(|| sched::parse(black_box(input)).unwrap());
    });
  }
  group.finish();
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);
