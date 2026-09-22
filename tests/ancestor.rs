#![allow(unused_imports)]
//! Coin selection over candidates that drag in unconfirmed ancestors (CPFP).
//!
//! The invariant under test is that a selection's fee obligation includes the bump owed by the
//! **union** of the ancestors its selected candidates drag in — each ancestor charged exactly once,
//! weights and fees netted over the union — and that `LowestFee` branch and bound stays correct
//! under the resulting non-monotone funding.

mod common;

use bdk_coin_select::{
    float::Ordf32, metrics::LowestFee, AncestorToBump, BnbMetric, Candidate, CoinSelector, Drain,
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

    assert_eq!(cs.ancestor_bump(), 2_500);

    let no_ancestors = SelectionProblem::new_no_ancestors(
        t,
        [Candidate {
            value: 100_000,
            weight: P2WPKH_INPUT_WEIGHT,
            input_count: 1,
        }],
    );
    let mut clean_cs = no_ancestors.selector();
    clean_cs.select(0);

    assert_eq!(clean_cs.ancestor_bump(), 0);
    assert_eq!(
        cs.weight(DrainWeights::NONE),
        clean_cs.weight(DrainWeights::NONE)
    );
    assert_eq!(
        cs.excess(Drain::NONE),
        clean_cs.excess(Drain::NONE) - 2_500,
        "the bump is the only difference between the two selections"
    );
    assert_eq!(
        cs.implied_fee(DrainWeights::NONE),
        clean_cs.implied_fee(DrainWeights::NONE) + 2_500
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
    assert!(clean_only.is_funded());

    let mut both = problem.selector();
    both.select(0);
    both.select(1);
    assert!(
        !both.is_funded(),
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
    assert_eq!(cs.ancestor_bump(), 2_500);
    assert_ne!(
        cs.ancestor_bump(),
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
    assert_eq!(cs.ancestor_bump(), 2_500);

    cs.deselect(0);
    assert_eq!(cs.ancestor_bump(), 2_500, "candidate 1 still drags in P");

    cs.select(2);
    assert_eq!(
        cs.ancestor_bump(),
        2_500,
        "a confirmed coin drags in nothing"
    );

    cs.deselect(1);
    assert_eq!(cs.ancestor_bump(), 0, "nothing selected drags in P anymore");
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
    assert_eq!(cs.ancestor_bump(), 1_000);
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
    assert_eq!(poor_only.ancestor_bump(), 100);

    let mut rich_only = problem.selector();
    rich_only.select(0);
    assert_eq!(rich_only.ancestor_bump(), 0, "never credits the child");

    let mut both = problem.selector();
    both.select(0);
    both.select(1);
    assert_eq!(
        both.ancestor_bump(),
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

    let child_weight = cs.weight(DrainWeights::NONE);
    assert!(child_weight < heavy);

    t.max_weight = Some(child_weight);
    let capped = SelectionProblem::new(
        t,
        [input(50_000, "P")],
        [ancestor("P", heavy, heavy, vec![])],
    );
    let mut capped_cs = capped.selector();
    capped_cs.select(0);
    assert!(capped_cs.is_within_max_weight(DrainWeights::NONE));
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
    let score = m.score(&cs).expect("funded");
    let drain = m.drain(&cs);
    assert_eq!(
        score,
        Ordf32((cs.fee(drain.value) as u64 + drain.weights.spend_fee(m.long_term_feerate)) as f32)
    );
    assert!(
        cs.fee(drain.value) as u64 >= cs.ancestor_bump(),
        "a funded selection's child fee covers the bump"
    );
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
    assert_eq!(problem.shared_drags_in(1), &[1_u32]);
    assert_eq!(problem.shared_drags_in(2), &[1_u32]);

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
    assert_eq!(cs.ancestor_bump(), 2_500);
    assert_eq!(
        cs.ancestor_bump_lower_bound(),
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
    assert_eq!(cs.ancestor_bump(), 1_000);
    assert_eq!(
        cs.ancestor_bump_lower_bound(),
        0,
        "RICH's 9_900 surplus swamps the 1_000 owed"
    );

    // Which is not pessimism: that descendant really does owe nothing.
    let mut both = cs.clone();
    both.select(1);
    assert_eq!(both.ancestor_bump(), 0);
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
    assert_eq!(cs.ancestor_bump(), 1_000);
    assert_eq!(cs.ancestor_bump_lower_bound(), 0);

    // Ban the coin that would bring RICH in and the surplus is out of reach again.
    let mut banned = cs.clone();
    banned.ban(1);
    assert!(banned.addable_ancestors().is_empty());
    assert_eq!(banned.ancestor_bump_lower_bound(), 1_000);

    // Likewise once there is nothing left to add.
    let mut exhausted = cs.clone();
    exhausted.select(1);
    assert!(exhausted.is_exhausted());
    assert_eq!(
        exhausted.ancestor_bump_lower_bound(),
        exhausted.ancestor_bump()
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

    let child_fee = t.fee.rate.implied_fee_wu(cs.weight(DrainWeights::NONE));
    let bound = metric().bound(&cs).expect("within max_weight");
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
        cs.ancestor_bump_lower_bound() <= cs.ancestor_bump(),
        "the lower bound must not exceed the bump, even where f32 fee arithmetic would round"
    );
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
    assert!(cs.is_funded());
    assert_eq!(cs.ancestor_bump(), 1_000);
    assert_eq!(cs.ancestor_bump_lower_bound(), 0);

    let score = metric().score(&cs).unwrap();
    let bound = metric().bound(&cs).unwrap();
    assert!(
        bound <= Ordf32(score.0 - 1_000.0),
        "bound {} must sit at least the 1_000 surplus below score {}",
        bound,
        score
    );

    let mut both = cs.clone();
    both.select(1);
    let both_score = metric().score(&both).unwrap();
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
                residing_txid: "POOR",
            },
            Input {
                value: 0,
                weight: 0,
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
    assert_eq!(node.ancestor_bump(), 1_998_000_000);
    assert_eq!(node.ancestor_bump_lower_bound(), 0);
    let bound = metric.bound(&node).unwrap();

    let mut descendant = node.clone();
    descendant.select(1);
    let score = metric.score(&descendant).unwrap();
    assert_eq!(score, Ordf32(720_000.0));
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
    assert!(!cs.is_funded());
    assert!(
        metric().bound(&cs).is_some(),
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
    assert!(!node.is_funded());

    let bound = metric().bound(&node).unwrap();
    let mut descendant = node.clone();
    descendant.select(1);
    let score = metric().score(&descendant).unwrap();
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
    let bound = metric().bound(&root).unwrap();
    let mut descendant = root.clone();
    descendant.select(0);
    let score = metric().score(&descendant).unwrap();
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
    let bound = metric().bound(&root).unwrap();
    let mut descendant = root.clone();
    descendant.select(0);
    let score = metric().score(&descendant).unwrap();
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
    assert_eq!(cs.ancestor_bump(), 1_000);

    // RICH's 9_900 surplus is real, but only comes with GRAN's 2_000 deficit: still a net surplus.
    assert_eq!(cs.ancestor_bump_lower_bound(), 0);
    let mut both = cs.clone();
    both.select(1);
    assert_eq!(
        both.ancestor_bump(),
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
    assert_eq!(cs.ancestor_bump(), 1_000);
    assert_eq!(
        cs.ancestor_bump_lower_bound(),
        1_000,
        "taking RICH means taking GRAN, which costs far more than RICH's surplus is worth"
    );

    let mut both = cs.clone();
    both.select(1);
    assert!(
        both.ancestor_bump() > 1_000,
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
    assert_eq!(cs.ancestor_bump(), 1_000);
    assert_eq!(
        cs.ancestor_bump_lower_bound(),
        0,
        "RICH is reachable without HEAVY, so its surplus counts"
    );
}

/// The lookahead prune leaves out candidates whose standalone effective value is negative, on the
/// grounds that they can only lower what is still reachable. With ancestors that is not the whole
/// story: such a candidate can *fund* a selection by dragging in an ancestor that overpays, which
/// lowers the bump the rest of the selection owes. The prune credits that through the bump's
/// branch-wide floor, so it must not cut this branch off.
///
/// The greedy seed funds the target with the cheap clean coin, so it cannot stand in for the search
/// here: the optimum is reachable only down the branch that excludes that coin, which is exactly
/// the node whose only remaining candidate is one the lookahead does not count.
#[test]
fn lookahead_keeps_a_branch_funded_only_by_a_subsidizing_ancestor() {
    let t = target(10.0, 90_000); // 2.5 sat/wu
    let problem = SelectionProblem::new(
        t,
        [
            // Pays for itself, but not enough to cover the target and its own ancestor's bump.
            input(100_000, "POOR"),
            // Costs far more weight than it is worth, so the lookahead ignores its value — but it
            // drags in an ancestor paying 50_000 sats over the rate, which is worth more than the
            // 5_000 sats of fee its own weight costs.
            Input {
                value: 100,
                weight: 2_000,
                residing_txid: "RICH",
            },
            // Enough to fund the target alongside the first coin, but at a worse fee than paying
            // the bump off with RICH's surplus.
            input(10_000, CONFIRMED),
        ],
        [
            ancestor("POOR", 4_000, 0, vec![]),    // owes 10_000
            ancestor("RICH", 400, 51_000, vec![]), // overpays by 50_000
        ],
    );

    let mut poor_only = problem.selector();
    poor_only.select(0);
    assert!(
        !poor_only.is_funded(),
        "the bump on POOR leaves it short of the target"
    );
    assert!(
        problem.candidate(1).effective_value(t.fee.rate) < 0.0,
        "the subsidizing coin's own value never covers its weight"
    );

    // The search sorts by descending value per weight before seeding, so seed from that order.
    let mut greedy = problem.selector();
    greedy.sort_candidates_by_descending_value_pwu();
    greedy
        .select_until_target_met()
        .expect("the clean coin funds it");
    assert!(
        greedy.is_selected(2) && !greedy.is_selected(1),
        "the seed takes the clean coin, so the search has to find the rest: {}",
        greedy
    );

    let mut exhaustive = problem.selector();
    let (best_score, _) =
        common::exhaustive_search(&mut exhaustive, &mut metric()).expect("solvable");
    assert!(
        exhaustive.is_selected(0) && exhaustive.is_selected(1) && !exhaustive.is_selected(2),
        "the optimum pays the bump off with RICH's surplus: {}",
        exhaustive
    );

    let mut cs = problem.selector();
    let (score, _) = cs
        .run_bnb(metric(), 100_000)
        .expect("the optimum must not be pruned");
    assert_eq!(score, best_score, "bnb settled for {}", cs);
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
/// sets of the selected candidates, sum weight and fee over that union, and take the exact
/// shortfall.
fn expected_bump(problem: &SelectionProblem, cs: &CoinSelector<'_>, feerate: FeeRate) -> u64 {
    let mut union = std::collections::BTreeSet::new();
    for i in cs.selected_indices().iter() {
        union.extend(problem.drags_in(i).iter().map(|&a| a as usize));
    }
    let (weight, fee) = union
        .iter()
        .map(|&i| problem.ancestors()[i])
        .fold((0u64, 0u64), |(w, f), (aw, af)| (w + aw, f + af));
    // Exact: an f32 rate converts to f64 exactly, and these weights are far too small for the
    // product to round.
    (weight as f64 * feerate.spwu() as f64 - fee as f64)
        .ceil()
        .max(0.0) as u64
}

proptest! {
    /// Every selection's bump must equal the union-derived figure — in particular it must never be
    /// the sum of the per-candidate `local_bump`s when ancestors are shared.
    #[test]
    fn bump_matches_union_definition(spec in spec_strategy()) {
        let problem = spec.build();
        let feerate = problem.target().fee.rate;
        let cs = problem.selector();

        prop_assert_eq!(cs.ancestor_bump(), expected_bump(&problem, &cs, feerate));

        for (node, _) in common::ExhaustiveIter::new(&cs).into_iter().flatten() {
            prop_assert_eq!(
                node.ancestor_bump(),
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
            let lower_bound = node.ancestor_bump_lower_bound();
            prop_assert!(
                lower_bound <= node.ancestor_bump(),
                "node={} lb={} owes={}", node, lower_bound, node.ancestor_bump()
            );

            for (descendant, inclusion) in common::ExhaustiveIter::new(&node).into_iter().flatten() {
                if !inclusion {
                    continue;
                }
                prop_assert!(
                    lower_bound <= descendant.ancestor_bump(),
                    "node={} lb={} descendant={} owes={}",
                    node, lower_bound, descendant, descendant.ancestor_bump()
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
            let bound = metric.bound(&node);
            let subtree = std::iter::once(node.clone()).chain(
                common::ExhaustiveIter::new(&node)
                    .into_iter()
                    .flatten()
                    .filter(|(_, inclusion)| *inclusion)
                    .map(|(descendant, _)| descendant),
            );

            for descendant in subtree {
                let score = metric.score(&descendant);
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
