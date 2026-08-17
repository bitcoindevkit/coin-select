mod common;
use bdk_coin_select::{
    float::Ordf32, BnbMetric, Candidate, CoinSelector, Drain, SelectionProblem, SelectionView,
    Target, TargetFee, TargetOutputs,
};
#[macro_use]
extern crate alloc;

use alloc::vec::Vec;
use proptest::{prelude::*, proptest, test_runner::*};

fn test_wv(mut rng: impl RngCore) -> impl Iterator<Item = Candidate> {
    core::iter::repeat_with(move || {
        let value = rng.random_range(0..1_000);
        let candidate = Candidate {
            value,
            weight: 100,
            segwit_count: rng.random_range(1..2),
            legacy_count: 0,
        };
        // Keep drawing the bool these tests always drew so the rng stream (and therefore the
        // generated cases) is unchanged. All candidates are segwit: mixing in legacy inputs makes
        // their weights context-dependent, which these tests can't lower-bound easily.
        let _ = rng.random_bool(0.5);
        candidate
    })
}

/// This is just an exhaustive search
struct MinExcessThenWeight;

/// Assumes tx weight is less than 1MB.
const EXCESS_RATIO: f32 = 1_000_000_f32;

impl BnbMetric for MinExcessThenWeight {
    fn score(&mut self, cs: &SelectionView<'_>) -> Option<Ordf32> {
        let excess = cs.excess(Drain::NONE);
        if excess < 0 {
            None
        } else {
            Some(Ordf32(
                excess as f32 * EXCESS_RATIO + cs.input_weight() as f32,
            ))
        }
    }

    fn bound(&mut self, cs: &SelectionView<'_>) -> Option<Ordf32> {
        let mut cs = cs.selector().clone();
        cs.select_until_target_met().ok()?;
        Some(Ordf32(cs.compute_view().input_weight() as f32))
    }

