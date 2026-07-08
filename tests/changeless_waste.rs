#![allow(unused_imports)]

mod common;
use bdk_coin_select::metrics::ChangelessWaste;
use bdk_coin_select::{
    BnbMetric, Candidate, CoinSelector, Drain, DrainWeights, FeeRate, Replace, Target, TargetFee,
    TargetOutputs, TX_FIXED_FIELD_WEIGHT,
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        ..Default::default()
    })]

    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn can_eventually_find_best_solution(
        n_candidates in 1..15_usize,
        target_value in 500..500_000_u64,
        n_target_outputs in 1usize..150,
        target_weight in 0..10_000_u32,
        replace in common::maybe_replace(0u64..10_000),
        feerate in 1.0..100.0_f32,
        feerate_lt_diff in -5.0..50.0_f32,
        drain_weight in 100..=500_u32,
        drain_spend_weight in 1..=2000_u32,
        drain_dust in 100..=1000_u64,
        n_drain_outputs in 1usize..150,
        max_weight in common::maybe_max_weight(500u64..4_000), // optional max tx weight cap (wu)
        absolute in 0u64..20_000,
    ) {
        let params = common::StrategyParams { n_candidates, target_value, n_target_outputs, target_weight, replace, feerate, feerate_lt_diff, drain_weight, drain_spend_weight, drain_dust, n_drain_outputs, max_weight, absolute };
        let candidates = common::gen_candidates(params.n_candidates);
        let metric = ChangelessWaste {
            long_term_feerate: params.long_term_feerate(),
            dust_relay_feerate: params.dust_relay_feerate(),
            drain_weights: params.drain_weights(),
        };
        common::can_eventually_find_best_solution(params, candidates, metric)?;
    }

    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn ensure_bound_is_not_too_tight(
        n_candidates in 0..12_usize,
        target_value in 500..500_000_u64,
        n_target_outputs in 1usize..150,
        target_weight in 0..10_000_u32,
        replace in common::maybe_replace(0u64..10_000),
        feerate in 1.0..100.0_f32,
        feerate_lt_diff in -5.0..50.0_f32,
        drain_weight in 100..=500_u32,
        drain_spend_weight in 1..=2000_u32,
        drain_dust in 100..=1000_u64,
        n_drain_outputs in 1usize..150,
        max_weight in common::maybe_max_weight(500u64..4_000), // optional max tx weight cap (wu)
        absolute in 0u64..20_000,
    ) {
        let params = common::StrategyParams { n_candidates, target_value, n_target_outputs, target_weight, replace, feerate, feerate_lt_diff, drain_weight, drain_spend_weight, drain_dust, n_drain_outputs, max_weight, absolute };
        let candidates = common::gen_candidates(params.n_candidates);
        let metric = ChangelessWaste {
            long_term_feerate: params.long_term_feerate(),
            dust_relay_feerate: params.dust_relay_feerate(),
            drain_weights: params.drain_weights(),
        };
        common::ensure_bound_is_not_too_tight(params, candidates, metric)?;
    }
}

/// Regression for the review finding: with a binding ABSOLUTE fee (so `excess` is bound by
/// `absolute_excess`, not `rate_excess`) and `rate_diff < 0`, the `ub_changeless_input_weight`
/// knapsack must not credit rate-based `effective_value` against an absolute-driven delta — doing
/// so over-removes weight and yields a too-tight (invalid) upper bound.
///
/// The candidates are deliberately HIGH-WEIGHT relative to their value (so
/// `effective_value(rate) ≪ value`, opening the gap the bug lives in) — the default random pool
/// never generates these, which is why the proptest missed it. Low feerate + high absolute makes
/// `absolute_excess` the binding constraint, and mixed values let a subset land `absolute_excess`
/// inside the narrow changeless-and-target-met window with high `input_weight`.
#[test]
fn bound_is_valid_with_binding_absolute_fee() {
    // rate feerate = 1 sat/vb (0.25 sat/wu); ev(rate) = value - weight*0.25.
    let mut candidates: Vec<Candidate> = Vec::new();
    for _ in 0..8 {
        candidates.push(Candidate {
            value: 5_000,
            weight: 10_000, // ev(rate) = 5000 - 2500 = 2500  (half the value)
            input_count: 1,
            is_segwit: false,
        });
    }
    for _ in 0..2 {
        candidates.push(Candidate {
            value: 1_000,
            weight: 2_000, // ev(rate) = 1000 - 500 = 500
            input_count: 1,
            is_segwit: false,
        });
    }

    let params = common::StrategyParams {
        n_candidates: candidates.len(),
        target_value: 5_000,
        n_target_outputs: 1,
        target_weight: 0,
        replace: None,
        feerate: 1.0,
        feerate_lt_diff: 9.0, // long_term_feerate = 10 > feerate = 1 (rate_diff < 0)
        drain_weight: 200,
        drain_spend_weight: 600,
        drain_dust: 200,
        n_drain_outputs: 1,
        max_weight: None,
        absolute: 27_000, // > rate-implied fee at d_all, so absolute_excess binds
    };
    let metric = ChangelessWaste {
        long_term_feerate: params.long_term_feerate(),
        dust_relay_feerate: params.dust_relay_feerate(),
        drain_weights: params.drain_weights(),
    };
    common::ensure_bound_is_not_too_tight(params, candidates, metric).unwrap();
}

