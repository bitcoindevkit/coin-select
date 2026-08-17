#![allow(unused_imports)]
//! Coin selection over candidates that drag in unconfirmed ancestors (CPFP).
//!
//! The invariant under test is that a selection's fee obligation includes the bump owed by the
//! **union** of the ancestors its selected candidates drag in — each ancestor charged exactly once,
//! weights and fees netted over the union — and that `LowestFee` branch and bound stays correct
//! under the resulting non-monotone funding.

mod common;

use bdk_coin_select::{
    float::Ordf32, metrics::LowestFee, AncestorToBump, Bitset, BnbMetric, Candidate, CoinSelector,
    Drain,
    DrainWeights, FeeRate, Input, Replace, SelectionProblem, Target, TargetFee, TargetOutputs,
    TX_FIXED_FIELD_WEIGHT,
};
use proptest::prelude::*;

/// Not a txid of any ancestor we pass in, so inputs residing on it are treated as confirmed.
const CONFIRMED: &str = "confirmed";

const P2WPKH_INPUT_WEIGHT: u64 = 272;

fn target(feerate_sat_per_vb: f32, value: u64) -> Target {
    Target {
        fee: TargetFee {
            rate: FeeRate::from_sat_per_vb(feerate_sat_per_vb),
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

fn input(value: u64, residing_txid: &'static str) -> Input<&'static str> {
    Input {
        value,
        weight: P2WPKH_INPUT_WEIGHT,
        is_segwit: true,
        residing_txid,
    }
}

fn ancestor(
    txid: &'static str,
    weight: u64,
    fee: u64,
    parents: Vec<&'static str>,
) -> AncestorToBump<&'static str> {
    AncestorToBump {
        txid,
        weight,
        fee,
        parents,
    }
}

fn metric() -> LowestFee {
    LowestFee {
        long_term_feerate: FeeRate::from_sat_per_vb(1.0),
        dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
        drain_weights: DrainWeights::TR_KEYSPEND,
    }
}

/// The bump is charged on top of the child's own feerate obligation, so it eats exactly that much
/// excess relative to the same selection with nothing unconfirmed behind it.
#[test]
fn bump_is_charged_on_top_of_the_childs_own_fee() {
    let t = target(10.0, 90_000);
    // 1000 wu at 10 sat/vb (2.5 sat/wu) => 2500 sats owed, and the ancestor pays nothing.
    let problem =
        SelectionProblem::new(t, [input(100_000, "P")], [ancestor("P", 1_000, 0, vec![])]);
    let mut cs = problem.selector();
    cs.select(0);

    assert_eq!(cs.compute_view().ancestor_bump(), 2_500);

    let no_ancestors = SelectionProblem::new_no_ancestors(
        t,
        [Candidate {
            value: 100_000,
            weight: P2WPKH_INPUT_WEIGHT,
            segwit_count: 1,
            legacy_count: 0,
        }],
    );
    let mut clean_cs = no_ancestors.selector();
    clean_cs.select(0);

    assert_eq!(clean_cs.compute_view().ancestor_bump(), 0);
    assert_eq!(
        cs.compute_view().weight(t.outputs, DrainWeights::NONE),
        clean_cs
            .compute_view()
            .weight(t.outputs, DrainWeights::NONE)
    );
    assert_eq!(
        cs.compute_view().excess(Drain::NONE),
        clean_cs.compute_view().excess(Drain::NONE) - 2_500,
        "the bump is the only difference between the two selections"
    );
    assert_eq!(
        cs.compute_view().implied_fee(DrainWeights::NONE),
        clean_cs.compute_view().implied_fee(DrainWeights::NONE) + 2_500
    );
}

/// An unconfirmed ancestor can cost more than the coin sitting on it is worth: funding is no longer
/// monotone in the selection.
#[test]
fn dragged_in_ancestor_can_unfund_a_selection() {
    let t = target(10.0, 90_000);
    let problem = SelectionProblem::new(
        t,
        [input(100_000, CONFIRMED), input(100_000, "P")],
        // 100_000 wu at 2.5 sat/wu => 250_000 sats owed: far more than the coin is worth.
        [ancestor("P", 100_000, 0, vec![])],
    );

    let mut clean_only = problem.selector();
    clean_only.select(0);
    assert!(clean_only.compute_view().is_funded());

    let mut both = problem.selector();
    both.select(0);
    both.select(1);
    assert!(
        !both.compute_view().is_funded(),
        "adding a coin with an expensive ancestor un-funds a funded selection"
    );
}

/// A shared ancestor is paid for once, no matter how many selected candidates drag it in — summing
/// the per-candidate `local_bump` figures would pay for it twice.
#[test]
fn shared_ancestor_is_charged_once() {
    let t = target(10.0, 10_000);
    let problem = SelectionProblem::new(
        t,
        [input(50_000, "P"), input(60_000, "P")],
        [ancestor("P", 1_000, 0, vec![])],
    );

    assert_eq!(problem.local_bump(0), 2_500);
    assert_eq!(problem.local_bump(1), 2_500);

    let mut cs = problem.selector();
    cs.select(0);
    cs.select(1);

    assert_eq!(cs.selected_ancestors().len(), 1);
    assert_eq!(cs.compute_view().ancestor_bump(), 2_500);
    assert_ne!(
        cs.compute_view().ancestor_bump(),
        problem.local_bump(0) + problem.local_bump(1)
    );
}

/// Deselecting one of two candidates that share an ancestor keeps the ancestor: it is still dragged
/// in by the other one.
#[test]
fn deselecting_keeps_an_ancestor_another_candidate_still_drags_in() {
    let t = target(10.0, 10_000);
    let problem = SelectionProblem::new(
        t,
        [
            input(50_000, "P"),
            input(60_000, "P"),
            input(70_000, CONFIRMED),
        ],
        [ancestor("P", 1_000, 0, vec![])],
    );
    let mut cs = problem.selector();

    cs.select(0);
    cs.select(1);
    assert_eq!(cs.compute_view().ancestor_bump(), 2_500);

    cs.deselect(0);
    assert_eq!(
        cs.compute_view().ancestor_bump(),
        2_500,
        "candidate 1 still drags in P"
    );

    cs.select(2);
    assert_eq!(
        cs.compute_view().ancestor_bump(),
        2_500,
        "a confirmed coin drags in nothing"
    );

    cs.deselect(1);
    assert_eq!(
        cs.compute_view().ancestor_bump(),
        0,
        "nothing selected drags in P anymore"
    );
}

/// The whole transitive chain is charged, and fees are netted across it (not per ancestor).
#[test]
fn transitive_ancestors_are_netted_as_one_package() {
    let t = target(10.0, 10_000);
    let problem = SelectionProblem::new(
        t,
        [input(50_000, "B")],
        [
            ancestor("A", 400, 0, vec![]),
            ancestor("B", 400, 1_000, vec!["A"]),
        ],
    );
    let mut cs = problem.selector();
    cs.select(0);

    // Union: weight 800 => 2000 sats owed at 2.5 sat/wu, of which B already paid 1000.
    assert_eq!(cs.selected_ancestors().len(), 2);
    assert_eq!(cs.compute_view().ancestor_bump(), 1_000);
}

/// Dragging in an ancestor that overpays *lowers* what the selection owes, because the deficit is
/// netted over the union. This is what makes a funded selection's fee a bad lower bound for its
/// descendants.
#[test]
fn overpaying_ancestor_offsets_an_underpaying_one() {
    let t = target(1.0, 10_000); // 0.25 sat/wu
    let problem = SelectionProblem::new(
        t,
        [input(50_000, "RICH"), input(50_000, "POOR")],
        [
            ancestor("RICH", 400, 10_000, vec![]),
            ancestor("POOR", 400, 0, vec![]),
        ],
    );

    let mut poor_only = problem.selector();
    poor_only.select(1);
    assert_eq!(poor_only.compute_view().ancestor_bump(), 100);

    let mut rich_only = problem.selector();
    rich_only.select(0);
    assert_eq!(
        rich_only.compute_view().ancestor_bump(),
        0,
        "never credits the child"
    );

    let mut both = problem.selector();
    both.select(0);
    both.select(1);
    assert_eq!(
        both.compute_view().ancestor_bump(),
        0,
        "RICH's surplus covers POOR's deficit, so the superset owes less"
    );
}

/// Ancestor weight is not part of the child transaction, so it must not count against
/// [`Target::max_weight`].
#[test]
fn ancestor_weight_does_not_count_against_max_weight() {
    let mut t = target(10.0, 10_000);
    let heavy = 100_000;
    let problem = SelectionProblem::new(
        t,
        [input(50_000, "P")],
        [ancestor("P", heavy, heavy, vec![])],
    );
    let mut cs = problem.selector();
    cs.select(0);

    let child_weight = cs.compute_view().weight(t.outputs, DrainWeights::NONE);
    assert!(child_weight < heavy);

    t.max_weight = Some(child_weight);
    let capped = SelectionProblem::new(
        t,
        [input(50_000, "P")],
        [ancestor("P", heavy, heavy, vec![])],
    );
    let mut capped_cs = capped.selector();
    capped_cs.select(0);
    assert!(capped_cs
        .compute_view()
        .is_within_max_weight(DrainWeights::NONE));
}

/// Two coins of equal value and weight are *not* interchangeable when only one of them drags in an
/// ancestor, so branch and bound must not ban them as a group.
///
/// Here the only fundable selection is the clean coin alone, and it sits *after* the coin with the
/// expensive ancestor in the search order (equal value-per-weight, so the sort is stable). If the
/// exclusion branch banned it along with its look-alike, the search would report no solution.
#[test]
fn look_alikes_with_different_ancestors_are_not_banned_together() {
    let t = target(10.0, 90_000);
    let problem = SelectionProblem::new(
        t,
        [input(100_000, "P"), input(100_000, CONFIRMED)],
        [ancestor("P", 100_000, 0, vec![])],
    );

    assert_eq!(problem.candidate(0).value, problem.candidate(1).value);
    assert_eq!(problem.candidate(0).weight, problem.candidate(1).weight);

    let mut cs = problem.selector();
    let (_score, _drain) = cs
        .run_bnb(metric(), 100_000)
        .expect("the clean coin funds the target on its own");

    assert!(cs.is_selected(1));
    assert!(!cs.is_selected(0));
}

/// The bump has to be inside the fee the metric reports, not added on top of it.
#[test]
fn score_is_the_childs_fee_which_already_covers_the_bump() {
    let t = target(10.0, 90_000);
    let problem =
        SelectionProblem::new(t, [input(100_000, "P")], [ancestor("P", 1_000, 0, vec![])]);
    let mut cs = problem.selector();
    cs.select(0);

    let mut m = metric();
    let score = m.score(&cs.compute_view()).expect("funded");
    let drain = m.drain(&cs.compute_view());
    assert_eq!(
        score,
        Ordf32(
            (cs.compute_view().fee(t.value(), drain.value) as u64
                + drain.weights.spend_fee(m.long_term_feerate)) as f32
        )
    );
    assert!(
        cs.compute_view().fee(t.value(), drain.value) as u64 >= cs.compute_view().ancestor_bump(),
        "a funded selection's child fee covers the bump"
    );
}

// --- the bump lower bound used by `LowestFee`'s bound ---

/// With nothing overpaying within reach, no descendant can owe less than this selection does, so the
/// lower bound is the full bump — the figure branch and bound gets to keep.
#[test]
fn bump_lower_bound_is_the_full_bump_when_nothing_overpays() {
    let t = target(10.0, 10_000);
    let problem = SelectionProblem::new(
        t,
        [
            input(50_000, "P"),
            input(60_000, "Q"),
            input(70_000, CONFIRMED),
        ],
        [
            ancestor("P", 1_000, 0, vec![]),
            ancestor("Q", 2_000, 0, vec![]),
        ],
    );

    let mut cs = problem.selector();
    cs.select(0);
    assert_eq!(cs.compute_view().ancestor_bump(), 2_500);
    assert_eq!(
        cs.compute_view().ancestor_bump_lower_bound(),
        2_500,
        "Q only ever adds to what is owed, so it cannot lower the floor"
    );

    // The reachable-but-unselected ancestors are exactly Q's.
    let addable: Vec<_> = cs.addable_ancestors().iter().collect();
    assert_eq!(addable, vec![1]);
}

/// A reachable ancestor that overpays is exactly what a descendant could use to owe less, so the
/// bound gives up precisely that surplus and no more.
#[test]
fn bump_lower_bound_gives_up_the_reachable_surplus() {
    let t = target(1.0, 10_000); // 0.25 sat/wu
    let problem = SelectionProblem::new(
        t,
        [input(50_000, "POOR"), input(50_000, "RICH")],
        [
            ancestor("POOR", 4_000, 0, vec![]),    // owes 1_000
            ancestor("RICH", 400, 10_000, vec![]), // overpays by 9_900
        ],
    );

    let mut cs = problem.selector();
    cs.select(0);
    assert_eq!(cs.compute_view().ancestor_bump(), 1_000);
    assert_eq!(
        cs.compute_view().ancestor_bump_lower_bound(),
        0,
        "RICH's 9_900 surplus swamps the 1_000 owed"
    );

    // Which is not pessimism: that descendant really does owe nothing.
    let mut both = cs.clone();
    both.select(1);
    assert_eq!(both.compute_view().ancestor_bump(), 0);
}

/// Only the surplus actually within reach is given up.
#[test]
fn bump_lower_bound_only_credits_reachable_surplus() {
    let t = target(1.0, 10_000); // 0.25 sat/wu
    let problem = SelectionProblem::new(
        t,
        [input(50_000, "POOR"), input(50_000, "RICH")],
        [
            ancestor("POOR", 4_000, 0, vec![]),   // owes 1_000
            ancestor("RICH", 400, 1_100, vec![]), // overpays by 1_000
        ],
    );

    let mut cs = problem.selector();
    cs.select(0);
    assert_eq!(cs.compute_view().ancestor_bump(), 1_000);
    assert_eq!(cs.compute_view().ancestor_bump_lower_bound(), 0);

    // Ban the coin that would bring RICH in and the surplus is out of reach again.
    let mut banned = cs.clone();
    banned.ban(1);
    assert!(banned.addable_ancestors().is_empty());
    assert_eq!(banned.compute_view().ancestor_bump_lower_bound(), 1_000);

    // Likewise once there is nothing left to add.
    let mut exhausted = cs.clone();
    exhausted.select(1);
    assert!(exhausted.is_exhausted());
    assert_eq!(
        exhausted.compute_view().ancestor_bump_lower_bound(),
        exhausted.compute_view().ancestor_bump()
    );
}

/// The whole point: the fee floor `LowestFee` bounds with actually charges for the ancestors.
#[test]
fn bound_credits_the_bump_when_nothing_overpays() {
    let t = target(10.0, 90_000);
    let problem = SelectionProblem::new(
        t,
        [input(100_000, "P"), input(100_000, CONFIRMED)],
        [ancestor("P", 1_000, 0, vec![])],
    );

    let mut cs = problem.selector();
    cs.select(0);

    let child_fee = t
        .fee
        .rate
        .implied_fee_wu(cs.compute_view().weight(t.outputs, DrainWeights::NONE));
    let bound = metric()
        .bound(&cs.compute_view())
        .expect("within max_weight");
    assert!(
        bound >= Ordf32((child_fee + 2_500) as f32),
        "bound {} must charge the child's own fee ({}) plus the 2_500 bump",
        bound,
        child_fee
    );
}

#[test]
fn bump_lower_bound_accounts_for_large_f32_fee_rounding() {
    let t = target(172.0, 1_000); // exactly 43 sat/wu
    let problem = SelectionProblem::new(
        t,
        [input(20_000_000, "P")],
        [ancestor("P", 399_999, 0, vec![])],
    );
    let mut cs = problem.selector();
    cs.select(0);

    assert!(
        cs.compute_view().ancestor_bump_lower_bound() <= cs.compute_view().ancestor_bump(),
        "the f64 relaxation must not exceed the f32 fee obligation"
    );
    let view = cs.compute_view();
    assert!(view.ancestor_bump_lower_bound() <= view.ancestor_bump());
}

/// An ancestor only one candidate can reach is folded into that candidate up front; the rest are
/// left to be de-duplicated per selection.
#[test]
fn ancestors_are_split_into_private_and_shared() {
    let t = target(10.0, 10_000);
    let problem = SelectionProblem::new(
        t,
        [
            vec![input(50_000, "MINE")],
            vec![input(50_000, "OURS")],
            vec![input(50_000, "OURS")],
        ],
        [
            ancestor("MINE", 1_000, 7, vec![]),
            ancestor("OURS", 2_000, 9, vec![]),
        ],
    );

    assert!(problem.has_shared_ancestors());

    // Candidate 0 is the only one that can reach MINE, so it is charged for it directly.
    assert_eq!(problem.private_ancestors(0), (1_000, 7));
    assert!(problem.shared_drags_in(0).is_empty());

    // OURS is reachable two ways, so it stays in the shared set for both.
    assert_eq!(problem.private_ancestors(1), (0, 0));
    assert_eq!(problem.private_ancestors(2), (0, 0));
    assert_eq!(
        problem.shared_drags_in(1),
        &[1_u32]
    );
    assert_eq!(
        problem.shared_drags_in(2),
        &[1_u32]
    );

    // Either way `drags_in` still describes the full truth.
    assert_eq!(problem.drags_in(0), &[0_u32]);
    assert_eq!(problem.drags_in(1), &[1_u32]);

    // And a problem where nothing is shared says so, which is what lets the bump skip
    // de-duplication entirely.
    let unshared = SelectionProblem::new(
        t,
        [input(50_000, "MINE")],
        [ancestor("MINE", 1_000, 0, vec![])],
    );
    assert!(!unshared.has_shared_ancestors());
    assert!(unshared.has_ancestors());
}

/// A funded node's bound must give up the surplus a descendant could still pick up — otherwise it
/// sits above that descendant's score.
#[test]
fn funded_bound_gives_up_reachable_surplus() {
    let t = target(1.0, 10_000); // 0.25 sat/wu
    let problem = SelectionProblem::new(
        t,
        [input(50_000, "POOR"), input(50_000, "RICH")],
        [
            ancestor("POOR", 4_000, 0, vec![]),    // owes 1_000
            ancestor("RICH", 400, 10_000, vec![]), // overpays by 9_900
        ],
    );

    let mut cs = problem.selector();
    cs.select(0);
    assert!(cs.compute_view().is_funded());
    assert_eq!(cs.compute_view().ancestor_bump(), 1_000);
    assert_eq!(cs.compute_view().ancestor_bump_lower_bound(), 0);

    let score = metric().score(&cs.compute_view()).unwrap();
    let bound = metric().bound(&cs.compute_view()).unwrap();
    assert!(
        bound <= Ordf32(score.0 - 1_000.0),
        "bound {} must sit at least the 1_000 surplus below score {}",
        bound,
        score
    );

    let mut both = cs.clone();
    both.select(1);
    let both_score = metric().score(&both.compute_view()).unwrap();
    assert!(
        bound <= both_score,
        "bound {} above descendant score {}",
        bound,
        both_score
    );
}

/// Subtracting two large `f32`s can round the bound upward. Surplus is therefore subtracted in
/// integer space before the result is converted to the metric's `f32` score.
#[test]
fn funded_bound_subtracts_surplus_before_float_conversion() {
    let t = Target {
        fee: TargetFee {
            rate: FeeRate::from_sat_per_vb(20_000.0), // 5_000 sat/wu
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
    let problem = SelectionProblem::new(
        t,
        [
            Input {
                value: 1_998_700_000,
                weight: 0,
                is_segwit: false,
                residing_txid: "POOR",
            },
            Input {
                value: 0,
                weight: 0,
                is_segwit: false,
                residing_txid: "RICH",
            },
        ],
        [
            ancestor("POOR", 400_000, 2_000_000, vec![]), // owes 1_998_000_000
            ancestor("RICH", 0, 1_998_000_000, vec![]),   // cancels POOR exactly
        ],
    );
    let mut metric = LowestFee {
        long_term_feerate: FeeRate::from_sat_per_vb(1.0),
        dust_relay_feerate: FeeRate::from_sat_per_vb(1.0),
        drain_weights: DrainWeights::NONE,
    };

    let mut node = problem.selector();
    node.select(0);
    assert_eq!(node.compute_view().ancestor_bump(), 1_998_000_000);
    assert_eq!(node.compute_view().ancestor_bump_lower_bound(), 0);
    let bound = metric.bound(&node.compute_view()).unwrap();

    let mut descendant = node.clone();
    descendant.select(1);
    let score = metric.score(&descendant.compute_view()).unwrap();
    assert_eq!(score, Ordf32(700_000.0));
    assert!(bound <= score, "bound {} above descendant {}", bound, score);
}

/// Selecting everything can un-fund, but that must not make the bound claim the subtree is empty.
#[test]
fn unfunded_bound_does_not_claim_infeasibility() {
    let t = target(10.0, 90_000);
    let problem = SelectionProblem::new(
        t,
        [input(100_000, CONFIRMED), input(100_000, "P")],
        [ancestor("P", 100_000, 0, vec![])],
    );

    let cs = problem.selector();
    assert!(!cs.compute_view().is_funded());
    assert!(
        metric().bound(&cs.compute_view()).is_some(),
        "an unfunded root with a live funded subset must not be pruned"
    );
}

/// Existing package surplus can pay a later candidate's private deficit. Pricing that deficit as
/// the candidate's marginal cost would put the bound above the descendant's score.
#[test]
fn unfunded_bound_credits_selected_package_surplus() {
    let t = target(1.0, 100_000); // 0.25 sat/wu
    let problem = SelectionProblem::new(
        t,
        [input(60_000, "RICH"), input(50_000, "POOR")],
        [
            ancestor("RICH", 400, 10_000, vec![]), // surplus 9_900
            ancestor("POOR", 4_000, 0, vec![]),    // deficit 1_000
        ],
    );

    let mut node = problem.selector();
    node.select(0);
    assert!(!node.compute_view().is_funded());

    let bound = metric().bound(&node.compute_view()).unwrap();
    let mut descendant = node.clone();
    descendant.select(1);
    let score = metric().score(&descendant.compute_view()).unwrap();
    assert!(
        bound <= score,
        "bound {} above package-subsidized descendant {}",
        bound,
        score
    );
}

/// The absolute fee is already the final child fee floor; the resize must not add target-rate
/// marginal cost on top of it.
#[test]
fn unfunded_bound_does_not_double_count_absolute_fee() {
    let mut t = target(1.0, 100_000);
    t.fee.absolute = 5_000;
    let problem =
        SelectionProblem::new(t, [input(105_000, "P")], [ancestor("P", 4_000, 0, vec![])]);

    let root = problem.selector();
    let bound = metric().bound(&root.compute_view()).unwrap();
    let mut descendant = root.clone();
    descendant.select(0);
    let score = metric().score(&descendant.compute_view()).unwrap();
    assert_eq!(score, Ordf32(5_000.0));
    assert!(bound <= score, "bound {} above descendant {}", bound, score);
}

/// RBF rule 4 prices only child weight. Ancestor weight must not enter its effective value, and the
/// replacement floor must not be charged twice.
#[test]
fn unfunded_bound_does_not_double_count_rbf_fee() {
    let mut t = target(1.0, 100_000);
    t.fee.replace = Some(Replace {
        fee: 5_000,
        incremental_relay_feerate: FeeRate::from_sat_per_vb(1.0),
    });
    let problem =
        SelectionProblem::new(t, [input(105_104, "P")], [ancestor("P", 4_000, 0, vec![])]);

    let root = problem.selector();
    let bound = metric().bound(&root.compute_view()).unwrap();
    let mut descendant = root.clone();
    descendant.select(0);
    let score = metric().score(&descendant.compute_view()).unwrap();
    assert_eq!(score, Ordf32(5_104.0));
    assert!(bound <= score, "bound {} above descendant {}", bound, score);
}

/// Surplus cannot be cherry-picked: an ancestor arrives only by selecting a candidate, which drags
/// in that candidate's whole chain. So a coin whose parent overpays but whose grandparent does not
/// offers no way to owe less, and the bound must not pretend otherwise.
#[test]
fn bump_lower_bound_nets_ancestors_that_must_arrive_together() {
    let t = target(1.0, 10_000); // 0.25 sat/wu
    let problem = SelectionProblem::new(
        t,
        [input(50_000, "POOR"), input(50_000, "RICH")],
        [
            ancestor("POOR", 4_000, 0, vec![]), // owes 1_000
            // Selecting the second coin brings RICH *and* its unpaid parent GRAN.
            ancestor("GRAN", 8_000, 0, vec![]), // owes 2_000
            ancestor("RICH", 400, 10_000, vec!["GRAN"]), // overpays by 9_900
        ],
    );

    let mut cs = problem.selector();
    cs.select(0);
    assert_eq!(cs.compute_view().ancestor_bump(), 1_000);

    // RICH's 9_900 surplus is real, but only comes with GRAN's 2_000 deficit: still a net surplus.
    assert_eq!(cs.compute_view().ancestor_bump_lower_bound(), 0);
    let mut both = cs.clone();
    both.select(1);
    assert_eq!(
        both.compute_view().ancestor_bump(),
        0,
        "that descendant really owes nothing"
    );

    // Now make the chain's deficit outweigh the surplus. Crediting RICH alone would wrongly drop the
    // bound to 0; netting the chain keeps the full bump.
    let deep = SelectionProblem::new(
        t,
        [input(50_000, "POOR"), input(50_000, "RICH")],
        [
            ancestor("POOR", 4_000, 0, vec![]),
            ancestor("GRAN", 80_000, 0, vec![]), // owes 20_000
            ancestor("RICH", 400, 10_000, vec!["GRAN"]),
        ],
    );
    let mut cs = deep.selector();
    cs.select(0);
    assert_eq!(cs.compute_view().ancestor_bump(), 1_000);
    assert_eq!(
        cs.compute_view().ancestor_bump_lower_bound(),
        1_000,
        "taking RICH means taking GRAN, which costs far more than RICH's surplus is worth"
    );

    let mut both = cs.clone();
    both.select(1);
    assert!(
        both.compute_view().ancestor_bump() > 1_000,
        "confirmed by the descendant, which owes more, not less"
    );
}

/// An ancestor several candidates can reach cannot be tied to any one of them, so its surplus is
/// credited on its own rather than netted against a particular candidate's other ancestors.
#[test]
fn bump_lower_bound_credits_shared_surplus_on_its_own() {
    let t = target(1.0, 10_000); // 0.25 sat/wu
    let problem = SelectionProblem::new(
        t,
        [
            vec![input(50_000, "POOR")],
            // Both of these reach RICH; the second also drags in its own expensive chain.
            vec![input(50_000, "RICH")],
            vec![input(50_000, "RICH"), input(50_000, "HEAVY")],
        ],
        [
            ancestor("POOR", 4_000, 0, vec![]),    // owes 1_000
            ancestor("RICH", 400, 10_000, vec![]), // overpays by 9_900
            ancestor("HEAVY", 80_000, 0, vec![]),  // owes 20_000
        ],
    );

    let mut cs = problem.selector();
    cs.select(0);
    assert_eq!(cs.compute_view().ancestor_bump(), 1_000);
    assert_eq!(
        cs.compute_view().ancestor_bump_lower_bound(),
        0,
        "RICH is reachable without HEAVY, so its surplus counts"
    );
}

// --- randomized cross-checks ---

/// Spec for a randomly generated ancestor problem. Indices are taken modulo the relevant length so
/// any combination of generated numbers describes a valid (acyclic) problem.
#[derive(Debug, Clone)]
struct AncestorProblemSpec {
    /// `(value, weight, residing_txid_selector)` per candidate.
    candidates: Vec<(u64, u64, usize)>,
    /// `(weight, fee, parent_selector)` per unconfirmed ancestor.
    ancestors: Vec<(u64, u64, usize)>,
    target_value: u64,
    feerate: f32,
    max_weight: Option<u64>,
}

impl AncestorProblemSpec {
    fn build(&self) -> SelectionProblem {
        let n_anc = self.ancestors.len();

        let ancestors: Vec<AncestorToBump<usize>> = self
            .ancestors
            .iter()
            .enumerate()
            .map(|(i, &(weight, fee, parent_sel))| {
                // Parents are strictly earlier ancestors (keeps the graph acyclic); selecting `i`
                // itself means "no unconfirmed parent".
                let parent = parent_sel % (i + 1);
                AncestorToBump {
                    txid: i,
                    weight,
                    fee,
                    parents: if parent == i { vec![] } else { vec![parent] },
                }
            })
            .collect();

        let inputs: Vec<Input<usize>> = self
            .candidates
            .iter()
            .map(|&(value, weight, residing_sel)| Input {
                value,
                weight,
                is_segwit: true,
                // `n_anc` means the coin sits on a confirmed tx (no matching txid).
                residing_txid: residing_sel % (n_anc + 1),
            })
            .collect();

        let mut t = target(self.feerate, self.target_value);
        t.max_weight = self.max_weight;
        SelectionProblem::new(t, inputs, ancestors)
    }
}

fn spec_strategy() -> impl Strategy<Value = AncestorProblemSpec> {
    (
        prop::collection::vec((1_000u64..200_000, 200u64..1_000, 0usize..8), 1..6),
        prop::collection::vec((200u64..4_000, 0u64..3_000, 0usize..8), 0..4),
        10_000u64..400_000,
        1.0f32..30.0,
        proptest::option::of(400u64..3_000),
    )
        .prop_map(
            |(candidates, ancestors, target_value, feerate, max_weight)| AncestorProblemSpec {
                candidates,
                ancestors,
                target_value,
                feerate,
                max_weight,
            },
        )
}

/// Independently computed bump for a selection, straight from the definition: union the ancestor
/// sets of the selected candidates, sum weight and fee over that union, and take the shortfall.
fn expected_bump(problem: &SelectionProblem, cs: &CoinSelector<'_>, feerate: FeeRate) -> u64 {
    let mut union = std::collections::BTreeSet::new();
    for i in cs.selected_indices().iter() {
        union.extend(problem.drags_in(i).iter().map(|&a| a as usize));
    }
    let (weight, fee) = union
        .iter()
        .map(|&i| problem.ancestors()[i])
        .fold((0u64, 0u64), |(w, f), (aw, af)| (w + aw, f + af));
    feerate.implied_fee_wu(weight).saturating_sub(fee)
}

proptest! {
    /// Every selection's bump must equal the union-derived figure — in particular it must never be
    /// the sum of the per-candidate `local_bump`s when ancestors are shared.
    #[test]
    fn bump_matches_union_definition(spec in spec_strategy()) {
        let problem = spec.build();
        let feerate = problem.target().fee.rate;
        let cs = problem.selector();

        prop_assert_eq!(cs.compute_view().ancestor_bump(), expected_bump(&problem, &cs, feerate));

        for (node, _) in common::ExhaustiveIter::new(&cs).into_iter().flatten() {
            prop_assert_eq!(
                node.compute_view().ancestor_bump(),
                expected_bump(&problem, &node, feerate),
                "selection={}", node
            );
        }
    }

    /// The bump lower bound must hold for the whole subtree, which is what lets the fee floor credit
    /// it: no selection reachable from a node may owe less than the node's bound says.
    #[test]
    fn bump_lower_bound_holds_for_every_descendant(spec in spec_strategy()) {
        let problem = spec.build();
        let root = problem.selector();

        let nodes = std::iter::once(root.clone()).chain(
            common::ExhaustiveIter::new(&root)
                .into_iter()
                .flatten()
                .map(|(node, _)| node),
        );

        for node in nodes {
            let lower_bound = node.compute_view().ancestor_bump_lower_bound();
            prop_assert!(
                lower_bound <= node.compute_view().ancestor_bump(),
                "node={} lb={} owes={}", node, lower_bound, node.compute_view().ancestor_bump()
            );

            for (descendant, inclusion) in common::ExhaustiveIter::new(&node).into_iter().flatten() {
                if !inclusion {
                    continue;
                }
                prop_assert!(
                    lower_bound <= descendant.compute_view().ancestor_bump(),
                    "node={} lb={} descendant={} owes={}",
                    node, lower_bound, descendant, descendant.compute_view().ancestor_bump()
                );
            }
        }
    }

    /// The bound must never exceed the score of any selection in its subtree (else branch and bound
    /// can prune the optimum), and `None` must really mean "nothing in this subtree is valid".
    #[test]
    fn bound_is_admissible_with_ancestors(spec in spec_strategy()) {
        let problem = spec.build();
        let mut metric = metric();

        let mut root = problem.selector();
        if metric.requires_ordering_by_descending_value_pwu() {
            root.sort_candidates_by_descending_value_pwu();
        }

        let nodes = std::iter::once(root.clone()).chain(
            common::ExhaustiveIter::new(&root)
                .into_iter()
                .flatten()
                .map(|(node, _)| node),
        );

        for node in nodes {
            let bound = metric.bound(&node.compute_view());
            let subtree = std::iter::once(node.clone()).chain(
                common::ExhaustiveIter::new(&node)
                    .into_iter()
                    .flatten()
                    .filter(|(_, inclusion)| *inclusion)
                    .map(|(descendant, _)| descendant),
            );

            for descendant in subtree {
                let score = metric.score(&descendant.compute_view());
                match bound {
                    Some(lb) => if let Some(score) = score {
                        prop_assert!(
                            score >= lb,
                            "bound too tight: node={} lb={} descendant={} score={}",
                            node, lb, descendant, score
                        );
                    },
                    None => prop_assert!(
                        score.is_none(),
                        "pruned a subtree with a solution: node={} descendant={} score={:?}",
                        node, descendant, score
                    ),
                }
            }
        }
    }

    /// With unlimited rounds, branch and bound must land on the same optimum as brute force — both
    /// the score and the feasibility verdict.
    #[test]
    fn bnb_finds_the_brute_force_optimum(spec in spec_strategy()) {
        let problem = spec.build();

        let mut exhaustive_cs = problem.selector();
        let mut exhaustive_metric = metric();
        let expected = common::exhaustive_search(&mut exhaustive_cs, &mut exhaustive_metric);

        let mut bnb_cs = problem.selector();
        let found = common::bnb_search(&mut bnb_cs, metric(), usize::MAX);

        match (expected, found) {
            (Some((expected_score, _)), Ok((score, _))) => {
                prop_assert_eq!(score, expected_score, "bnb={} exhaustive={}", bnb_cs, exhaustive_cs);
            }
            (None, Err(_)) => {}
            (expected, found) => prop_assert!(
                false,
                "disagreement: exhaustive={:?} bnb={:?}",
                expected.map(|(score, _)| score),
                found.map(|(score, _)| score),
            ),
        }
    }

}

/// Deepening escapes a dive the candidate order misleads, which is why it is the default.
///
/// Two groups, each a fat underpaying root spent by one tip that overpays and one that underpays.
/// Coins on the same root **share** its bump, so what a coin costs depends on which others are
/// already selected — and value-per-weight, the order the dive descends in, cannot see that at all.
/// The dive commits to spreading across both roots and then prunes against that incumbent for the
/// rest of its budget.
///
/// This is the in-tree version of what the external 42-fixture benchmark measures, and a regression
/// guard on the *reason* deepening is on by default rather than on any particular round count.
#[test]
fn deepening_escapes_a_dive_that_the_candidate_order_misleads() {
    let mut ancestors = Vec::new();
    let mut tips: Vec<&'static str> = Vec::new();
    for (root, rich, poor) in [("root0", "rich0", "poor0"), ("root1", "rich1", "poor1")] {
        // The root is fat and pays almost nothing, so the whole group needs bumping even though
        // the rich tip pays well over the target rate on its own.
        ancestors.push(ancestor(root, 8_000, 500, vec![]));
        ancestors.push(ancestor(rich, 1_200, 1_200 * 10, vec![root]));
        ancestors.push(ancestor(poor, 1_200, 100, vec![root]));
        tips.push(rich);
        tips.push(poor);
    }

    let mut inputs = Vec::new();
    for k in 0..10u64 {
        inputs.push(input(100_000 - k * 300, tips[k as usize % tips.len()]));
    }
    for k in 0..10u64 {
        inputs.push(input(97_000 - k * 300, CONFIRMED));
    }
    let total: u64 = inputs.iter().map(|i| i.value).sum();
    let problem = SelectionProblem::new(target(10.0, total * 60 / 100), inputs, ancestors);

    const BUDGET: usize = 600;
    let score_of = |mut iter: Box<dyn Iterator<Item = Option<(CoinSelector, Ordf32)>> + '_>| {
        iter.by_ref()
            .take(BUDGET)
            .flatten()
            .last()
            .expect("the greedy seed always scores")
            .1
    };

    let dive_cs = problem.selector();
    let dive = score_of(Box::new(dive_cs.bnb_solutions_dive_only(metric())));
    let hybrid_cs = problem.selector();
    let hybrid = score_of(Box::new(hybrid_cs.bnb_solutions(metric())));

    assert!(
        hybrid < dive,
        "deepening should beat the plain dive on the shape it exists for: dive={} hybrid={}",
        dive,
        hybrid,
    );
    // Guard the size of the win, not the round count: a change that keeps deepening enabled but
    // makes it converge later would otherwise pass silently.
    let gain = (dive.0 - hybrid.0) / dive.0;
    assert!(
        gain > 0.05,
        "the win shrank to {:.1}%, which is small enough to be worth re-justifying the default",
        gain * 100.0,
    );
}

/// The one thing no candidate sort key can say: a coin is cheap only because another selected coin
/// already pays for its parent.
///
/// Here `A` and `B` share `P`, while `C` is alone on `Q`. A selection of `{A, C}` pays both bumps.
/// Swapping `C` for `B` keeps the same total value and drops `Q` entirely — but no fixed order over
/// individual coins can prefer `B` to `C`, because they are identical until `A` is selected.
/// `repair` is what closes that, so this is the case it exists for.
#[test]
fn repair_drops_a_coin_that_pays_for_an_ancestor_alone() {
    let t = target(10.0, 100_000);
    let problem = SelectionProblem::new(
        t,
        [
            input(80_000, "P"),       // 0: A
            input(80_000, "P"),       // 1: B, indistinguishable from A on any per-coin key
            input(80_000, "Q"),       // 2: C, the only candidate that can drag in Q
            input(20_000, CONFIRMED), // 3: D, too small to fund the target in anyone's place
        ],
        [
            ancestor("P", 4_000, 0, vec![]),
            ancestor("Q", 4_000, 0, vec![]),
        ],
    );
    assert!(problem.has_shared_ancestors(), "P is reachable from both A and B");

    let mut cs = problem.selector();
    cs.select(0);
    cs.select(2);
    let before = metric().score(&cs.compute_view()).expect("{A, C} is funded");
    assert_eq!(cs.compute_view().ancestor_bump(), 20_000, "P and Q both charged");

    let after = cs.repair(&mut metric(), 100, &Bitset::default()).expect("C can be swapped for B");

    assert!(after < before, "{} is not an improvement on {}", after, before);
    assert!(!cs.is_selected(2), "C still holds Q alone");
    assert!(cs.is_selected(1), "B is the replacement: same value, and P is already paid for");
    assert_eq!(cs.compute_view().ancestor_bump(), 10_000, "Q is gone, P charged once");
    assert_eq!(
        before.0 - after.0,
        10_000.0,
        "the whole improvement is the bump that stopped being owed",
    );
}

/// With every ancestor reachable from one candidate there is nothing set-dependent for the order to
/// have got wrong, so the pass is a pure cost and declines to run.
#[test]
fn repair_declines_when_no_ancestor_is_shared() {
    let t = target(10.0, 100_000);
    let problem = SelectionProblem::new(
        t,
        [
            input(80_000, "P"),
            input(80_000, "Q"),
            input(79_000, CONFIRMED),
        ],
        [
            ancestor("P", 4_000, 0, vec![]),
            ancestor("Q", 4_000, 0, vec![]),
        ],
    );
    assert!(!problem.has_shared_ancestors());

    let mut cs = problem.selector();
    cs.select(0);
    cs.select(1);
    assert!(cs.repair(&mut metric(), 100, &Bitset::default()).is_none());
    assert!(cs.is_selected(0) && cs.is_selected(1), "the selection is untouched");
}

/// `repair` trials a swap by mutating one cached view and undoing it, thousands of times over. The
/// cache carries floating-point accumulators, so if `add` and `sub` are not exact inverses the score
/// drifts as the pass runs and every comparison after that is against a corrupted incumbent.
///
/// Note what this does and does not establish. `add` and `sub` touch the reachable-surplus
/// accumulators in different orders on the do and undo legs, so exactness here is a property of
/// these magnitudes, not a guarantee of the arithmetic. What makes it safe is who reads them: the
/// only reader is `ancestor_bump_lower_bound`, which `LowestFee::score` never calls — it is the
/// bound's, and `repair` never bounds.
#[test]
fn view_add_and_sub_round_trip_exactly() {
    let t = target(10.0, 100_000);
    let problem = SelectionProblem::new(
        t,
        [
            input(80_000, "P"),
            input(80_000, "P"),
            input(80_000, "Q"),
            input(70_000, "Q"),
            input(60_000, CONFIRMED),
        ],
        [
            ancestor("P", 4_000, 1_500, vec![]),
            ancestor("Q", 3_000, 100, vec!["P"]),
        ],
    );
    let mut cs = problem.selector();
    cs.select(0);
    cs.select(2);

    let view = cs.compute_view();
    let before = metric().score(&view).expect("funded");
    let mut view = view;
    for _ in 0..2_000 {
        for (out, into) in [(0_usize, 1_usize), (2, 3), (0, 4)] {
            view.sub(out);
            view.add(into);
            let _ = metric().score(&view);
            view.sub(into);
            view.add(out);
        }
    }
    assert_eq!(
        metric().score(&view),
        Some(before),
        "6,000 undone swaps moved the view's own score",
    );
}

/// Branch and bound only ever selects and bans, so `run_bnb` has always returned a superset of what
/// the caller had already selected — which is how a wallet pins a required input. `repair`
/// deselects, so it is the one thing in the search path that can break that, and it must not.
#[test]
fn run_bnb_keeps_an_input_the_caller_required() {
    let t = target(10.0, 100_000);
    let problem = SelectionProblem::new(
        t,
        [
            input(80_000, "P"),
            input(80_000, "P"),
            input(80_000, "Q"), // 2: required, and the sole reason Q is paid for
            input(20_000, CONFIRMED),
        ],
        [
            ancestor("P", 4_000, 0, vec![]),
            ancestor("Q", 4_000, 0, vec![]),
        ],
    );

    let mut cs = problem.selector();
    cs.select(2);
    cs.run_bnb(metric(), 100_000).expect("fundable");

    assert!(
        cs.is_selected(2),
        "the caller's required input was dropped: got {:?}",
        cs.selected_indices().iter().collect::<Vec<_>>(),
    );
}
