use bdk_coin_select::{
    Candidate, CoinSelector, Drain, DrainWeights, FeeRate, Replace, Target, TargetFee,
    TargetOutputs, UnconfirmedAncestor, TR_KEYSPEND_TXIN_WEIGHT,
};

fn simple_target(feerate: f32) -> Target {
    Target {
        outputs: TargetOutputs {
            value_sum: 100_000,
            weight_sum: 200,
            n_outputs: 1,
        },
        fee: TargetFee::from_feerate(FeeRate::from_sat_per_vb(feerate)),
        max_weight: None,
    }
}

#[test]
fn zero_ancestors_backward_compatible() {
    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
    }];

    let mut cs = CoinSelector::new(&candidates);
    cs.select(0);

    assert_eq!(
        cs.selected_ancestor_bump_fee(FeeRate::from_sat_per_vb(10.0)),
        0
    );

    let target = simple_target(10.0);
    let excess_no_ancestors = cs.excess(target, Drain::NONE);
    assert!(
        excess_no_ancestors > 0,
        "should meet target without ancestors"
    );
}

#[test]
fn single_ancestor_reduces_excess() {
    // Ancestor: 400 wu, paid 10 sats (very low feerate)
    let ancestors = [UnconfirmedAncestor {
        weight: 400,
        fee_paid: 10,
        dependent_candidates: vec![0],
    }];

    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
    }];

    let feerate = FeeRate::from_sat_per_vb(10.0);
    let target = simple_target(10.0);

    // Without ancestors
    let mut cs_no_anc = CoinSelector::new(&candidates);
    cs_no_anc.select(0);
    let excess_no_anc = cs_no_anc.excess(target, Drain::NONE);

    // With ancestors
    let mut cs_with_anc = CoinSelector::new(&candidates).with_ancestors(&ancestors);
    cs_with_anc.select(0);

    let bump_fee = cs_with_anc.selected_ancestor_bump_fee(feerate);
    assert!(bump_fee > 0, "ancestor should need bumping");

    let excess_with_anc = cs_with_anc.excess(target, Drain::NONE);
    assert!(
        excess_with_anc < excess_no_anc,
        "ancestor bump fee should reduce excess: {} < {}",
        excess_with_anc,
        excess_no_anc
    );
    assert_eq!(
        excess_no_anc - excess_with_anc,
        bump_fee as i64,
        "excess difference should equal bump fee"
    );
}

#[test]
fn shared_ancestors_are_deduplicated() {
    // Both candidates share the same ancestor
    let ancestors = [UnconfirmedAncestor {
        weight: 400,
        fee_paid: 10,
        dependent_candidates: vec![0, 1],
    }];

    let candidates = [
        Candidate {
            input_count: 1,
            value: 100_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
        },
        Candidate {
            input_count: 1,
            value: 100_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
        },
    ];

    let feerate = FeeRate::from_sat_per_vb(10.0);

    // Select only candidate 0
    let mut cs_one = CoinSelector::new(&candidates).with_ancestors(&ancestors);
    cs_one.select(0);
    let bump_one = cs_one.selected_ancestor_bump_fee(feerate);

    // Select both candidates
    let mut cs_both = CoinSelector::new(&candidates).with_ancestors(&ancestors);
    cs_both.select(0);
    cs_both.select(1);
    let bump_both = cs_both.selected_ancestor_bump_fee(feerate);

    // The bump fee should be the SAME because the ancestor is shared (deduplicated)
    assert_eq!(
        bump_one, bump_both,
        "shared ancestor should only be counted once: one={} both={}",
        bump_one, bump_both
    );
}

#[test]
fn high_feerate_ancestor_subsidizes_low_feerate() {
    // Two ancestors: one overpaid, one underpaid
    // At package level, the overpayment subsidizes the underpayment
    let ancestors = [
        UnconfirmedAncestor {
            weight: 400,
            fee_paid: 10, // very low fee
            dependent_candidates: vec![0],
        },
        UnconfirmedAncestor {
            weight: 400,
            fee_paid: 10_000, // very high fee
            dependent_candidates: vec![0],
        },
    ];

    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
    }];

    let feerate = FeeRate::from_sat_per_vb(10.0);

    let mut cs = CoinSelector::new(&candidates).with_ancestors(&ancestors);
    cs.select(0);

    let bump = cs.selected_ancestor_bump_fee(feerate);

    // Package: total_weight = 800, total_fee_paid = 10_010
    // implied_fee at 10 sat/vb = ceil(800/4) * 10 = 2000 sats
    // bump = max(0, 2000 - 10_010) = 0
    assert_eq!(
        bump, 0,
        "high-feerate ancestor should subsidize low-feerate ancestor in the package"
    );
}

