//! BnB with `LowestFee` must find the same optimum as brute force when values are large.
use bdk_coin_select::{
    metrics::LowestFee, BnbMetric, Candidate, CoinSelector, DrainWeights, FeeRate, Target,
    TargetFee, TargetOutputs,
};

/// Small deterministic random number generator, so every run tests the same cases.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo)
    }
}

/// Exact (integer) long-term fee of a selection, or `None` if it isn't a valid solution.
fn exact_score(metric: &mut LowestFee, cs: &CoinSelector<'_>) -> Option<u64> {
    if !cs.is_funded() {
        return None;
    }
    let drain = metric.drain(cs);
    if !cs.is_within_max_weight(drain.weights) {
        return None;
    }
    Some(cs.fee(drain.value) as u64 + drain.weights.spend_fee(metric.long_term_feerate))
}

/// Runs `cases` random scenarios and returns (suboptimal, panics, solvable).
fn run(
    min_value: u64,
    max_value: u64,
    max_feerate: u64,
    absolute: bool,
    cases: u64,
) -> (u64, u64, u64) {
    let (mut mismatches, mut panics, mut solved) = (0, 0, 0);
    for seed in 0..cases {
        let mut rng = Lcg(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ max_value);
        let n = rng.range(2, 11) as usize;
        let candidates: Vec<Candidate> = (0..n)
            .map(|_| Candidate {
                value: rng.range(min_value, max_value),
                weight: rng.range(160, 600),
                input_count: 1,
            })
            .collect();
        let total: u64 = candidates.iter().map(|c| c.value).sum();
        let target = Target {
            outputs: TargetOutputs {
                value_sum: rng.range(total / 10, total),
                weight_sum: rng.range(100, 1000),
                n_outputs: 1,
            },
            fee: TargetFee {
                absolute: if absolute {
                    rng.range(min_value, total / 10 + min_value + 1)
                } else {
                    0
                },
                ..TargetFee::from_feerate(FeeRate::from_sat_per_vb(
                    rng.range(1, max_feerate + 1) as f32
                ))
            },
            max_weight: None,
        };
        let mut metric = LowestFee {
            long_term_feerate: FeeRate::from_sat_per_vb(rng.range(1, max_feerate + 1) as f32),
            dust_relay_feerate: FeeRate::from_sat_per_vb(3.0),
            drain_weights: DrainWeights::TR_KEYSPEND,
        };

        // Brute force: try every subset of candidates and keep the cheapest.
        let mut best: Option<u64> = None;
        for mask in 1u32..(1 << n) {
            let mut cs = CoinSelector::new(&candidates, target);
            for i in 0..n {
                if mask & (1 << i) != 0 {
                    cs.select(i);
                }
            }
            if let Some(s) = exact_score(&mut metric, &cs) {
                best = Some(best.map_or(s, |b| b.min(s)));
            }
        }
        let best = match best {
            Some(b) => b,
            None => continue,
        };
        solved += 1;

        // Branch and bound must find the same fee, without panicking.
        let res = std::panic::catch_unwind(|| {
            let mut cs = CoinSelector::new(&candidates, target);
            let mut m = metric;
            cs.run_bnb(metric, usize::MAX).ok()?;
            exact_score(&mut m, &cs)
        });
        match res {
            Err(_) => panics += 1,
            Ok(got) if got != Some(best) => {
                if mismatches < 3 {
                    println!(
                        "seed {seed}: bnb={got:?} brute_force={best} (diff {:?})",
                        got.map(|g| g as i64 - best as i64)
                    );
                }
                mismatches += 1
            }
            Ok(_) => {}
        }
    }
    (mismatches, panics, solved)
}

macro_rules! case {
    ($name:ident, $min:expr, $max:expr, $fr:expr, $abs:expr) => {
        #[test]
        fn $name() {
            let (mismatches, panics, solved) = run($min, $max, $fr, $abs, 20_000);
            println!(
                "{}: {solved} solvable, {mismatches} suboptimal, {panics} panics",
                stringify!($name)
            );
            assert_eq!((mismatches, panics), (0, 0));
        }
    };
}

// name, min UTXO value, max UTXO value (sats), max feerate (sat/vB), use absolute fee
case!(small_values, 1_000, 500_000, 100, false);
// around 1 BTC per UTXO
case!(one_btc, 50_000_000, 200_000_000, 100, false);
case!(one_btc_low_feerate, 50_000_000, 200_000_000, 2, false);
// 0.2 BTC .. 20 BTC per UTXO
case!(large_values, 20_000_000, 2_000_000_000, 100, false);
case!(
    large_values_low_feerate,
    20_000_000,
    2_000_000_000,
    2,
    false
);
// up to 500 BTC per UTXO
case!(
    huge_values_low_feerate,
    20_000_000,
    50_000_000_000,
    2,
    false
);
// large absolute fee (> 2^24 sats), the #82 scenario
case!(large_absolute_fee, 20_000_000, 2_000_000_000, 100, true);