fn target(value: u64, rate_sat_vb: f32) -> Target {
    Target {
        fee: TargetFee {
            rate: FeeRate::from_sat_per_vb(rate_sat_vb),
            absolute: 0,
            replace: None,
        },
        outputs: TargetOutputs {
            value_sum: value,
            weight_sum: 100,
            n_outputs: 1,
        },
        max_weight: None,
    }
}

const DRAIN_WEIGHTS: DrainWeights = DrainWeights {
    output_weight: 100,
    spend_weight: 600,
    n_outputs: 1,
};

fn metric(long_term_sat_vb: f32) -> ChangelessWaste {
    ChangelessWaste {
        long_term_feerate: FeeRate::from_sat_per_vb(long_term_sat_vb),
        dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
        drain_weights: DRAIN_WEIGHTS,
    }
}

/// Regression (review finding #3): the rate_diff > 0 resize lower bound includes the greedy prefix's segwit
/// corrections (+2 witness header, +1 per legacy input), which an all-legacy descendant avoids.
#[test]
fn bound_is_valid_with_segwit_prefix_corrections() {
    // feerate 100 sat/vb (25 spwu), long-term 1 sat/vb -> rate_diff = 24.75 spwu.
    let rate = FeeRate::from_sat_per_vb(100.0);
    let candidates = vec![
        // SW: same raw weight as L1/L2 but 10 more sats -> best value_pwu, walked first by the
        // greedy prefix, which then carries +2 (header) +1 (L1's witness-length byte) = 3 wu of
        // corrections that the all-legacy descendant {L1, L2} avoids.
        Candidate {
            value: 20_010,
            weight: 400,
            input_count: 1,
            is_segwit: true,
        },
        Candidate {
            value: 20_000,
            weight: 400,
            input_count: 1,
            is_segwit: false,
        },
        Candidate {
            value: 20_000,
            weight: 400,
            input_count: 1,
            is_segwit: false,
        },
    ];

    let mut cs = CoinSelector::new(&candidates);
    cs.sort_candidates_by_descending_value_pwu();

    // D = {L1, L2}.
    let mut d = cs.clone();
    for (idx, c) in cs.candidates().collect::<Vec<_>>() {
        if !c.is_segwit {
            d.select(idx);
        }
    }

    // Pad the target's output weight so W_D % 4 == 0: fee(W_D) then has no vbyte rounding, and
    // the prefix's +3 wu of corrections cross a vbyte boundary (so the greedy walk cannot stop
    // at {SW, L1} and its lower bound carries the corrected prefix weight).
    let mut t = target(0, 100.0);
    let w_d_unpadded = d.weight(t.outputs, DrainWeights::NONE);
    t.outputs.weight_sum += (4 - (w_d_unpadded % 4)) % 4;

    // T such that D's excess is exactly 2 sats.
    let fee_d = rate.implied_fee(d.weight(t.outputs, DrainWeights::NONE));
    t.outputs.value_sum = (40_000_u64).checked_sub(fee_d + 2).expect("no underflow");

    println!(
        "W_D={} fee_D={} T={} excess(D)={}",
        d.weight(t.outputs, DrainWeights::NONE),
        fee_d,
        t.outputs.value_sum,
        d.excess(t, Drain::NONE)
    );
    assert_eq!(
        d.excess(t, Drain::NONE),
        2,
        "construction: D overshoots by 2"
    );
    assert!(d.is_funded(t), "D must be funded");

    assert_bound_admissible(metric(1.0), &cs, &d, t);
}

