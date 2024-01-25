#![allow(unused)]
mod common;
use bdk_coin_select::{
    float::Ordf32,
    metrics::{self, Changeless},
    Candidate, ChangePolicy, CoinSelector, Drain, DrainWeights, FeeRate, Target, TargetFee,
    TargetOutputs,
};
use proptest::{prelude::*, proptest, test_runner::*};
use rand::{prelude::IteratorRandom, Rng, RngCore};

fn test_wv(mut rng: impl RngCore) -> impl Iterator<Item = Candidate> {
    core::iter::repeat_with(move || {
        let value = rng.gen_range(0..1_000);
        Candidate {
            value,
            weight: rng.gen_range(0..100),
            input_count: rng.gen_range(1..2),
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
        n_candidates in 0..50_usize,        // candidates (n)
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
            output_weight: drain_weight,
            spend_weight: drain_spend_weight,
            n_outputs: n_drain_outputs,
        };

        let change_policy = ChangePolicy::min_value(drain_weights, 100);
        let wv = test_wv(&mut rng);
        let candidates = wv.take(n_candidates).collect::<Vec<_>>();

        let cs = CoinSelector::new(&candidates);

        let target = Target {
            outputs: TargetOutputs {
                n_outputs: n_target_outputs,
                value_sum: target_value,
                weight_sum: target_weight,
            },
            fee: TargetFee {
                rate: feerate,
                replace,
            }
        };

        let solutions = cs.bnb_solutions(metrics::Changeless {
            target,
            change_policy
        });

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
                let mut metric = metrics::Changeless { target, change_policy };
                let has_solution = common::exhaustive_search(&mut cs, &mut metric).is_some();
                dbg!(format!("{}", cs));
                assert!(!has_solution);
            }
        }
        dbg!(start.elapsed());
    }
}
