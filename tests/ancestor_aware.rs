use bdk_coin_select::*;

#[test]
fn zero_ancestor_bump_fee_is_backward_compatible() {
    let candidates = [
        Candidate::new_tr_keyspend(500_000),
        Candidate::new_tr_keyspend(200_000),
    ];

    let target = Target {
        fee: TargetFee::from_feerate(FeeRate::from_sat_per_vb(5.0)),
        outputs: TargetOutputs::fund_outputs([(200, 100_000)]),
    };

    let mut cs = CoinSelector::new(&candidates);
    cs.select(0);

    assert_eq!(cs.selected_ancestor_bump_fee(), 0);
    assert!(cs.is_target_met(target));
}

#[test]
fn ancestor_bump_fee_reduces_effective_value() {
    let without = Candidate::new_tr_keyspend(500_000);
    let with = Candidate {
        ancestor_bump_fee: 10_000,
        ..Candidate::new_tr_keyspend(500_000)
    };

    let feerate = FeeRate::from_sat_per_vb(5.0);
    assert_eq!(
        without.effective_value(feerate) - with.effective_value(feerate),
        10_000.0
    );
}

#[test]
fn ancestor_bump_fee_reduces_excess() {
    let candidates = [Candidate {
        ancestor_bump_fee: 20_000,
        ..Candidate::new_tr_keyspend(500_000)
    }];

    let target = Target {
        fee: TargetFee::from_feerate(FeeRate::from_sat_per_vb(1.0)),
        outputs: TargetOutputs::fund_outputs([(200, 100_000)]),
    };

    let mut cs = CoinSelector::new(&candidates);
    cs.select(0);

    let excess_with = cs.rate_excess(target, Drain::NONE);

    // Compare against same candidate without ancestor cost.
    let candidates_no_anc = [Candidate::new_tr_keyspend(500_000)];
    let mut cs_no_anc = CoinSelector::new(&candidates_no_anc);
    cs_no_anc.select(0);

    let excess_without = cs_no_anc.rate_excess(target, Drain::NONE);

    assert_eq!(excess_without - excess_with, 20_000);
}