/// Regression (review finding #1): change_unavoidable's least-excess construction skips a candidate with small
/// positive *linear* effective value whose *actual* marginal excess is negative (segwit
/// corrections + vbyte rounding), so it prunes a branch with a changeless descendant.
#[test]
fn bound_does_not_prune_changeless_reachable_only_via_corrections() {
    // feerate 100 sat/vb (25 spwu), long-term 1 sat/vb -> spend_cost = ceil(600 * 0.25) = 150.
    let rate = FeeRate::from_sat_per_vb(100.0);
    let candidates = vec![
        // A, B: selected, legacy.
        Candidate {
            value: 20_000,
            weight: 40,
            input_count: 1,
            is_segwit: false,
        },
        Candidate {
            value: 20_000,
            weight: 40,
            input_count: 1,
            is_segwit: false,
        },
        // C: linear ev = 1050 - 40*25 = +50, but adding it to a 2-legacy tx also adds
        // +2 (header) + 2 (two legacy +1s) = 4 wu of corrections = 100 sats at 25 spwu,
        // so its actual marginal excess is negative.
        Candidate {
            value: 1_050,
            weight: 40,
            input_count: 1,
            is_segwit: true,
        },
    ];

    let mut cs = CoinSelector::new(&candidates);
    cs.sort_candidates_by_descending_value_pwu();

    // cs = {A, B}; D = {A, B, C}.
    let mut sel = cs.clone();
    for (idx, c) in cs.candidates().collect::<Vec<_>>() {
        if !c.is_segwit {
            sel.select(idx);
        }
    }
    let mut d = sel.clone();
    for (idx, c) in cs.candidates().collect::<Vec<_>>() {
        if c.is_segwit {
            d.select(idx);
        }
    }

    // T such that excess_with_drain_weight(sel) = 175: just above spend_cost (=150) so `sel`
    // "has change", while D's ~50 sat lower actual excess drops below it (changeless).
    let m = metric(1.0);
    let drain = Drain {
        weights: DRAIN_WEIGHTS,
        value: 0,
    };
    let fee_sel_with_drain = rate.implied_fee(sel.weight(target(0, 100.0).outputs, DRAIN_WEIGHTS));
    let mut t = target(0, 100.0);
    t.outputs.value_sum = 40_000 - fee_sel_with_drain - 175;

    // Load-bearing preconditions: `sel` must sit above the spend_cost edge (so by excess alone it
    // "has change" and change_unavoidable reaches the least-excess comparison being regression
    // tested), while D's actual excess must have dropped below it (changeless).
    assert_eq!(sel.excess(t, drain), 175, "sel must be past the edge (150)");
    assert!(
        d.excess(t, drain) <= 150,
        "D must be changeless: ewd(D) = {}",
        d.excess(t, drain)
    );
    assert!(d.is_funded(t), "D must be funded");

    assert_bound_admissible(m, &sel, &d, t);
}

