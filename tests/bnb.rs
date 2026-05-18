mod common;
use bdk_coin_select::{
    float::Ordf32, BnbMetric, Candidate, CoinSelector, Drain, SelectionView, Target, TargetFee,
    TargetOutputs,
};
#[macro_use]
extern crate alloc;

use alloc::vec::Vec;
use proptest::{prelude::*, proptest, test_runner::*};

fn test_wv(mut rng: impl RngCore) -> impl Iterator<Item = Candidate> {
    core::iter::repeat_with(move || {
        let value = rng.random_range(0..1_000);
        let mut candidate = Candidate {
            value,
            weight: 100,
            input_count: rng.random_range(1..2),
            is_segwit: rng.random_bool(0.5),
        };
        // HACK: set is_segwit = true for all these tests because you can't actually lower bound
        // things easily with how segwit inputs interfere with their weights. We can't modify the
        // above since that would change what we pull from rng.
        candidate.is_segwit = true;
        candidate
    })
}

/// This is just an exhaustive search
struct MinExcessThenWeight;

/// Assumes tx weight is less than 1MB.
const EXCESS_RATIO: f32 = 1_000_000_f32;

impl BnbMetric for MinExcessThenWeight {
    fn score(&mut self, view: &SelectionView<'_>, target: Target) -> Option<Ordf32> {
        let excess = view.excess(target, Drain::NONE);
        if excess < 0 {
            None
        } else {
            Some(Ordf32(
                excess as f32 * EXCESS_RATIO + view.input_weight() as f32,
            ))
        }
    }

    fn bound(&mut self, view: &SelectionView<'_>, target: Target) -> Option<Ordf32> {
        let mut cs = view.selector().clone();
        cs.select_until_target_met(target).ok()?;
        Some(Ordf32(cs.compute_view().input_weight() as f32))
    }

    fn drain(&mut self, _view: &SelectionView<'_>, _target: Target) -> Drain {
        Drain::NONE
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
        cs.compute_view().input_weight()
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
        max_weight: None,
    };

    let solutions = cs.bnb_solutions(target, MinExcessThenWeight);

    let mut rounds = 0;
    let (best, score) = solutions
        .enumerate()
        .inspect(|(i, _)| rounds = *i + 1)
        .filter_map(|(_, sol)| sol)
        .last()
        .expect("it found a solution");

    assert_eq!(rounds, 3194);
    let best_view = best.compute_view();
    assert_eq!(best_view.input_weight(), solution_weight);
    assert_eq!(
        best_view.selected_value(),
        target_value,
        "score={:?}",
        score
    );
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
        max_weight: None,
    };

    let solutions = cs.bnb_solutions(target, MinExcessThenWeight);

    let mut rounds = 0;
    let (sol, _score) = solutions
        .enumerate()
        .inspect(|(i, _)| rounds = *i + 1)
        .filter_map(|(_, sol)| sol)
        .last()
        .expect("found a solution");

    assert_eq!(rounds, 164);
    let excess = sol.compute_view().excess(target, Drain::NONE);
    assert_eq!(excess, 0);
}

#[test]
/// The exclusion branch's same-(value, weight) dedup run must skip already-decided candidates
/// instead of banning them (a pre-selected candidate must never end up banned) or letting them
/// end the run early.
///
/// Regression test: candidate 1 is pre-selected by the caller and shares (value, weight) with
/// candidate 0. The optimal solution requires excluding candidate 0, and the dedup run following
/// that exclusion used to also ban the pre-selected candidate 1, contaminating the selector that
/// `run_bnb` hands back.
fn bnb_exclusion_dedup_skips_decided_candidates() {
    let candidates = vec![
        Candidate::new(500, 100, false), // same (value, weight) as the pre-selected candidate
        Candidate::new(500, 100, false), // pre-selected
        Candidate::new(400, 100, false),
    ];

    let mut cs = CoinSelector::new(&candidates);
    cs.select(1);

    let target = Target {
        outputs: TargetOutputs {
            value_sum: 900,
            weight_sum: 0,
            n_outputs: 1,
        },
        fee: TargetFee::ZERO,
        max_weight: None,
    };

    let _ = cs
        .run_bnb(target, MinExcessThenWeight, 1_000)
        .expect("must find solution");

    // Optimal selection is {1, 2}: candidate 1 is mandatory and adding candidate 2 hits the
    // target exactly, while any selection containing candidate 0 overshoots.
    assert_eq!(cs.selected_indices().iter().collect::<Vec<_>>(), vec![1, 2]);
    // The returned selector must not have any candidate simultaneously selected and banned.
    for (idx, _) in cs.selected() {
        assert!(
            !cs.banned().contains(idx),
            "candidate {} is both selected and banned",
            idx
        );
    }
}

proptest! {
    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn bnb_always_finds_solution_if_possible(num_inputs in 1usize..18, target_value in 0u64..10_000) {
        let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
        let wv = test_wv(&mut rng);
        let candidates = wv.take(num_inputs).collect::<Vec<_>>();
        let cs = CoinSelector::new(&candidates);

        let target = Target {
            outputs: TargetOutputs { value_sum: target_value, weight_sum: 0, n_outputs: 1 },
            fee: TargetFee::ZERO,
            max_weight: None,
        };

        let solutions = cs.bnb_solutions(target, MinExcessThenWeight);

        match solutions.enumerate().filter_map(|(i, sol)| Some((i, sol?))).last() {
            Some((_i, (sol, _score))) => assert!(sol.compute_view().selected_value() >= target_value),
            _ => prop_assert!(!cs.compute_view().is_fundable(target)),
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
            cs.compute_view().input_weight()
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
            max_weight: None,
        };

        let solutions = cs.bnb_solutions(target, MinExcessThenWeight);

        let (_i, (best, _score)) = solutions
            .enumerate()
            .filter_map(|(i, sol)| Some((i, sol?)))
            .last()
            .expect("it found a solution");

        let best_view = best.compute_view();
        prop_assert!(best_view.input_weight() <= solution_weight);
        prop_assert_eq!(best_view.selected_value(), target.value());
    }
}
