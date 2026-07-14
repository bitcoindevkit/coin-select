#![allow(unused)]
mod common;
use bdk_coin_select::{
    float::Ordf32,
    metrics::{Changeless, LowestFee},
    Candidate, CoinSelector, DrainWeights, FeeRate, Target, TargetFee, TargetOutputs,
};
use proptest::{prelude::*, proptest, test_runner::*};
use rand::{prelude::IteratorRandom, Rng, RngCore};

fn test_wv(mut rng: impl RngCore) -> impl Iterator<Item = Candidate> {
    core::iter::repeat_with(move || {
        let value = rng.random_range(0..1_000);
        Candidate {
            value,
            weight: rng.random_range(0..100),
            input_count: rng.random_range(1..2),
            is_segwit: false,
        }
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        ..Default::default()
    })]

    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn compare_against_benchmarks(
        n_candidates in 0..15_usize,        // candidates (n)
        target_value in 500..1_000_000_u64,   // target value (sats)
        n_target_outputs in 1..150_usize,    // the number of outputs we're funding
        target_weight in 0..10_000_u32,         // the sum of the weight of the outputs (wu)
        replace in common::maybe_replace(0..10_000u64), // The weight of the transaction we're replacing
        feerate in 1.0..100.0_f32,          // feerate (sats/vb)
        feerate_lt_diff in -5.0..50.0_f32,  // longterm feerate diff (sats/vb)
        drain_weight in 100..=500_u32,      // drain weight (wu)
        drain_spend_weight in 1..=2000_u32, // drain spend weight (wu)
        drain_dust in 100..=1000_u64,       // drain dust (sats)
        n_drain_outputs in 1..150usize,     // the number of drain outputs
    ) {
        println!("=======================================");
        let start = std::time::Instant::now();
        let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
        let feerate = FeeRate::from_sat_per_vb(feerate);
        let drain_weights = DrainWeights {
            output_weight: drain_weight as u64,
            spend_weight: drain_spend_weight as u64,
            n_outputs: n_drain_outputs,
        };

        let wv = test_wv(&mut rng);
        let candidates = wv.take(n_candidates).collect::<Vec<_>>();

        let cs = CoinSelector::new(&candidates);

        let target = Target {
            outputs: TargetOutputs {
                n_outputs: n_target_outputs,
                value_sum: target_value,
                weight_sum: target_weight as u64,
            },
            fee: TargetFee {
                rate: feerate,
                replace,
                ..TargetFee::ZERO
            },
            max_weight: None,
        };

        let make_metric = || {
            Changeless(LowestFee {
                long_term_feerate: feerate,
                dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
                drain_weights,
            })
        };

        let solutions = cs.bnb_solutions(target, make_metric());

        println!("candidates: {:#?}", cs.candidates().collect::<Vec<_>>());

        let best = solutions
            .enumerate()
            .filter_map(|(i, sol)| Some((i, sol?)))
            .last();


        match best {
            Some((_i, (_sol, _score))) => {
                /* there is nothing to check about a changeless solution */
            }
            None => {
                let mut cs = cs.clone();
                let mut metric = make_metric();
                let has_solution = common::exhaustive_search(&mut cs, target, &mut metric).is_some();
                dbg!(format!("{}", cs));
                assert!(!has_solution);
            }
        }
        dbg!(start.elapsed());
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
        let change_policy = ChangePolicy::min_value(params.drain_weights(), params.drain_dust);
        let metric = Changeless { target: params.target(), change_policy };
        common::ensure_bound_is_not_too_tight(params, candidates, change_policy, metric)?;
    }
}
