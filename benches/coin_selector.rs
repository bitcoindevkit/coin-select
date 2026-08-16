//! Benchmarks for `CoinSelector`.
//!
//! Groups include selector construction and cloning, cached-view construction, and end-to-end BnB
//! with and without ancestors. Linear operations cover wallet (~1k) through exchange (~10M) pools;
//! BnB sizes remain moderate because its search space is exponential.
//!
//! - `clone`: direct cost of `CoinSelector::clone()`, the operation `Bitset`
//!   was introduced to make cheap.
//! - `run_bnb_lowest_fee`: end-to-end Branch-and-Bound solution finding on a deterministic
//!   synthetic pool using the `LowestFee` metric.
//! - `run_bnb_lowest_fee_exhaust_cap`: large-pool BnB under the same fixed round cap. DFS still
//!   produces a solution at these sizes; the cap mainly limits how long we spend proving it.
//! - `run_bnb_lowest_fee_ancestors`: the same, but where the coins sit on unconfirmed ancestors that
//!   need bumping — covering both the private and shared ancestor paths, which cost different
//!   amounts per fee calculation.
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

const LARGE_N: &[usize] = &[64, 1_024, 16_384, 262_144, 1_048_576, 10_000_000];
const SPARSE_SELECTED: usize = 100;

/// Deterministic synthetic pool of P2WPKH-shaped UTXOs.
///
/// Values grow super-linearly so the pool resembles a real wallet's mix of
/// small/medium/large UTXOs rather than uniform values.
fn make_candidates(n: usize) -> Vec<Candidate> {
    const P2WPKH_SAT_W: u64 = 107;
    (0..n)
        .map(|i| {
            let i = i as u64;
            let value = 1_000 + i.wrapping_mul(137).wrapping_add(i.wrapping_mul(i));
            Candidate {
                value,
                weight: TXIN_BASE_WEIGHT + P2WPKH_SAT_W,
                segwit_count: 1,
                legacy_count: 0,
            }
        })
        .collect()
}

fn select_sparse(selector: &mut CoinSelector<'_>, n: usize) {
    let count = SPARSE_SELECTED.min(n);
    let stride = (n / count.max(1)).max(1);
    for index in (0..n).step_by(stride).take(count) {
        selector.select(index);
    }
}

fn bench_coin_selector_new(c: &mut Criterion) {
    let mut group = c.benchmark_group("new");
    group.sample_size(20);
    for &n in LARGE_N {
        let candidates = make_candidates(n);
        let (target, _) = make_bnb_inputs(&candidates);
        let problem = SelectionProblem::new_no_ancestors(target, candidates.iter().copied());
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| black_box(CoinSelector::new(&problem)));
        });
    }
    group.finish();
}

fn make_bnb_inputs(candidates: &[Candidate]) -> (Target, FeeRate) {
    let target_fr = FeeRate::from_sat_per_vb(2.0);
    let long_term_fr = FeeRate::from_sat_per_vb(10.0);
    let total = candidates
        .iter()
        .fold(0_u64, |sum, candidate| sum.wrapping_add(candidate.value));
    let target = Target {
        fee: TargetFee::from_feerate(target_fr),
        outputs: TargetOutputs::fund_outputs([(TXOUT_BASE_WEIGHT + TR_SPK_WEIGHT, total / 2)]),
        max_weight: None,
    };
    (target, long_term_fr)
}

fn bench_coin_selector_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("clone");
    group.sample_size(20);
    for &n in LARGE_N {
        let candidates = make_candidates(n);
        let (target, _) = make_bnb_inputs(&candidates);
        let problem = SelectionProblem::new_no_ancestors(target, candidates.iter().copied());
        let mut selector = CoinSelector::new(&problem);
        select_sparse(&mut selector, n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| black_box(selector.clone()));
        });
    }
    group.finish();
}

fn bench_compute_view(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_view");
    group.sample_size(20);
    for &n in LARGE_N {
        let candidates = make_candidates(n);
        let (target, _) = make_bnb_inputs(&candidates);
        let problem = SelectionProblem::new_no_ancestors(target, candidates.iter().copied());
        let mut selector = CoinSelector::new(&problem);
        select_sparse(&mut selector, n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| black_box(selector.compute_view().selected_value()));
        });
    }
    group.finish();
}

const MAX_ROUNDS: usize = 100_000;

fn bench_run_bnb_lowest_fee_sizes(
    c: &mut Criterion,
    group_name: &str,
    sizes: &[usize],
    expect_solution: bool,
) {
    let mut group = c.benchmark_group(group_name);
    group.sample_size(10);
    for &n in sizes {
        let candidates = make_candidates(n);
        let (target, long_term_feerate) = make_bnb_inputs(&candidates);
        let problem = SelectionProblem::new_no_ancestors(target, candidates.iter().copied());
        let selector = CoinSelector::new(&problem);
        let metric = || LowestFee {
            long_term_feerate,
            dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
            drain_weights: DrainWeights::TR_KEYSPEND,
        };
        assert_eq!(
            selector.clone().run_bnb(metric(), MAX_ROUNDS).is_ok(),
            expect_solution,
            "{}/{} changed search path",
            group_name,
            n,
        );
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched(
                || selector.clone(),
                |mut sel| {
                    let _ = sel.run_bnb(metric(), black_box(MAX_ROUNDS));
                    sel
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_run_bnb_lowest_fee(c: &mut Criterion) {
    bench_run_bnb_lowest_fee_sizes(c, "run_bnb_lowest_fee", &[20, 50, 100], true);
}

fn bench_run_bnb_lowest_fee_exhaust_cap(c: &mut Criterion) {
    bench_run_bnb_lowest_fee_sizes(
        c,
        "run_bnb_lowest_fee_exhaust_cap",
        &[200, 500, 1_000],
        true,
    );
}

/// Deterministic synthetic pool where every third coin sits on an unconfirmed chain that still owes
/// fees, so every fee calculation has to work out the bump.
///
/// With `share`, all such coins sit on the *same* chain, which is the case that cannot be folded into
/// the candidates up front and has to be de-duplicated per selection.
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
                // A two-long chain: an unpaid parent and a tip that pays a little.
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

    let inputs = (0..n).map(|i| {
        let value = 1_000 + i as u64 * 137 + (i * i) as u64;
        Input {
            value,
            weight: TXIN_BASE_WEIGHT + P2WPKH_SAT_W,
            is_segwit: true,
            residing_txid: residing[i],
        }
    });

    let total: u64 = (0..n)
        .map(|i| 1_000 + i as u64 * 137 + (i * i) as u64)
        .sum();
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

criterion_group!(
    benches,
    bench_coin_selector_new,
    bench_coin_selector_clone,
    bench_compute_view,
    bench_run_bnb_lowest_fee,
    bench_run_bnb_lowest_fee_exhaust_cap,
    bench_run_bnb_lowest_fee_ancestors
);
criterion_main!(benches);
