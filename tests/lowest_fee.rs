#![allow(unused_imports)]

mod common;
use bdk_coin_select::metrics::{Changeless, LowestFee};
use bdk_coin_select::{
    BnbMetric, Candidate, ChangePolicy, CoinSelector, Drain, DrainWeights, FeeRate, Replace,
    Target, TargetFee, TargetOutputs, TX_FIXED_FIELD_WEIGHT,
};
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
        n_target_outputs in 1usize..150,    // the number of outputs we're funding
        target_weight in 0..10_000_u32,         // the sum of the weight of the outputs (wu)
        replace in common::maybe_replace(0u64..10_000), // The weight of the transaction we're replacing
        feerate in 1.0..100.0_f32,          // feerate (sats/vb)
        feerate_lt_diff in -5.0..50.0_f32,  // longterm feerate diff (sats/vb)
        drain_weight in 100..=500_u32,      // drain weight (wu)
        drain_spend_weight in 1..=2000_u32, // drain spend weight (wu)
        drain_dust in 100..=1000_u64,       // drain dust (sats)
        n_drain_outputs in 1usize..150,     // the number of drain outputs
    ) {
        let params = common::StrategyParams { n_candidates, target_value, n_target_outputs, target_weight, replace, feerate, feerate_lt_diff, drain_weight, drain_spend_weight, drain_dust, n_drain_outputs };
        let candidates = common::gen_candidates(params.n_candidates);
        let metric = params.lowest_fee_metric();
        common::can_eventually_find_best_solution(params, candidates, metric)?;
    }

    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn ensure_bound_is_not_too_tight(
        n_candidates in 0..15_usize,        // candidates (n)
        target_value in 500..500_000_u64,   // target value (sats)
        n_target_outputs in 1usize..150,    // the number of outputs we're funding
        target_weight in 0..10_000_u32,         // the sum of the weight of the outputs (wu)
        replace in common::maybe_replace(0u64..10_000), // The weight of the transaction we're replacing
        feerate in 1.0..100.0_f32,          // feerate (sats/vb)
        feerate_lt_diff in -5.0..50.0_f32,  // longterm feerate diff (sats/vb)
        drain_weight in 100..=500_u32,      // drain weight (wu)
        drain_spend_weight in 1..=2000_u32, // drain spend weight (wu)
        drain_dust in 100..=1000_u64,       // drain dust (sats)
        n_drain_outputs in 1usize..150,     // the number of drain outputs
    ) {
        let params = common::StrategyParams { n_candidates, target_value, n_target_outputs, target_weight, replace, feerate, feerate_lt_diff, drain_weight, drain_spend_weight, drain_dust, n_drain_outputs };
        let candidates = common::gen_candidates(params.n_candidates);
        let metric = params.lowest_fee_metric();
        common::ensure_bound_is_not_too_tight(params, candidates, metric)?;
    }

    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn identical_candidates(
        n_candidates in 30..300_usize,
        target_value in 50_000..500_000_u64,   // target value (sats)
        n_target_outputs in 1usize..150,    // the number of outputs we're funding
        target_weight in 0..10_000_u32,         // the sum of the weight of the outputs (wu)
        replace in common::maybe_replace(0u64..10_000), // The weight of the transaction we're replacing
        feerate in 1.0..100.0_f32,          // feerate (sats/vb)
        feerate_lt_diff in -5.0..50.0_f32,  // longterm feerate diff (sats/vb)
        drain_weight in 100..=500_u32,      // drain weight (wu)
        drain_spend_weight in 1..=2000_u32, // drain spend weight (wu)
        drain_dust in 100..=1000_u64,       // drain dust (sats)
        n_drain_outputs in 1usize..150,     // the number of drain outputs
    ) {
        println!("== TEST ==");

        let params = common::StrategyParams { n_candidates, target_value, n_target_outputs, target_weight, replace, feerate, feerate_lt_diff, drain_weight, drain_spend_weight, drain_dust, n_drain_outputs };
        println!("{:?}", params);

        let candidates = core::iter::repeat(Candidate {
                value: 20_000,
                weight: (32 + 4 + 4 + 1) * 4 + 64 + 32,
                input_count: 1,
                is_segwit: true,
            })
            .take(params.n_candidates)
            .collect::<Vec<_>>();

        let mut cs = CoinSelector::new(&candidates);

        let metric = params.lowest_fee_metric();
        let is_impossible = !cs.is_selection_possible(params.target());
        match common::bnb_search(&mut cs, params.target(), metric, params.n_candidates * 10) {
            Ok((score, rounds)) => {
                // the +1 is because the iterator will always try selecting nothing as a solution so we have
                // to do one extra iteration to try that
                prop_assert!(rounds <= params.n_candidates + 1, "\t\tscore={} rounds={}", score, rounds)
            },
            Err(_e) => assert!(is_impossible),
        }
    }

    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn compare_against_benchmarks(
        n_candidates in 0..50_usize,        // candidates (n)
        target_value in 500..1_000_000_u64,   // target value (sats)
        n_target_outputs in 1usize..150,    // the number of outputs we're funding
        target_weight in 0..10_000_u32,         // the sum of the weight of the outputs (wu)
        replace in common::maybe_replace(0u64..10_000), // The weight of the transaction we're replacing
        feerate in 1.0..100.0_f32,          // feerate (sats/vb)
        feerate_lt_diff in -5.0..50.0_f32,  // longterm feerate diff (sats/vb)
        drain_weight in 100..=500_u32,      // drain weight (wu)
        drain_spend_weight in 1..=2000_u32, // drain spend weight (wu)
        drain_dust in 100..=1000_u64,       // drain dust (sats)
        n_drain_outputs in 1usize..150,     // the number of drain outputs
    ) {

        let params = common::StrategyParams { n_candidates, target_value, n_target_outputs, target_weight, replace, feerate, feerate_lt_diff, drain_weight, drain_spend_weight, drain_dust, n_drain_outputs };
        let candidates = common::gen_candidates(params.n_candidates);
        let metric = params.lowest_fee_metric();
        common::compare_against_benchmarks(params, candidates, metric)?;
    }
}

