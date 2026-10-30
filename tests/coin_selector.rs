use bdk_coin_select::{Candidate, CoinSelector, Target, TargetFee, TargetOutputs};

#[test]
fn clear_selection_keeps_bans_and_order() {
    let candidates = [100_000, 300_000, 200_000, 400_000, 50_000]
        .iter()
        .map(|&value| Candidate::new_tr_keyspend(value))
        .collect::<Vec<_>>();
    let target = Target {
        fee: TargetFee::default(),
        outputs: TargetOutputs::fund_outputs([(43 * 4, 150_000)]),
        max_weight: None,
    };
    let mut cs = CoinSelector::new(&candidates, target);
    cs.sort_candidates_by_key(|(_, c)| core::cmp::Reverse(c.value));
    cs.ban(3);
    cs.select(1);
    cs.select(4);

    cs.clear_selection();

    assert!(cs.is_empty());
    assert!(cs.selected_indices().is_empty());
    assert_eq!(cs.banned().iter().collect::<Vec<_>>(), [3]);
    assert_eq!(cs.unselected_indices().collect::<Vec<_>>(), [1, 2, 0, 4]);
}
