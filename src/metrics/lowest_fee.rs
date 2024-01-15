use crate::{
    change_policy::ChangePolicy, float::Ordf32, BnbMetric, Candidate, CoinSelector, Drain, FeeRate,
    Target,
};

/// Metric that aims to minimize transaction fees. The future fee for spending the change output is
/// included in this calculation.
///
/// The fee is simply:
///
/// > `inputs - outputs` where `outputs = target.value + change_value`
///
/// But the total value includes the cost of spending the change output if it exists:
///
/// > `change_spend_weight * long_term_feerate`
///
/// The `change_spend_weight` and `change_value` are determined by the `change_policy`
#[derive(Clone, Copy)]
pub struct LowestFee {
    /// The target parameters for the resultant selection.
    pub target: Target,
    /// The estimated feerate needed to spend our change output later.
    pub long_term_feerate: FeeRate,
    /// Policy to determine the change output (if any) of a given selection.
    pub change_policy: ChangePolicy,
}

impl BnbMetric for LowestFee {
    fn score(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf32> {
        if !cs.is_target_met(self.target) {
            return None;
        }

        let long_term_fee = {
            let drain = cs.drain(self.target, self.change_policy);
            let fee_for_the_tx = cs.fee(self.target.value, drain.value);
            assert!(
                fee_for_the_tx > 0,
                "must not be called unless selection has met target"
            );
            // Why `spend_fee` rounds up here. We could use floats but I felt it was just better to
            // accept the extra 1 sat penality to having a change output
            let fee_for_spending_drain = drain.weights.spend_fee(self.long_term_feerate);
            fee_for_the_tx as u64 + fee_for_spending_drain
        };

        Some(Ordf32(long_term_fee as f32))
    }

    fn bound(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf32> {
        if cs.is_target_met(self.target) {
            let current_score = self.score(cs).unwrap();

            let drain_value = cs.drain_value(self.target, self.change_policy);

            if let Some(drain_value) = drain_value {
                // it's possible that adding another input might reduce your long term if it gets
                // rid of an expensive change output. Our strategy is to take the lowest sat per
                // value candidate we have and use it as a benchmark. We imagine it has the perfect
                // value (but the same sats per weight unit) to get rid of the change output by
                // adding negative effective value (i.e. perfectly reducing excess to the point
                // where change wouldn't be added according to the policy).
                //
                // TODO: This metric could be tighter by being more complicated but this seems to be
                // good enough for now.
                let amount_above_change_threshold = drain_value - self.change_policy.min_value;

                if let Some((_, low_sats_per_wu_candidate)) = cs.unselected().next_back() {
                    let ev = low_sats_per_wu_candidate.effective_value(self.target.feerate);
                    if ev < -0.0 {
                        // we can only reduce excess if ev is negative
                        let value_per_negative_effective_value =
                            low_sats_per_wu_candidate.value as f32 / ev.abs();
                        // this is how much abosolute value we have to add to cancel out the excess
                        let extra_value_needed_to_get_rid_of_change = amount_above_change_threshold
                            as f32
                            * value_per_negative_effective_value;

                        // NOTE: the drain_value goes to fees if we get rid of it so it's part of
                        // the cost of removing the change output
                        let cost_of_getting_rid_of_change =
                            extra_value_needed_to_get_rid_of_change + drain_value as f32;
                        let cost_of_change = self
                            .change_policy
                            .drain_weights
                            .waste(self.target.feerate, self.long_term_feerate);
                        let best_score_without_change = Ordf32(
                            current_score.0 + cost_of_getting_rid_of_change - cost_of_change,
                        );
                        if best_score_without_change < current_score {
                            return Some(best_score_without_change);
                        }
                    }
                }
            }

            Some(current_score)
        } else {
            // Step 1: select everything up until the input that hits the target.
            let (mut cs, slurp_index, to_slurp) = cs
                .clone()
                .select_iter()
                .find(|(cs, _, _)| cs.is_target_met(self.target))?;

            cs.deselect(slurp_index);

            // Step 2: We pretend that the final input exactly cancels out the remaining excess
            // by taking whatever value we want from it but at the value per weight of the real
            // input.
            let ideal_next_weight = {
                let remaining_rate = cs.rate_excess(self.target, Drain::none());

                slurp_wv(to_slurp, remaining_rate.min(0), self.target.feerate)
            };
            let input_weight_lower_bound = cs.input_weight() as f32 + ideal_next_weight;
            let ideal_fee_by_feerate =
                (cs.base_weight() as f32 + input_weight_lower_bound) * self.target.feerate.spwu();
            let ideal_fee = ideal_fee_by_feerate.max(self.target.min_fee as f32);

            Some(Ordf32(ideal_fee))
        }
    }

    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        true
    }
}

/// Returns the "perfect weight" for this candidate to slurp up a given value with `feerate` while
/// not changing the candidate's value/weight ratio.
///
/// Used to pretend that a candidate had precisely `value_to_slurp` + fee needed to include it. It
/// tells you how much weight such a perfect candidate would have if it had the same value per
/// weight unit as `candidate`. This is useful for estimating a lower weight bound for a perfect
/// match.
fn slurp_wv(candidate: Candidate, value_to_slurp: i64, feerate: FeeRate) -> f32 {
    // the value per weight unit this candidate offers at feerate
    let value_per_wu = (candidate.value as f32 / candidate.weight as f32) - feerate.spwu();
    // return how much weight we need
    let weight_needed = value_to_slurp as f32 / value_per_wu;
    debug_assert!(weight_needed <= candidate.weight as f32);
    weight_needed.min(0.0)
}
