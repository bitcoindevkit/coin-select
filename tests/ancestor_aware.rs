use bdk_coin_select::{
    Candidate, CoinSelector, Drain, FeeRate, Target, TargetFee, TargetOutputs, UnconfirmedAncestor,
    TR_KEYSPEND_TXIN_WEIGHT,
};

fn simple_target(feerate: f32) -> Target {
    Target {
        outputs: TargetOutputs {
            value_sum: 100_000,
            weight_sum: 200,
            n_outputs: 1,
        },
        fee: TargetFee::from_feerate(FeeRate::from_sat_per_vb(feerate)),
    }
}

#[test]
fn zero_ancestors_backward_compatible() {
    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
        ancestors: vec![],
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
    }];

    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
        ancestors: vec![0],
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
    }];

    let candidates = [
        Candidate {
            input_count: 1,
            value: 100_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
            ancestors: vec![0], // points to ancestor 0
        },
        Candidate {
            input_count: 1,
            value: 100_000,
            weight: TR_KEYSPEND_TXIN_WEIGHT,
            is_segwit: true,
            ancestors: vec![0], // also points to ancestor 0
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
        },
        UnconfirmedAncestor {
            weight: 400,
            fee_paid: 10_000, // very high fee
        },
    ];

    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
        ancestors: vec![0, 1], // depends on both ancestors
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
    }];

    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
        ancestors: vec![0],
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
    }];

    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
        ancestors: vec![0],
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
    }];

    let candidates = [Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true,
        ancestors: vec![0],
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
