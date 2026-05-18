//! Benchmarks for `CoinSelector`.
//!
//! Groups:
//! - `new`: cost of `CoinSelector::new(candidates)` — allocations grow with
//!   the candidate pool size. Bounds the cost of standing up a selector.
//! - `clone`: cost of `CoinSelector::clone()`. The per-branch cost of BnB
//!   exploration is dominated by this.
//! - `compute_view`: cost of `CoinSelector::compute_view()` — walks the
//!   selected bitset to build the cached aggregates. Scales with |selected|.
//! - `run_bnb_lowest_fee`: end-to-end BnB solution-finding on a deterministic
//!   synthetic pool using the `LowestFee` metric, at sizes where the search
//!   converges to a solution within the round cap.
//! - `run_bnb_lowest_fee_exhaust_cap`: the same search at sizes where
//!   best-first exploration does NOT complete any target-meeting selection
//!   within the cap — every sample runs exactly `MAX_ROUNDS` rounds of
//!   frontier expansion (`bound()` + branch cloning), which is precisely the
//!   hot path the delta-aware cache optimizes. BnB's search space is
//!   exponential in pool size, so sizes stay moderate (BnB at 10M candidates
//!   would take eons; real callers pre-filter / pre-group).
//!
//! Pool sizes target the spectrum from wallets (~1k UTXOs) to exchanges
//! (~10M UTXOs). At the high end, this allocates hundreds of MB — adjust the
//! `LARGE_N` list if your machine can't fit.
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

/// Pool sizes for the O(n)-ish operations (new, clone, compute_view).
///
/// 1_024 ~ typical wallet, 1_048_576 ~ small exchange, 10_000_000 ~ very large
/// exchange. The 10M case allocates ~320MB just for the `Candidate` slice and
/// ~80MB for the selector's `candidate_order`; comment out if running on a
/// memory-constrained host.
const LARGE_N: &[usize] = &[64, 1_024, 16_384, 262_144, 1_048_576, 10_000_000];

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

/// Number of selected candidates to use as a representative "sparse"
/// selection (real wallets/exchanges typically select 1–100 UTXOs even from a
/// huge pool).
const SPARSE_SELECTED: usize = 100;

fn select_sparse(selector: &mut CoinSelector<'_>, n: usize) {
    let count = SPARSE_SELECTED.min(n);
    if count == 0 {
        return;
    }
    let stride = (n / count).max(1);
    for i in (0..n).step_by(stride).take(count) {
        selector.select(i);
    }
}

fn bench_new(c: &mut Criterion) {
    let mut group = c.benchmark_group("new");
    group.sample_size(20);
    for &n in LARGE_N {
        let candidates = make_candidates(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| black_box(CoinSelector::new(&candidates)));
        });
    }
    group.finish();
}

fn bench_coin_selector_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("clone");
    group.sample_size(20);
    for &n in LARGE_N {
        let candidates = make_candidates(n);
        let mut selector = CoinSelector::new(&candidates);
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
        let mut selector = CoinSelector::new(&candidates);
        select_sparse(&mut selector, n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let view = selector.compute_view();
                black_box(view.selected_value())
            });
        });
    }
    group.finish();
}

/// Round cap for the BnB benches. Bounds the per-sample cost; at the
/// `exhaust_cap` sizes every sample runs exactly this many rounds.
const MAX_ROUNDS: usize = 100_000;

fn bnb_lowest_fee_metric(long_term_feerate: FeeRate) -> LowestFee {
    LowestFee {
        long_term_feerate,
        dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
        drain_weights: DrainWeights::TR_KEYSPEND,
    }
}

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
        let selector = CoinSelector::new(&candidates);
        let (target, long_term_feerate) = make_bnb_inputs(&candidates);

        // Pin what this group measures: if search dynamics change (metric,
        // bound tightness, candidate distribution), a size silently flipping
        // between the solution-finding and cap-exhaustion paths would corrupt
        // cross-version comparisons — fail loudly instead.
        let found = selector
            .clone()
            .run_bnb(target, bnb_lowest_fee_metric(long_term_feerate), MAX_ROUNDS)
            .is_ok();
        assert_eq!(
            found, expect_solution,
            "{}/{}: expected run_bnb solution-found == {}",
            group_name, n, expect_solution,
        );

        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter_batched(
                || selector.clone(),
                |mut sel| {
                    let metric = bnb_lowest_fee_metric(long_term_feerate);
                    let _ = sel.run_bnb(target, metric, black_box(MAX_ROUNDS));
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
        &[200, 500, 1000],
        false,
    );
}

criterion_group!(
    benches,
    bench_new,
    bench_coin_selector_clone,
    bench_compute_view,
    bench_run_bnb_lowest_fee,
    bench_run_bnb_lowest_fee_exhaust_cap
);
criterion_main!(benches);