/// Regression (review finding #2): ub_changeless_input_weight's knapsack demands the full vbyte-rounded `delta` in
/// linear-ev terms, but a descendant can become changeless shedding up to ~sat_vb+1 less (vbyte
/// rounding), so the LP over-removes weight (through a heavy low-ev candidate) and the upper
/// bound goes invalid. All-legacy candidates: this is purely a rounding repro.
#[test]
fn bound_is_valid_under_vbyte_rounding_slack() {
    // feerate 4 sat/vb (1 spwu), long-term 50 sat/vb -> rate_diff = -11.5 spwu.
    // spend_cost = ceil(600 * 12.5) = 7500 -> changeless edge = 7500.
    let rate = FeeRate::from_sat_per_vb(4.0);
    let candidates = vec![
        // A: the selected branch.
        Candidate {
            value: 29_000,
            weight: 400,
            input_count: 1,
            is_segwit: false,
        },
        // P: linear ev = 1000 - 399 = 601; its ACTUAL removal sheds 604 (the 399 wu removal
        // saves 99 vb = 396 sats when W_all is 4-aligned).
        Candidate {
            value: 1_000,
            weight: 399,
            input_count: 1,
            is_segwit: false,
        },
        // Q: linear ev = 2, raw 1400 — the LP's fractional over-removal runs through it.
        Candidate {
            value: 1_402,
            weight: 1_400,
            input_count: 1,
            is_segwit: false,
        },
    ];

    let mut cs = CoinSelector::new(&candidates);
    cs.sort_candidates_by_descending_value_pwu();

    let by_value = |v: u64| {
        cs.candidates()
            .collect::<Vec<_>>()
            .iter()
            .find(|(_, c)| c.value == v)
            .unwrap()
            .0
    };
    // cs = {A}; D = {A, Q} (sheds P only).
    let mut sel = cs.clone();
    sel.select(by_value(29_000));
    let mut d = sel.clone();
    d.select(by_value(1_402));

    let m = metric(50.0);
    let drain = Drain {
        weights: DRAIN_WEIGHTS,
        value: 0,
    };

    // Pad outputs so W(d_all)+drain is 4-aligned: removing P's 399 wu then saves only 99 vb.
    let mut d_all = cs.clone();
    d_all.select_all();
    let mut t = target(0, 4.0);
    let w_all_wd = d_all.weight(t.outputs, DRAIN_WEIGHTS);
    t.outputs.weight_sum += (4 - (w_all_wd % 4)) % 4;

    // T such that ewd(d_all) = edge + 603: delta = 603 > ev_lin(P) = 601, so the LP runs into Q
    // and fractionally removes (603-601)/2 * 1400 = 1400 wu. But D sheds P's ACTUAL 604 and is
    // already changeless at ewd = 7499, keeping Q's 1400 wu.
    let fee_all_wd = rate.implied_fee(d_all.weight(t.outputs, DRAIN_WEIGHTS));
    t.outputs.value_sum = (29_000 + 1_000 + 1_402) - fee_all_wd - (7_500 + 603);

    // Load-bearing preconditions: delta = ewd(d_all) - edge must be 603 — above ev_lin(P) = 601
    // so the knapsack runs into Q — while D (which sheds only P) must have landed changeless,
    // i.e. the vbyte rounding covered the 2-sat linear shortfall being regression tested.
    assert_eq!(
        d_all.excess(t, drain),
        7_500 + 603,
        "delta must be 603 (> ev_lin(P) = 601)"
    );
    assert!(
        d.excess(t, drain) <= 7_500,
        "D must be changeless: ewd(D) = {}",
        d.excess(t, drain)
    );
    assert!(d.is_funded(t), "D must be funded");
    assert!(
        sel.excess(t, drain) <= 7_500,
        "cs itself must be changeless so change_unavoidable does not prune"
    );

    assert_bound_admissible(m, &sel, &d, t);
}

/// Regression (review round 2, finding #1): the rate_diff >= 0 bound computed its funding `gap`
/// from f32 conversions of full-magnitude sat values. f32's 24-bit mantissa is coarser than a sat
/// above ~0.17 BTC (32 sats at 5 BTC), so an exactly-funded selection could produce a phantom
/// positive gap; with only negative-ev candidates left, the phantom gap became an invalid `None`
/// prune of a branch whose selection is itself funded, changeless and scoreable. Funded-ness must
/// be decided by exact integer arithmetic and the walk must run in f64.
#[test]
fn bound_is_valid_at_btc_scale_values() {
    // feerate 2 sat/vb (0.5 spwu), long-term 1 sat/vb -> rate_diff > 0.
    let rate = FeeRate::from_sat_per_vb(2.0);
    let candidates = vec![
        // A: selected; value/target tuned below so integer excess is exactly 0.
        Candidate {
            value: 500_000_400, // 5 BTC: f32 ulp here is 32 sats
            weight: 272,
            input_count: 1,
            is_segwit: false,
        },
        // B: negative ev, so the bound's knapsack walk finds nothing to cover a phantom gap.
        Candidate {
            value: 1,
            weight: 272,
            input_count: 1,
            is_segwit: false,
        },
    ];
    let cs = CoinSelector::new(&candidates);
    let mut sel = cs.clone();
    sel.select(0);

    let mut t = target(0, 2.0);
    t.outputs.value_sum = 500_000_176;
    t.outputs.weight_sum = 136; // W = 448 (incl. output-count varint) -> fee = 224 = A.value - target.value

    // The two load-bearing preconditions: the selection is funded with integer excess exactly 0,
    // while the f32 rendition of the linear gap is phantom-positive (+32: both big values sit on
    // f32 round-to-even ties that resolve away from each other).
    assert_eq!(sel.excess(t, Drain::NONE), 0);
    let w = sel.weight(t.outputs, DrainWeights::NONE);
    let f32_gap = t.outputs.value_sum as f32 + w as f32 * rate.spwu() - sel.selected_value() as f32;
    assert!(
        f32_gap > 0.0,
        "construction: the f32 gap must be phantom-positive (got {})",
        f32_gap
    );

    // The funded selection is its own descendant: it scores, so its bound must exist and admit it.
    assert_bound_admissible(metric(1.0), &sel, &sel, t);
}