#[test]
fn ancestor_package_above_target_contributes_zero_bump() {
    let ancestors = [UnconfirmedAncestor {
        weight: 400,
        fee_paid: 10_000, // way above any reasonable feerate
        dependent_candidates: vec![0],
    }];

    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
    }];

    let feerate = FeeRate::from_sat_per_vb(10.0);

    let mut cs = CoinSelector::new(&candidates).with_ancestors(&ancestors);
    cs.select(0);

    assert_eq!(
        cs.selected_ancestor_bump_fee(feerate),
        0,
        "ancestor already above target feerate should contribute zero bump"
    );
}

#[test]
fn different_feerates_produce_different_bump_fees() {
    let ancestors = [UnconfirmedAncestor {
        weight: 400,
        fee_paid: 100, // 1 sat/vb
        dependent_candidates: vec![0],
    }];

    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
    }];

    let mut cs = CoinSelector::new(&candidates).with_ancestors(&ancestors);
    cs.select(0);

    let bump_low = cs.selected_ancestor_bump_fee(FeeRate::from_sat_per_vb(5.0));
    let bump_high = cs.selected_ancestor_bump_fee(FeeRate::from_sat_per_vb(20.0));

    assert!(
        bump_high > bump_low,
        "higher feerate should produce larger bump fee: high={} low={}",
        bump_high,
        bump_low
    );
}

#[test]
fn effective_value_includes_ancestor_bump() {
    let ancestors = [UnconfirmedAncestor {
        weight: 400,
        fee_paid: 10,
        dependent_candidates: vec![0],
    }];

    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
    }];

    let feerate = FeeRate::from_sat_per_vb(10.0);

    let mut cs_no_anc = CoinSelector::new(&candidates);
    cs_no_anc.select(0);
    let ev_no_anc = cs_no_anc.effective_value(feerate);

    let mut cs_with_anc = CoinSelector::new(&candidates).with_ancestors(&ancestors);
    cs_with_anc.select(0);
    let ev_with_anc = cs_with_anc.effective_value(feerate);

    let bump = cs_with_anc.selected_ancestor_bump_fee(feerate);
    assert!(bump > 0);
    assert_eq!(
        ev_no_anc - ev_with_anc,
        bump as i64,
        "effective value difference should equal bump fee"
    );
}

/// `implied_fee` is the exact counterpart of `excess`, so it must carry the ancestor bump. A
/// wallet sizing its change output from `implied_fee` would otherwise underpay the package by
/// exactly the bump amount.
#[test]
fn implied_fee_includes_ancestor_bump() {
    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
    }];
    // 400 wu = 100 vb, so the ancestor owes 100 * 10 = 1000 sats but paid 10 => 990 sat bump.
    let ancestors = [UnconfirmedAncestor {
        weight: 400,
        fee_paid: 10,
        dependent_candidates: vec![0],
    }];
    let target = simple_target(10.0);

    let mut without = CoinSelector::new(&candidates);
    without.select(0);
    let mut with = CoinSelector::new(&candidates).with_ancestors(&ancestors);
    with.select(0);

    assert_eq!(
        with.selected_ancestor_bump_fee(FeeRate::from_sat_per_vb(10.0)),
        990
    );
    assert_eq!(
        with.implied_fee(target, DrainWeights::NONE)
            - without.implied_fee(target, DrainWeights::NONE),
        990,
        "implied_fee must carry the ancestor bump"
    );
}

