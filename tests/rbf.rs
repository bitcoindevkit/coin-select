use bdk_coin_select::{FeeRate, Replace};

#[test]
fn run_bitcoin_core_rbf_tests() {
    // see rbf_tests.cpp
    //
    // https://github.com/bitcoin/bitcoin/blob/e69796c79c0aa202087a13ba62d9fbcc1c8754d4/src/test/rbf_tests.cpp#L151
    const CENT: u64 = 1_000_000;
    let low_fee = CENT / 100;
    let _normal_fee = CENT / 10;
    let high_fee = CENT;
    let incremental_relay_feerate = FeeRate::DEFUALT_RBF_INCREMENTAL_RELAY;
    let higher_relay_feerate = FeeRate::from_sat_per_vb(2.0);

    assert!(pays_for_rbf(high_fee, high_fee, 4, FeeRate::ZERO));
    assert!(!pays_for_rbf(high_fee, high_fee - 1, 4, FeeRate::ZERO));
    assert!(!pays_for_rbf(high_fee + 1, high_fee, 4, FeeRate::ZERO));
    assert!(!pays_for_rbf(
        high_fee,
        high_fee + 1,
        8,
        incremental_relay_feerate
    ));
    assert!(pays_for_rbf(
        high_fee,
        high_fee + 2,
        8,
        incremental_relay_feerate
    ));
    assert!(!pays_for_rbf(
        high_fee,
        high_fee + 2,
        8,
        higher_relay_feerate
    ));
    assert!(pays_for_rbf(
        high_fee,
        high_fee + 4,
        8,
        higher_relay_feerate
    ));
    assert!(!pays_for_rbf(
        low_fee,
        high_fee,
        99999999 * 4,
        incremental_relay_feerate
    ));
    assert!(pays_for_rbf(
        low_fee,
        high_fee + 99999999,
        99999999 * 4,
        incremental_relay_feerate
    ));
}

fn pays_for_rbf(
    original_fees: u64,
    replacement_fees: u64,
    replacement_weight: u64,
    relay_fee: FeeRate,
) -> bool {
    let min_fee = Replace {
        fee: original_fees,
        incremental_relay_feerate: relay_fee,
    }
    .min_fee_to_do_replacement(replacement_weight);

    replacement_fees >= min_fee
}
