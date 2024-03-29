mod common;
use bdk_coin_select::{
    float::Ordf32, BnbMetric, Candidate, CoinSelector, Drain, Target, TargetFee, TargetOutputs,
};
#[macro_use]
extern crate alloc;

use alloc::vec::Vec;
use proptest::{prelude::*, proptest, test_runner::*};

fn test_wv(mut rng: impl RngCore) -> impl Iterator<Item = Candidate> {
    core::iter::repeat_with(move || {
        let value = rng.gen_range(0..1_000);
        let mut candidate = Candidate {
            value,
            weight: 100,
            input_count: rng.gen_range(1..2),
            is_segwit: rng.gen_bool(0.5),
        };
        // HACK: set is_segwit = true for all these tests because you can't actually lower bound
        // things easily with how segwit inputs interfere with their weights. We can't modify the
        // above since that would change what we pull from rng.
        candidate.is_segwit = true;
        candidate
    })
}

/// This is just an exhaustive search
struct MinExcessThenWeight {
    target: Target,
}

/// Assumes tx weight is less than 1MB.
const EXCESS_RATIO: f32 = 1_000_000_f32;

impl BnbMetric for MinExcessThenWeight {
    fn score(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf32> {
        let excess = cs.excess(self.target, Drain::NONE);
        if excess < 0 {
            None
        } else {
            Some(Ordf32(
                excess as f32 * EXCESS_RATIO + cs.input_weight() as f32,
            ))
        }
    }

    fn bound(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf32> {
        let mut cs = cs.clone();
        cs.select_until_target_met(self.target).ok()?;
        Some(Ordf32(cs.input_weight() as f32))
    }
}

#[test]
/// Detect regressions/improvements by making sure it always finds the solution in the same
/// number of iterations.
fn bnb_finds_an_exact_solution_in_n_iter() {
    let solution_len = 6;
    let num_additional_canidates = 12;

    let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
    let mut wv = test_wv(&mut rng).map(|mut candidate| {
        candidate.is_segwit = true;
        candidate
    });

    let solution: Vec<Candidate> = (0..solution_len).map(|_| wv.next().unwrap()).collect();
    let solution_weight = {
        let mut cs = CoinSelector::new(&solution);
        cs.select_all();
        cs.input_weight()
    };

    let target_value = solution.iter().map(|c| c.value).sum();

    let mut candidates = solution;
    candidates.extend(wv.take(num_additional_canidates));
    candidates.sort_unstable_by_key(|wv| core::cmp::Reverse(wv.value));

    let cs = CoinSelector::new(&candidates);

    let target = Target {
        outputs: TargetOutputs {
            value_sum: target_value,
            weight_sum: 0,
            n_outputs: 1,
        },
        // we're trying to find an exact selection value so set fees to 0
        fee: TargetFee::ZERO,
    };

    let solutions = cs.bnb_solutions(MinExcessThenWeight { target });

    let mut rounds = 0;
    let (best, score) = solutions
        .enumerate()
        .inspect(|(i, _)| rounds = *i + 1)
        .filter_map(|(_, sol)| sol)
        .last()
        .expect("it found a solution");

    assert_eq!(rounds, 3150);
    assert_eq!(best.input_weight(), solution_weight);
    assert_eq!(best.selected_value(), target_value, "score={:?}", score);
}

#[test]
fn bnb_finds_solution_if_possible_in_n_iter() {
    let num_inputs = 18;
    let target_value = 8_314;
    let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
    let wv = test_wv(&mut rng);
    let candidates = wv.take(num_inputs).collect::<Vec<_>>();

    let cs = CoinSelector::new(&candidates);

    let target = Target {
        outputs: TargetOutputs {
            value_sum: target_value,
            weight_sum: 0,
            n_outputs: 1,
        },
        fee: TargetFee::default(),
    };

    let solutions = cs.bnb_solutions(MinExcessThenWeight { target });

    let mut rounds = 0;
    let (sol, _score) = solutions
        .enumerate()
        .inspect(|(i, _)| rounds = *i + 1)
        .filter_map(|(_, sol)| sol)
        .last()
        .expect("found a solution");

    assert_eq!(rounds, 193);
    let excess = sol.excess(target, Drain::NONE);
    assert_eq!(excess, 1);
}

proptest! {
    #[test]
    fn bnb_always_finds_solution_if_possible(num_inputs in 1usize..18, target_value in 0u64..10_000) {
        let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
        let wv = test_wv(&mut rng);
        let candidates = wv.take(num_inputs).collect::<Vec<_>>();
        let cs = CoinSelector::new(&candidates);

        let target = Target {
            outputs: TargetOutputs { value_sum: target_value, weight_sum: 0, n_outputs: 1 },
            fee: TargetFee::ZERO,
        };

        let solutions = cs.bnb_solutions(MinExcessThenWeight { target });

        match solutions.enumerate().filter_map(|(i, sol)| Some((i, sol?))).last() {
            Some((_i, (sol, _score))) => assert!(sol.selected_value() >= target_value),
            _ => prop_assert!(!cs.is_selection_possible(target)),
        }
    }

    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn bnb_always_finds_exact_solution_eventually(
        solution_len in 1usize..8,
        num_additional_canidates in 0usize..16,
        num_preselected in 0usize..8
    ) {
        let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
        let mut wv = test_wv(&mut rng);

        let solution: Vec<Candidate> = (0..solution_len).map(|_| wv.next().unwrap()).collect();
        let solution_weight = {
            let mut cs = CoinSelector::new(&solution);
            cs.select_all();
            cs.input_weight()
        };

        let target_value = solution.iter().map(|c| c.value).sum();

        let mut candidates = solution;
        candidates.extend(wv.take(num_additional_canidates));

        let mut cs = CoinSelector::new(&candidates);


        for i in 0..num_preselected.min(solution_len) {
            cs.select(i);
        }

        // sort in descending value
        cs.sort_candidates_by_key(|(_, wv)| core::cmp::Reverse(wv.value));

        let target = Target {
            outputs: TargetOutputs { value_sum: target_value, weight_sum: 0, n_outputs: 1 },
            // we're trying to find an exact selection value so set fees to 0
            fee: TargetFee::ZERO,
        };

        let solutions = cs.bnb_solutions(MinExcessThenWeight { target });

        let (_i, (best, _score)) = solutions
            .enumerate()
            .filter_map(|(i, sol)| Some((i, sol?)))
            .last()
            .expect("it found a solution");

        prop_assert!(best.input_weight() <= solution_weight);
        prop_assert_eq!(best.selected_value(), target.value());
    }
}
