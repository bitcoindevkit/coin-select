#![allow(unused_imports)]

mod common;
use bdk_coin_select::metrics::{Changeless, LowestFee};
use bdk_coin_select::{BnbMetric, Candidate, CoinSelector};
use bdk_coin_select::{ChangePolicy, Drain, DrainWeights, FeeRate, Target};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig {
        ..Default::default()
    })]

    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn can_eventually_find_best_solution(
        n_candidates in 1..20_usize,        // candidates (n)
        target_value in 500..500_000_u64,   // target value (sats)
        base_weight in 0..1000_u32,         // base weight (wu)
        min_fee in 0..1000_u64,             // min fee (sats)
        feerate in 1.0..100.0_f32,          // feerate (sats/vb)
        feerate_lt_diff in -5.0..50.0_f32,  // longterm feerate diff (sats/vb)
        drain_weight in 100..=500_u32,      // drain weight (wu)
        drain_spend_weight in 1..=2000_u32, // drain spend weight (wu)
        drain_dust in 100..=1000_u64,       // drain dust (sats)
    ) {
        let params = common::StrategyParams { n_candidates, target_value, base_weight, min_fee, feerate, feerate_lt_diff, drain_weight, drain_spend_weight, drain_dust };
        let candidates = common::gen_candidates(params.n_candidates);
        let change_policy = ChangePolicy::min_value_and_waste(params.drain_weights(), params.drain_dust, params.feerate(), params.long_term_feerate());
        let metric = LowestFee { target: params.target(), long_term_feerate: params.long_term_feerate(), change_policy };
        common::can_eventually_find_best_solution(params, candidates, change_policy, metric)?;
    }

    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn ensure_bound_is_not_too_tight(
        n_candidates in 0..15_usize,        // candidates (n)
        target_value in 500..500_000_u64,   // target value (sats)
        base_weight in 0..1000_u32,         // base weight (wu)
        min_fee in 0..1000_u64,             // min fee (sats)
        feerate in 1.0..100.0_f32,          // feerate (sats/vb)
        feerate_lt_diff in -5.0..50.0_f32,  // longterm feerate diff (sats/vb)
        drain_weight in 100..=500_u32,      // drain weight (wu)
        drain_spend_weight in 1..=1000_u32, // drain spend weight (wu)
        drain_dust in 100..=1000_u64,       // drain dust (sats)
    ) {
        let params = common::StrategyParams { n_candidates, target_value, base_weight, min_fee, feerate, feerate_lt_diff, drain_weight, drain_spend_weight, drain_dust };
        let candidates = common::gen_candidates(params.n_candidates);
        let change_policy = ChangePolicy::min_value_and_waste(params.drain_weights(), params.drain_dust, params.feerate(), params.long_term_feerate());
        let metric = LowestFee { target: params.target(), long_term_feerate: params.long_term_feerate(), change_policy };
        common::ensure_bound_is_not_too_tight(params, candidates, change_policy, metric)?;
    }

    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn identical_candidates(
        n_candidates in 30..300_usize,
        target_value in 50_000..500_000_u64,   // target value (sats)
        base_weight in 0..641_u32,          // base weight (wu)
        min_fee in 0..1_000_u64,            // min fee (sats)
        feerate in 1.0..10.0_f32,          // feerate (sats/vb)
        feerate_lt_diff in -5.0..5.0_f32,  // longterm feerate diff (sats/vb)
        drain_weight in 100..=500_u32,      // drain weight (wu)
        drain_spend_weight in 1..=1000_u32, // drain spend weight (wu)
        drain_dust in 100..=1000_u64,       // drain dust (sats)
    ) {
        println!("== TEST ==");

        let params = common::StrategyParams {
            n_candidates,
            target_value,
            base_weight,
            min_fee,
            feerate,
            feerate_lt_diff,
            drain_weight,
            drain_spend_weight,
            drain_dust,
        };
        println!("{:?}", params);

        let candidates = core::iter::repeat(Candidate {
                value: 20_000,
                weight: (32 + 4 + 4 + 1) * 4 + 64 + 32,
                input_count: 1,
                is_segwit: true,
            })
            .take(params.n_candidates)
            .collect::<Vec<_>>();

        let mut cs = CoinSelector::new(&candidates, params.base_weight);

        let change_policy = ChangePolicy::min_value_and_waste(
            params.drain_weights(),
            params.drain_dust,
            params.feerate(),
            params.long_term_feerate(),
        );

        let metric = LowestFee {
            target: params.target(),
            long_term_feerate: params.long_term_feerate(),
            change_policy,
        };

        let (score, rounds) = common::bnb_search(&mut cs, metric, params.n_candidates)?;
        println!("\t\tscore={} rounds={}", score, rounds);
        prop_assert!(rounds <= params.n_candidates);
    }
}

