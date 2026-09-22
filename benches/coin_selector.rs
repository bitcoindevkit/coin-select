//! Benchmarks for `CoinSelector`.
//!
//! - `clone`: direct cost of `CoinSelector::clone()`, the operation `Bitset`
//!   was introduced to make cheap.
//! - `run_bnb_lowest_fee`: end-to-end Branch-and-Bound throughput on a
//!   deterministic synthetic pool using the `LowestFee` metric.
//! - `run_bnb_lowest_fee_ancestors`: the same, but where a third of the coins sit on unconfirmed
//!   ancestors that need bumping — covering both the private and shared ancestor paths, which cost
//!   different amounts per fee calculation.
//! - `new_with_ancestors`: building a `SelectionProblem` with ancestors and a `CoinSelector` over it
//!   at wallet-to-exchange pool sizes. Setup has to scale with the ancestor entries, not candidates
//!   times ancestors, or a large pool loses its time budget before the search starts.
//!
//! Run with `cargo bench`. Filter with `cargo bench -- <pattern>`.

// Benchmarks are dev-only and are never built under the MSRV (the `build-msrv` CI job excludes
// dev-dependencies), so lints about newer std APIs — e.g. `black_box`, stable since 1.66 — don't
// apply here.
#![allow(clippy::incompatible_msrv)]

use bdk_coin_select::{
    metrics::LowestFee, AncestorToBump, Candidate, CoinSelector, DrainWeights, FeeRate, Input,
    SelectionProblem, Target, TargetFee, TargetOutputs, TR_SPK_WEIGHT, TXIN_BASE_WEIGHT,
    TXOUT_BASE_WEIGHT,
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
        let (target, _) = make_bnb_inputs(&candidates);
        let problem = SelectionProblem::new_no_ancestors(target, candidates.iter().copied());
        let mut selector = CoinSelector::new(&problem);
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
        let (target, long_term_feerate) = make_bnb_inputs(&candidates);
        let problem = SelectionProblem::new_no_ancestors(target, candidates.iter().copied());
        let selector = CoinSelector::new(&problem);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched(
                || selector.clone(),
                |mut sel| {
                    let metric = LowestFee {
                        long_term_feerate,
                        dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
                        drain_weights: DrainWeights::TR_KEYSPEND,
                    };
                    let _ = sel.run_bnb(metric, black_box(100_000));
                    sel
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// A third of the coins sit on a two-long unconfirmed chain: an unpaid parent and a tip that pays a
/// little. Without `share`, every such coin has a chain of its own (private ancestors). With
/// `share`, they all sit on the *same* chain, which is the case that cannot be folded into the
/// candidates up front and has to be de-duplicated per selection.
fn make_ancestor_problem(n: usize, share: bool) -> SelectionProblem {
    const P2WPKH_SAT_W: u64 = 107;
    const CONFIRMED: usize = usize::MAX;

    let mut ancestors = Vec::new();
    let mut residing = Vec::with_capacity(n);
    let mut shared_tip = None;
    for i in 0..n {
        if i % 3 != 0 {
            residing.push(CONFIRMED);
            continue;
        }
        match (share, shared_tip) {
            (true, Some(tip)) => residing.push(tip),
            _ => {
                let parent = ancestors.len();
                ancestors.push(AncestorToBump {
                    txid: parent,
                    weight: 800,
                    fee: 0,
                    parents: vec![],
                });
                let tip = ancestors.len();
                ancestors.push(AncestorToBump {
                    txid: tip,
                    weight: 800,
                    fee: 200,
                    parents: vec![parent],
                });
                residing.push(tip);
                shared_tip = Some(tip);
            }
        }
    }

    let value = |i: usize| 1_000 + i as u64 * 137 + (i * i) as u64;
    let inputs = (0..n).map(|i| Input {
        value: value(i),
        weight: TXIN_BASE_WEIGHT + P2WPKH_SAT_W,
        residing_txid: residing[i],
    });
    let total: u64 = (0..n).map(value).sum();
    let target = Target {
        fee: TargetFee::from_feerate(FeeRate::from_sat_per_vb(2.0)),
        outputs: TargetOutputs::fund_outputs([(TXOUT_BASE_WEIGHT + TR_SPK_WEIGHT, total / 2)]),
        max_weight: None,
    };
    SelectionProblem::new(target, inputs, ancestors)
}

fn bench_run_bnb_lowest_fee_ancestors(c: &mut Criterion) {
    let mut group = c.benchmark_group("run_bnb_lowest_fee_ancestors");
    group.sample_size(20);
    for &share in &[false, true] {
        let kind = match share {
            false => "private",
            true => "shared",
        };
        for &n in &[20usize, 50, 100] {
            let problem = make_ancestor_problem(n, share);
            let selector = CoinSelector::new(&problem);
            group.bench_with_input(BenchmarkId::new(kind, n), &n, |b, _| {
                b.iter_batched(
                    || selector.clone(),
                    |mut sel| {
                        let metric = LowestFee {
                            long_term_feerate: FeeRate::from_sat_per_vb(10.0),
                            dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
                            drain_weights: DrainWeights::TR_KEYSPEND,
                        };
                        let _ = sel.run_bnb(metric, black_box(100_000));
                        sel
                    },
                    BatchSize::SmallInput,
                );
            });
        }
    }
    group.finish();
}

fn bench_new_with_ancestors(c: &mut Criterion) {
    let mut group = c.benchmark_group("new_with_ancestors");
    group.sample_size(10);
    for &n in &[20_000usize, 200_000] {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            b.iter(|| {
                let problem = make_ancestor_problem(black_box(n), false);
                let selector = CoinSelector::new(&problem);
                black_box(selector.ancestor_bump_lower_bound());
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_coin_selector_clone,
    bench_run_bnb_lowest_fee,
    bench_run_bnb_lowest_fee_ancestors,
    bench_new_with_ancestors
);
criterion_main!(benches);