/// If some descendant `d` of `cs` has a score, then `bound(cs)` must be `Some` and `<= score(d)`.
///
/// This is stronger than `common::ensure_bound_is_not_too_tight`, which skips branches where
/// `bound` returns `None` and therefore cannot catch invalid prunes.
fn assert_bound_admissible(
    mut metric: ChangelessWaste,
    cs: &CoinSelector<'_>,
    descendant: &CoinSelector<'_>,
    target: Target,
) {
    let score = metric
        .score(descendant, target)
        .expect("test construction broken: descendant must be scoreable");
    let bound = metric.bound(cs, target).unwrap_or_else(|| {
        panic!(
            "bound pruned the branch but a descendant scores {:?}",
            score
        )
    });
    assert!(
        bound <= score,
        "bound {:?} > descendant score {:?}",
        bound,
        score
    );
}

/// `drain_value` must refuse change that would push the tx over `target.max_weight` (the same
/// rule as `LowestFee`), making such selections changeless and scoreable — and the
/// `change_unavoidable` prune must account for that: a branch may look change-forced by excess
/// alone while a heavier descendant is changeless because the cap refuses its change output.
#[test]
fn cap_refused_change_is_changeless_and_not_pruned() {
    // feerate 10 sat/vb (2.5 spwu), long-term 1 sat/vb -> spend_cost = ceil(600 * 0.25) = 150.
    let rate = FeeRate::from_sat_per_vb(10.0);
    let drain_weights = DrainWeights {
        output_weight: 100,
        spend_weight: 600,
        n_outputs: 1,
    };
    let metric = ChangelessWaste {
        long_term_feerate: FeeRate::from_sat_per_vb(1.0),
        dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
        drain_weights,
    };

    let candidates = vec![
        Candidate {
            value: 10_000,
            weight: 400,
            input_count: 1,
            is_segwit: false,
        },
        Candidate {
            value: 10_000,
            weight: 400,
            input_count: 1,
            is_segwit: false,
        },
    ];
    let cs = CoinSelector::new(&candidates);

    // cs = {A}; s = {A, B}.
    let mut sel = cs.clone();
    sel.select(0);
    let mut s = sel.clone();
    s.select(1);

    let mut target = Target {
        fee: TargetFee {
            rate,
            absolute: 0,
            replace: None,
        },
        outputs: TargetOutputs {
            value_sum: 0,
            weight_sum: 100,
            n_outputs: 1,
        },
        max_weight: None,
    };
    // T such that excess({A}) = 500: its excess-with-drain (250 lower) sits above spend_cost,
    // so by excess alone every superset of {A} "has change"...
    target.outputs.value_sum =
        10_000 - rate.implied_fee(sel.weight(target.outputs, DrainWeights::NONE)) - 500;
    // ...but the cap admits {A, B} only WITHOUT its change output: change is refused, so
    // {A, B} is changeless and scoreable.
    target.max_weight = Some(s.weight(target.outputs, DrainWeights::NONE));

    let mut m = metric;
    assert!(
        m.score(&s, target).is_some(),
        "cap-refused change must make the selection changeless and scoreable"
    );
    assert_bound_admissible(metric, &sel, &s, target);
}