/// We combine the `LowestFee` and `Changeless` metrics to derive a `ChangelessLowestFee` metric.
#[test]
fn combined_changeless_metric() {
    let params = common::StrategyParams {
        n_candidates: 100,
        target_value: 100_000,
        base_weight: 1000,
        min_fee: 0,
        feerate: 5.0,
        feerate_lt_diff: -4.0,
        drain_weight: 200,
        drain_spend_weight: 600,
        drain_dust: 200,
    };

    let candidates = common::gen_candidates(params.n_candidates);
    let mut cs_a = CoinSelector::new(&candidates, params.base_weight);
    let mut cs_b = CoinSelector::new(&candidates, params.base_weight);

    let change_policy = ChangePolicy::min_value_and_waste(
        params.drain_weights(),
        params.drain_dust,
        params.feerate(),
        params.long_term_feerate(),
    );

    let metric_lowest_fee = LowestFee {
        target: params.target(),
        long_term_feerate: params.long_term_feerate(),
        change_policy,
    };

    let metric_changeless = Changeless {
        target: params.target(),
        change_policy,
    };

    let metric_combined = ((metric_lowest_fee, 1.0_f32), (metric_changeless, 0.0_f32));

    // cs_a uses the non-combined metric
    let (score, rounds) =
        common::bnb_search(&mut cs_a, metric_lowest_fee, usize::MAX).expect("must find solution");
    println!("score={:?} rounds={}", score, rounds);

    // cs_b uses the combined metric
    let (combined_score, combined_rounds) =
        common::bnb_search(&mut cs_b, metric_combined, usize::MAX).expect("must find solution");
    println!("score={:?} rounds={}", combined_score, combined_rounds);

    assert!(combined_rounds >= rounds);
}

/// This test considers the case where you could actually lower your long term fee by adding another input.
#[test]
fn adding_another_input_to_remove_change() {
    let target = Target {
        feerate: FeeRate::from_sat_per_vb(1.0),
        min_fee: 0,
        value: 99_870,
    };

    let candidates = vec![
        Candidate {
            value: 100_000,
            weight: 100,
            input_count: 1,
            is_segwit: true,
        },
        Candidate {
            value: 50_000,
            weight: 100,
            input_count: 1,
            is_segwit: true,
        },
        // NOTE: this input has negative effective value
        Candidate {
            value: 10,
            weight: 100,
            input_count: 1,
            is_segwit: true,
        },
    ];

    let mut cs = CoinSelector::new(&candidates, 200);

    let best_solution = {
        let mut cs = cs.clone();
        cs.select(0);
        cs.select(2);
        cs.excess(target, Drain::none());
        assert!(cs.is_target_met(target));
        cs
    };

    let drain_weights = DrainWeights {
        output_weight: 100,
        spend_weight: 1_000,
    };

    let excess_to_make_first_candidate_satisfy_but_have_change = {
        let mut cs = cs.clone();
        cs.select(0);
        assert!(cs.is_target_met(target));
        let with_change_excess = cs.excess(
            target,
            Drain {
                value: 0,
                weights: drain_weights,
            },
        );
        assert!(with_change_excess > 0);
        with_change_excess as u64
    };

    let change_policy = ChangePolicy {
        min_value: excess_to_make_first_candidate_satisfy_but_have_change - 10,
        drain_weights,
    };

    let mut metric = LowestFee {
        target,
        long_term_feerate: FeeRate::from_sat_per_vb(1.0),
        change_policy,
    };

    let (score, _) = common::bnb_search(&mut cs, metric, 10).expect("finds solution");
    let best_solution_score = metric.score(&best_solution).expect("must be a solution");

    assert_eq!(best_solution.drain(target, change_policy), Drain::none());

    assert!(score <= best_solution_score);
    assert_eq!(cs.selected_indices(), best_solution.selected_indices());
}
