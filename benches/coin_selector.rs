//! Benchmarks for `CoinSelector`.
//!
//! Two groups:
//! - `clone`: direct cost of `CoinSelector::clone()`, the operation `Bitset`
//!   was introduced to make cheap.
//! - `run_bnb_lowest_fee`: end-to-end Branch-and-Bound throughput on a
//!   deterministic synthetic pool using the `LowestFee` metric.
//!
//! Run with `cargo bench`. Filter with `cargo bench -- <pattern>`.

// Benchmarks are dev-only and are never built under the MSRV (the `build-msrv` CI job excludes
// dev-dependencies), so lints about newer std APIs — e.g. `black_box`, stable since 1.66 — don't
// apply here.
#![allow(clippy::incompatible_msrv)]

use bdk_coin_select::{
    metrics::LowestFee, Candidate, CoinSelector, DrainWeights, FeeRate, Target, TargetFee,
    TargetOutputs, TR_SPK_WEIGHT, TXIN_BASE_WEIGHT, TXOUT_BASE_WEIGHT,
};
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::hint::black_box;

/// Deterministic synthetic pool of P2WPKH-shaped UTXOs.
///
/// Values grow super-linearly so the pool resembles a real wallet's mix of
/// small/medium/large UTXOs rather than uniform values.
fn make_candidates(n: usize) -> Vec<Candidate> {
    const P2WPKH_SAT_W: u64 = 107;
    (0..n)
        .map(|i| {
            let i = i as u64;
            let value = 1_000 + i * 137 + i * i;
            Candidate {
                value,
                weight: TXIN_BASE_WEIGHT + P2WPKH_SAT_W,
                input_count: 1,
                is_segwit: true,
            }
        })
        .collect()
}

fn make_bnb_inputs(candidates: &[Candidate]) -> (Target, FeeRate) {
    let target_fr = FeeRate::from_sat_per_vb(2.0);
    let long_term_fr = FeeRate::from_sat_per_vb(10.0);
    let total: u64 = candidates.iter().map(|c| c.value).sum();
    let target = Target {
        fee: TargetFee::from_feerate(target_fr),
        outputs: TargetOutputs::fund_outputs([(TXOUT_BASE_WEIGHT + TR_SPK_WEIGHT, total / 2)]),
        max_weight: None,
    };
    (target, long_term_fr)
}

fn bench_coin_selector_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("clone");
    for &n in &[64usize, 256, 1024, 4096] {
        let candidates = make_candidates(n);
        let mut selector = CoinSelector::new(&candidates);
        // Select ~10% of candidates so `selected` is non-trivial to copy.
        for i in (0..n).step_by(10) {
            selector.select(i);
        }
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| black_box(selector.clone()));
        });
    }
    group.finish();
}

fn bench_run_bnb_lowest_fee(c: &mut Criterion) {
    let mut group = c.benchmark_group("run_bnb_lowest_fee");
    // Cap iterations so the largest case fits in a benchmark sample.
    group.sample_size(20);
    for &n in &[20usize, 50, 100, 200] {
        let candidates = make_candidates(n);
        let selector = CoinSelector::new(&candidates);
        let (target, long_term_feerate) = make_bnb_inputs(&candidates);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched(
                || selector.clone(),
                |mut sel| {
                    let metric = LowestFee {
                        long_term_feerate,
                        dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
                        drain_weights: DrainWeights::TR_KEYSPEND,
                    };
                    let _ = sel.run_bnb(target, metric, black_box(100_000));
                    sel
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_coin_selector_clone, bench_run_bnb_lowest_fee);
criterion_main!(benches);