/// `excess == selected_value - target.value() - drain.value - implied_fee` must hold for every
/// combination of fee constraints. This pins the whole `*_excess` family against `implied_fee`, so
/// a bump added to one but not the other cannot go unnoticed.
#[test]
fn excess_and_implied_fee_agree() {
    let candidates = [
        Candidate {
            input_count: 1,
            value: 200_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
        },
        Candidate {
            input_count: 1,
            value: 50_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
        },
    ];

    let ancestor_sets: [Vec<UnconfirmedAncestor>; 3] = [
        // no ancestors at all: the identity must hold on the pre-existing code paths too
        vec![],
        // one underpaying ancestor
        vec![UnconfirmedAncestor {
            weight: 400,
            fee_paid: 10,
            dependent_candidates: vec![0],
        }],
        // an underpaying ancestor plus an overpaying one that subsidizes it
        vec![
            UnconfirmedAncestor {
                weight: 400,
                fee_paid: 10,
                dependent_candidates: vec![0],
            },
            UnconfirmedAncestor {
                weight: 1_000,
                fee_paid: 100_000,
                dependent_candidates: vec![1],
            },
        ],
    ];

    for ancestors in &ancestor_sets {
        for absolute in [0_u64, 5_000, 500_000] {
            for replace in [None, Some(Replace::new(1_000))] {
                for feerate in [1.0_f32, 10.0, 50.0] {
                    for drain in [
                        Drain::NONE,
                        Drain {
                            weights: DrainWeights::TR_KEYSPEND,
                            value: 20_000,
                        },
                    ] {
                        let target = Target {
                            outputs: TargetOutputs {
                                value_sum: 100_000,
                                weight_sum: 200,
                                n_outputs: 1,
                            },
                            fee: TargetFee {
                                rate: FeeRate::from_sat_per_vb(feerate),
                                replace,
                                absolute,
                            },
                            max_weight: None,
                        };

                        for selection in [vec![], vec![0], vec![1], vec![0, 1]] {
                            let mut cs = CoinSelector::new(&candidates).with_ancestors(ancestors);
                            for i in &selection {
                                cs.select(*i);
                            }

                            assert_eq!(
                                cs.excess(target, drain),
                                cs.selected_value() as i64
                                    - target.value() as i64
                                    - drain.value as i64
                                    - cs.implied_fee(target, drain.weights) as i64,
                                "identity broken: n_ancestors={} absolute={} replace={} \
                                 feerate={} drain={} selection={:?}",
                                ancestors.len(),
                                absolute,
                                replace.is_some(),
                                feerate,
                                drain.value,
                                selection,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Candidates with unconfirmed ancestors are banned from the automatic selection algorithms,
/// because the bump fee is a package-level property that per-candidate ranking cannot see.
#[test]
fn candidates_with_ancestors_are_banned_from_automatic_selection() {
    let candidates = [
        // depends on an expensive unconfirmed ancestor
        Candidate {
            input_count: 1,
            value: 200_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
        },
        // ordinary confirmed candidate
        Candidate {
            input_count: 1,
            value: 200_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
        },
    ];
    let ancestors = [UnconfirmedAncestor {
        weight: 400,
        fee_paid: 10,
        dependent_candidates: vec![0],
    }];

    let cs = CoinSelector::new(&candidates).with_ancestors(&ancestors);

    assert!(cs.banned().contains(0), "ancestor candidate must be banned");
    assert!(!cs.banned().contains(1), "candidate 1 has no ancestors");
    assert_eq!(
        cs.unselected_indices().collect::<Vec<_>>(),
        vec![1],
        "only the ancestor-free candidate is reachable"
    );

    // A greedy selection must fund itself from candidate 1 alone.
    let mut greedy = cs.clone();
    greedy
        .select_until_target_met(simple_target(10.0))
        .expect("candidate 1 alone covers the target");
    assert!(!greedy.is_selected(0));
    assert!(greedy.is_selected(1));
    assert_eq!(
        greedy.selected_ancestor_bump_fee(FeeRate::from_sat_per_vb(10.0)),
        0,
        "no ancestor is reachable, so the bump stays zero"
    );
}

/// Banning is only about *automatic* selection — the caller can still spend an unconfirmed UTXO
/// explicitly, which is the actual CPFP flow, and the bump is priced when they do.
#[test]
fn banned_ancestor_candidates_remain_manually_selectable() {
    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
    }];
    let ancestors = [UnconfirmedAncestor {
        weight: 400,
        fee_paid: 10,
        dependent_candidates: vec![0],
    }];

    let mut cs = CoinSelector::new(&candidates).with_ancestors(&ancestors);
    assert!(cs.banned().contains(0));

    assert!(cs.select(0), "ban must not block manual selection");
    assert!(cs.is_selected(0));
    assert_eq!(
        cs.selected_ancestor_bump_fee(FeeRate::from_sat_per_vb(10.0)),
        990,
        "the package is still priced once the candidate is selected"
    );
}

/// `candidates_with_ancestors` reports each affected candidate once even when several ancestors
/// name it, and excludes candidates the caller banned for their own reasons.
#[test]
fn candidates_with_ancestors_is_deduplicated_and_distinct_from_banned() {
    let candidates = [
        Candidate {
            input_count: 1,
            value: 100_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
        },
        Candidate {
            input_count: 1,
            value: 100_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
        },
        Candidate {
            input_count: 1,
            value: 100_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
        },
    ];
    // candidate 0 is named by both ancestors; candidate 1 by one of them
    let ancestors = [
        UnconfirmedAncestor {
            weight: 400,
            fee_paid: 10,
            dependent_candidates: vec![0, 1],
        },
        UnconfirmedAncestor {
            weight: 400,
            fee_paid: 20,
            dependent_candidates: vec![0],
        },
    ];

    let mut cs = CoinSelector::new(&candidates).with_ancestors(&ancestors);
    // a ban of the caller's own, unrelated to ancestors
    cs.ban(2);

    assert_eq!(
        cs.candidates_with_ancestors().collect::<Vec<_>>(),
        vec![0, 1],
        "each affected candidate reported once, and candidate 2 is not one of them"
    );
    assert_eq!(
        cs.banned().iter().collect::<Vec<_>>(),
        vec![0, 1, 2],
        "banned() also carries the caller's own ban"
    );
}
