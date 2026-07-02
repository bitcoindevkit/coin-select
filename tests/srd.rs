#![allow(clippy::zero_prefixed_literal)]
mod common;

use bdk_coin_select::{
    Candidate, CoinSelector, Drain, DrainWeights, FeeRate, SelectError, Target, TargetFee,
    TargetOutputs, CHANGE_LOWER, TR_SPK_WEIGHT, TXOUT_BASE_WEIGHT,
};

/// Deterministic, dependency-free `u64` source (SplitMix64) so we can drive `select_srd` without a
/// `rand` dependency.
fn splitmix64(mut state: u64) -> impl FnMut() -> u64 {
    move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn target(value: u64, feerate: f32) -> Target {
    Target {
        fee: TargetFee::from_feerate(FeeRate::from_sat_per_vb(feerate)),
        outputs: TargetOutputs::fund_outputs([(TXOUT_BASE_WEIGHT + TR_SPK_WEIGHT, value)]),
        max_weight: None,
    }
}

/// Whenever SRD succeeds, the change must be at least `change_lower` and the selection must actually
/// meet the target with that drain.
#[test]
fn srd_success_yields_healthy_change_that_meets_target() {
    let candidates = common::gen_candidates(60);
    let target = target(200_000, 10.0);
    let drain_weights = DrainWeights::TR_KEYSPEND;

    let mut successes = 0;
    for seed in 0..300u64 {
        let mut cs = CoinSelector::new(&candidates);
        let result = cs.select_srd(target, drain_weights, CHANGE_LOWER, splitmix64(seed));

        if let Ok(drain) = result {
            successes += 1;
            assert!(
                drain.value >= CHANGE_LOWER,
                "seed {}: change {} is below CHANGE_LOWER",
                seed,
                drain.value
            );
            assert_eq!(drain.weights, drain_weights);
            assert!(
                cs.is_funded_with_drain(target, drain),
                "seed {}: target not met with the returned drain",
                seed
            );
            // The reported change equals the actual excess available to the drain.
            let excess = cs.excess(
                target,
                Drain {
                    weights: drain_weights,
                    value: 0,
                },
            );
            assert_eq!(drain.value as i64, excess);
        }
    }
    assert!(successes > 0, "expected SRD to succeed for some seeds");
}

/// SRD errors when the candidates can't cover target + change_lower.
#[test]
fn srd_insufficient_funds() {
    // 3 * 50_000 = 150_000 total, well below target (200_000) + CHANGE_LOWER (50_000) + fees.
    let candidates = vec![
        Candidate {
            value: 50_000,
            weight: 100,
            input_count: 1,
            is_segwit: true,
        },
        Candidate {
            value: 50_000,
            weight: 100,
            input_count: 1,
            is_segwit: true,
        },
        Candidate {
            value: 50_000,
            weight: 100,
            input_count: 1,
            is_segwit: true,
        },
    ];
    let target = target(200_000, 5.0);
    let drain_weights = DrainWeights::TR_KEYSPEND;

    for seed in 0..50u64 {
        let mut cs = CoinSelector::new(&candidates);
        let result = cs.select_srd(target, drain_weights, CHANGE_LOWER, splitmix64(seed));
        assert!(
            matches!(result, Err(SelectError::InsufficientFunds(_))),
            "seed {}: expected InsufficientFunds, got {:?}",
            seed,
            result
        );
    }
}

/// SRD reports [`SelectError::MaxWeightExceeded`] when reaching `change_lower` would push the
/// selection past `Target::max_weight`. (It errors rather than evicting inputs — see the `TODO` on
/// `select_srd`.)
#[test]
fn srd_max_weight_exceeded() {
    // Identical candidates, so the selection's value and weight are independent of the random draw
    // order — the outcome is deterministic across seeds.
    let candidates = vec![
        Candidate {
            value: 100_000,
            weight: 1000,
            input_count: 1,
            is_segwit: true,
        };
        10
    ];
    let drain_weights = DrainWeights::TR_KEYSPEND;
    let drain = Drain {
        weights: drain_weights,
        value: 0,
    };

    // Weight of the smallest selection that reaches target + change_lower, with no cap.
    let mut probe = CoinSelector::new(&candidates);
    probe
        .select_until(|cs| cs.excess(target(200_000, 5.0), drain) >= CHANGE_LOWER as i64)
        .expect("candidates can cover target + change_lower");
    let needed_weight = probe.weight(target(200_000, 5.0).outputs, drain_weights);

    // Cap just below that, so SRD trips the weight limit as it reaches `change_lower`.
    let capped = Target {
        max_weight: Some(needed_weight - 1),
        ..target(200_000, 5.0)
    };

    for seed in 0..20u64 {
        let mut cs = CoinSelector::new(&candidates);
        let result = cs.select_srd(capped, drain_weights, CHANGE_LOWER, splitmix64(seed));
        assert!(
            matches!(result, Err(SelectError::MaxWeightExceeded)),
            "seed {}: expected MaxWeightExceeded, got {:?}",
            seed,
            result
        );
    }
}

/// If the already-selected inputs already provide enough change, SRD adds nothing (exercising the
/// pre-loop guard) and keeps counting them toward the target.
#[test]
fn srd_adds_nothing_when_already_sufficient() {
    let candidates = common::gen_candidates(60);
    let target = target(200_000, 6.0);
    let drain_weights = DrainWeights::TR_KEYSPEND;
    let drain = Drain {
        weights: drain_weights,
        value: 0,
    };

    // Preselect enough that the change already exceeds `change_lower`.
    let mut cs = CoinSelector::new(&candidates);
    cs.select_until(|cs| cs.excess(target, drain) >= CHANGE_LOWER as i64)
        .expect("candidates can cover target + change_lower");
    let before: Vec<usize> = cs.selected_indices().iter().collect();

    let out = cs
        .select_srd(target, drain_weights, CHANGE_LOWER, splitmix64(3))
        .expect("already sufficient");

    let after: Vec<usize> = cs.selected_indices().iter().collect();
    assert_eq!(
        after, before,
        "SRD selected more inputs even though the selection was already sufficient"
    );
    assert!(out.value >= CHANGE_LOWER);
}
