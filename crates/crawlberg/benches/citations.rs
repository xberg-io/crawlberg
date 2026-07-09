use std::hint::black_box;

use crawlberg::generate_citations;
use criterion::{Criterion, criterion_group, criterion_main};

fn build_short_md(target_bytes: usize) -> String {
    let fragment = "Visit [example](https://example.com) here.\n";
    let reps = target_bytes.div_ceil(fragment.len()).max(1);
    fragment.repeat(reps)
}

fn build_paren_md(target_bytes: usize) -> String {
    let fragment = "See [Rust](https://en.wikipedia.org/wiki/Rust_(programming_language)) here.\n";
    let reps = target_bytes.div_ceil(fragment.len()).max(1);
    fragment.repeat(reps)
}

fn build_mixed_md(target_bytes: usize) -> String {
    let short = "Visit [example](https://example.com) here.\n";
    let paren = "See [Rust](https://en.wikipedia.org/wiki/Rust_(programming_language)) here.\n";
    let pair = format!("{short}{paren}");
    let reps = target_bytes.div_ceil(pair.len()).max(1);
    pair.repeat(reps)
}

fn bench_citations(c: &mut Criterion) {
    let short_input = build_short_md(64 * 1024);
    let paren_input = build_paren_md(64 * 1024);
    let mixed_input = build_mixed_md(1024 * 1024);

    let mut group = c.benchmark_group("citations");
    group.sample_size(20);

    group.bench_function("short_urls", |b| {
        b.iter(|| generate_citations(black_box(&short_input)));
    });

    group.bench_function("paren_urls", |b| {
        b.iter(|| generate_citations(black_box(&paren_input)));
    });

    group.bench_function("mixed_1mib", |b| {
        b.iter(|| generate_citations(black_box(&mixed_input)));
    });

    group.finish();
}

criterion_group!(benches, bench_citations);
criterion_main!(benches);