    fn drain(&mut self, _cs: &SelectionView<'_>) -> Drain {
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
    let mut wv = test_wv(&mut rng);

    let solution: Vec<Candidate> = (0..solution_len).map(|_| wv.next().unwrap()).collect();
    let target_value = solution.iter().map(|c| c.value).sum();

    let mut candidates = solution.clone();
    candidates.extend(wv.take(num_additional_canidates));
    candidates.sort_unstable_by_key(|wv| core::cmp::Reverse(wv.value));

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

    let solution_weight = {
        let problem = SelectionProblem::new_no_ancestors(target, solution.iter().copied());
        let mut cs = CoinSelector::new(&problem);
        cs.select_all();
        cs.compute_view().input_weight()
    };

    let problem_2 = SelectionProblem::new_no_ancestors(target, candidates.iter().copied());
    let cs = CoinSelector::new(&problem_2);
    let solutions = cs.bnb_solutions(MinExcessThenWeight);

    let mut rounds = 0;
    let (best, score) = solutions
        .enumerate()
        .inspect(|(i, _)| rounds = *i + 1)
        .filter_map(|(_, sol)| sol)
        .last()
        .expect("it found a solution");

    // Unchanged by deepening, and deliberately so: this problem has no unconfirmed ancestors, so
    // `bnb_solutions` gates deepening off and dives. Deepening would in fact reach the same
    // exact-value solution here in 2,970 rounds — but the gate is set by what happens under a
    // *budget*, where re-expanding from the root costs a truncated ancestor-free search more than
    // the better node order wins it.
    assert_eq!(rounds, 62453);
    assert_eq!(best.compute_view().input_weight(), solution_weight);
    assert_eq!(
        best.compute_view().selected_value(),
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

    let target = Target {
        outputs: TargetOutputs {
            value_sum: target_value,
            weight_sum: 0,
            n_outputs: 1,
        },
        fee: TargetFee::default(),
        max_weight: None,
    };

    let problem_3 = SelectionProblem::new_no_ancestors(target, candidates.iter().copied());
    let cs = CoinSelector::new(&problem_3);
    let solutions = cs.bnb_solutions(MinExcessThenWeight);

    let mut rounds = 0;
    let (sol, _score) = solutions
        .enumerate()
        .inspect(|(i, _)| rounds = *i + 1)
        .filter_map(|(_, sol)| sol)
        .last()
        .expect("found a solution");

    assert_eq!(rounds, 95);
    let excess = sol.compute_view().excess(Drain::NONE);
    assert_eq!(excess, 0);
}

#[test]
fn exclusion_cursor_skips_preselected_equivalent_candidate() {
    let candidates = [
        Candidate::new_legacy(500, 100),
        Candidate::new_legacy(500, 100),
        Candidate::new_legacy(400, 100),
    ];
    let target = Target {
        outputs: TargetOutputs {
            value_sum: 900,
            weight_sum: 0,
            n_outputs: 1,
        },
        fee: TargetFee::ZERO,
        max_weight: None,
    };
    let problem = SelectionProblem::new_no_ancestors(target, candidates);
    let mut selector = problem.selector();
    selector.select(1);

    selector
        .run_bnb(MinExcessThenWeight, 1_000)
        .expect("must find a solution");

    assert_eq!(
        selector.selected_indices().iter().collect::<Vec<_>>(),
        vec![1, 2]
    );
    for (index, _) in selector.selected() {
        assert!(!selector.banned().contains(index));
    }
}

proptest! {
    #[test]
    #[cfg(not(debug_assertions))] // too slow if compiling for debug
    fn bnb_always_finds_solution_if_possible(num_inputs in 1usize..18, target_value in 0u64..10_000) {
        let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
        let wv = test_wv(&mut rng);
        let candidates = wv.take(num_inputs).collect::<Vec<_>>();

        let target = Target {
            outputs: TargetOutputs { value_sum: target_value, weight_sum: 0, n_outputs: 1 },
            fee: TargetFee::ZERO,
            max_weight: None,
        };
        let problem_4 = SelectionProblem::new_no_ancestors(target, candidates.iter().copied());
        let cs = CoinSelector::new(&problem_4);
        let solutions = cs.bnb_solutions(MinExcessThenWeight);

        match solutions.enumerate().filter_map(|(i, sol)| Some((i, sol?))).last() {
            Some((_i, (sol, _score))) => assert!(sol.compute_view().selected_value() >= target_value),
            _ => prop_assert!(!cs.compute_view().is_fundable()),
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
        let target_value = solution.iter().map(|c| c.value).sum();

        let mut candidates = solution.clone();
        candidates.extend(wv.take(num_additional_canidates));

        let target = Target {
            outputs: TargetOutputs { value_sum: target_value, weight_sum: 0, n_outputs: 1 },
            // we're trying to find an exact selection value so set fees to 0
            fee: TargetFee::ZERO,
            max_weight: None,
        };

        let solution_weight = {
            let problem_5 = SelectionProblem::new_no_ancestors(target, solution.iter().copied());
            let mut cs = CoinSelector::new(&problem_5);
            cs.select_all();
            cs.compute_view().input_weight()
        };

        let problem_6 = SelectionProblem::new_no_ancestors(target, candidates.iter().copied());
        let mut cs = CoinSelector::new(&problem_6);
        for i in 0..num_preselected.min(solution_len) {
            cs.select(i);
        }

        // sort in descending value
        cs.sort_candidates_by_key(|(_, wv)| core::cmp::Reverse(wv.value));

        let solutions = cs.bnb_solutions(MinExcessThenWeight);

        let (_i, (best, _score)) = solutions
            .enumerate()
            .filter_map(|(i, sol)| Some((i, sol?)))
            .last()
            .expect("it found a solution");

        prop_assert!(best.compute_view().input_weight() <= solution_weight);
        prop_assert_eq!(best.compute_view().selected_value(), target.value());
    }
}