/// We wrap `LowestFee` in `Changeless` to derive a metric that finds the lowest-fee changeless
/// solution. Constraining to changeless should never take fewer rounds than the unconstrained
/// `LowestFee`.
#[test]
fn combined_changeless_metric() {
    let params = common::StrategyParams {
        n_candidates: 100,
        target_value: 100_000,
        target_weight: 1000 - TX_FIXED_FIELD_WEIGHT as u32 - 1,
        replace: None,
        feerate: 5.0,
        feerate_lt_diff: -4.0,
        drain_weight: 200,
        drain_spend_weight: 600,
        drain_dust: 200,
        n_target_outputs: 1,
        n_drain_outputs: 1,
    };

    let candidates = common::gen_candidates(params.n_candidates);
    let mut cs_a = CoinSelector::new(&candidates);
    let mut cs_b = CoinSelector::new(&candidates);

    let target = params.target();
    let metric_lowest_fee = params.lowest_fee_metric();

    let metric_changeless = Changeless(params.lowest_fee_metric());

    // cs_a uses the unconstrained metric
    let (score, rounds) = common::bnb_search(&mut cs_a, target, metric_lowest_fee, usize::MAX)
        .expect("must find solution");
    println!("score={:?} rounds={}", score, rounds);

    // cs_b uses the changeless-constrained metric
    let (combined_score, combined_rounds) =
        common::bnb_search(&mut cs_b, target, metric_changeless, usize::MAX)
            .expect("must find solution");
    println!("score={:?} rounds={}", combined_score, combined_rounds);

    assert!(combined_rounds >= rounds);
}

/// Because this metric decides change optimally, it never creates a change output whose value
/// wouldn't cover the future cost of spending it. Here a single input overshoots the target by only
/// ~130 sats — far less than the drain's spend cost — so the fee-optimal choice is to burn the
/// excess (changeless) rather than add not-worth-it change, and adding a further input to shed the
/// excess only raises the fee.
#[test]
fn does_not_create_change_below_spend_cost() {
    let target = Target {
        fee: TargetFee::default(),
        outputs: TargetOutputs {
            value_sum: 99_870,
            weight_sum: 200 - TX_FIXED_FIELD_WEIGHT - 1,
            n_outputs: 1,
        },
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

    let mut cs = CoinSelector::new(&candidates);

    let drain_weights = DrainWeights {
        output_weight: 100,
        spend_weight: 1_000,
        n_outputs: 1,
    };

    let mut metric = LowestFee {
        long_term_feerate: FeeRate::from_sat_per_vb(1.0),
        dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
        drain_weights,
    };

    let (score, _) = common::bnb_search(&mut cs, target, metric, 10).expect("finds solution");

    // The optimal selection is candidate 0 alone, and it must be changeless.
    let expected = {
        let mut expected = CoinSelector::new(&candidates);
        expected.select(0);
        expected
    };
    assert_eq!(cs.selected_indices(), expected.selected_indices());
    assert!(
        metric.drain(&cs, target).is_none(),
        "optimal selection must be changeless"
    );

    // Adding the negative-effective-value input to shed the excess only raises the fee.
    let with_extra_input = {
        let mut with_extra_input = expected.clone();
        with_extra_input.select(2);
        with_extra_input
    };
    assert!(
        score
            <= metric
                .score(&with_extra_input, target)
                .expect("target is met")
    );
}

#[test]
fn zero_fee_tx() {
    let target_feerate = FeeRate::ZERO;
    let long_term_feerate = FeeRate::DEFAULT_MIN_RELAY;

    let target = Target {
        fee: TargetFee {
            rate: target_feerate,
            replace: None,
            ..TargetFee::ZERO
        },
        outputs: TargetOutputs {
            value_sum: 99_870,
            weight_sum: 200 - TX_FIXED_FIELD_WEIGHT - 1,
            n_outputs: 1,
        },
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
    ];

    let drain_weights = DrainWeights {
        output_weight: 100,
        spend_weight: 1_000,
        n_outputs: 1,
    };

    let mut cs = CoinSelector::new(&candidates);
    let metric = LowestFee {
        long_term_feerate,
        dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
        drain_weights,
    };
    let (_score, _rounds) =
        common::bnb_search(&mut cs, target, metric, 1000).expect("must find solution");
}