/// Sanity-check: the BnB solution must never have a change output, and its waste must be
/// no greater than the waste of any changeless brute-force selection we try.
#[test]
fn solution_is_changeless_and_not_worse_than_naive() {
    let params = common::StrategyParams {
        n_candidates: 12,
        target_value: 90_000,
        n_target_outputs: 1,
        target_weight: 200 - TX_FIXED_FIELD_WEIGHT as u32 - 1,
        replace: None,
        feerate: 10.0,
        feerate_lt_diff: 2.0, // long_term_feerate < feerate (rate_diff > 0)
        drain_weight: 200,
        drain_spend_weight: 600,
        drain_dust: 200,
        n_drain_outputs: 1,
        max_weight: None,
        absolute: 0,
    };

    let candidates = common::gen_candidates(params.n_candidates);
    let mut cs = CoinSelector::new(&candidates);

    let mut metric = ChangelessWaste {
        long_term_feerate: params.long_term_feerate(),
        dust_relay_feerate: params.dust_relay_feerate(),
        drain_weights: params.drain_weights(),
    };

    match common::bnb_search(&mut cs, params.target(), metric, usize::MAX) {
        Ok((_score, _rounds)) => {
            // A scored solution is changeless by construction: `score` returns `None` for any
            // selection the metric would give a change output, so a returned solution is one the
            // metric was able to score.
            assert!(
                metric.score(&cs, params.target()).is_some(),
                "BnB result must be changeless and meet the target"
            );
            assert!(cs.is_funded(params.target()));
        }
        Err(_) => {
            // No changeless solution exists for this combo — that's allowed.
        }
    }
}

/// When `rate_diff < 0`, the metric will tend to consolidate (add inputs to reduce input_waste),
/// but only as long as it can keep the selection changeless.
#[test]
fn consolidation_regime_stays_changeless() {
    let params = common::StrategyParams {
        n_candidates: 10,
        target_value: 50_000,
        n_target_outputs: 1,
        target_weight: 200 - TX_FIXED_FIELD_WEIGHT as u32 - 1,
        replace: None,
        feerate: 2.0,
        feerate_lt_diff: 10.0, // long_term_feerate > feerate (rate_diff < 0)
        drain_weight: 200,
        drain_spend_weight: 600,
        drain_dust: 200,
        n_drain_outputs: 1,
        max_weight: None,
        absolute: 0,
    };

    let candidates = common::gen_candidates(params.n_candidates);
    let mut cs = CoinSelector::new(&candidates);

    let mut metric = ChangelessWaste {
        long_term_feerate: params.long_term_feerate(),
        dust_relay_feerate: params.dust_relay_feerate(),
        drain_weights: params.drain_weights(),
    };

    if common::bnb_search(&mut cs, params.target(), metric, usize::MAX).is_ok() {
        assert!(
            metric.score(&cs, params.target()).is_some(),
            "result must be changeless"
        );
    }
}

/// A candidate pool with ~50% segwit and small values/weights, where the `input_weight()`
/// corrections and vbyte rounding are large relative to candidate effective values — the regime
/// the review findings live in (`gen_candidates` makes segwit candidates only 1% of the time).
#[cfg(not(debug_assertions))] // only used by the release-gated proptest below
fn small_candidate() -> impl Strategy<Value = Candidate> {
    (100..5_000_u64, 100..1_500_u64, prop::bool::ANY).prop_map(|(value, weight, is_segwit)| {
        Candidate {
            value,
            weight,
            input_count: 1,
            is_segwit,
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2048, ..Default::default() })]

    #[test]
    #[cfg(not(debug_assertions))]
    fn segwit_mixed_bound_is_not_too_tight(
        candidates in prop::collection::vec(small_candidate(), 1..8),
        target_value in 200..8_000_u64,
        feerate in 1.0..100.0_f32,
        feerate_lt_diff in -99.0..50.0_f32,
        drain_weight in 100..=500_u32,
        drain_spend_weight in 1..=2000_u32,
        drain_dust in 100..=1000_u64,
    ) {
        let params = common::StrategyParams {
            n_candidates: candidates.len(),
            target_value,
            n_target_outputs: 1,
            target_weight: 100,
            replace: None,
            feerate,
            feerate_lt_diff,
            drain_weight,
            drain_spend_weight,
            drain_dust,
            n_drain_outputs: 1,
            max_weight: None,
            absolute: 0,
        };
        let metric = ChangelessWaste {
            long_term_feerate: params.long_term_feerate(),
            dust_relay_feerate: params.dust_relay_feerate(),
            drain_weights: params.drain_weights(),
        };
        common::ensure_bound_is_not_too_tight(params, candidates, metric)?;
    }
}
