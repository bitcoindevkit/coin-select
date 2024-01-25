use crate::{float::Ordf32, BnbMetric, ChangePolicy, CoinSelector, Drain, FeeRate, Target};

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
            let fee_for_the_tx = cs.fee(self.target.value(), drain.value);
            assert!(
                fee_for_the_tx > 0,
                "must not be called unless selection has met target"
            );
            // `spend_fee` rounds up here. We could use floats but I felt it was just better to
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

            // I think this whole if statement could be removed if we made this metric decide the change policy
            if let Some(drain_value) = drain_value {
                // it's possible that adding another input might reduce your long term fee if it
                // gets rid of an expensive change output. Our strategy is to take the lowest sat
                // per value candidate we have and use it as a benchmark. We imagine it has the
                // perfect value (but the same sats per weight unit) to get rid of the change output
                // by adding negative effective value (i.e. perfectly reducing excess to the point
                // where change wouldn't be added according to the policy).
                //
                // TODO: This metric could be tighter by being more complicated but this seems to be
                // good enough for now.
                let amount_above_change_threshold = drain_value - self.change_policy.min_value;

                if let Some((_, low_sats_per_wu_candidate)) = cs.unselected().next_back() {
                    let ev = low_sats_per_wu_candidate.effective_value(self.target.fee.rate);
                    // we can only reduce excess if ev is negative
                    if ev < -0.0 {
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
                        let cost_of_change = self.change_policy.drain_weights.waste(
                            self.target.fee.rate,
                            self.long_term_feerate,
                            self.target.outputs.n_outputs,
                        );
                        let best_score_without_change = Ordf32(
                            current_score.0 + cost_of_getting_rid_of_change - cost_of_change,
                        );
                        if best_score_without_change < current_score {
                            return Some(best_score_without_change);
                        }
                    }
                }
            } else {
                // Ok but maybe adding change could improve the metric?
                let cost_of_adding_change = self.change_policy.drain_weights.waste(
                    self.target.fee.rate,
                    self.long_term_feerate,
                    self.target.outputs.n_outputs,
                );
                let cost_of_no_change = cs.excess(self.target, Drain::none());

                let best_score_with_change =
                    Ordf32(current_score.0 - cost_of_no_change as f32 + cost_of_adding_change);
                if best_score_with_change < current_score {
                    return Some(best_score_with_change);
                }
            }

            Some(current_score)
        } else {
            // Step 1: select everything up until the input that hits the target.
            let (mut cs, resize_index, to_resize) = cs
                .clone()
                .select_iter()
                .find(|(cs, _, _)| cs.is_target_met(self.target))?;

            cs.deselect(resize_index);

            // We need to find the minimum fee we'd pay if we satisfy the feerate constraint. We do
            // this by imagining we had a perfect input that perfectly hit the target. The sats per
            // weight unit of this perfect input is the one at `slurp_index` but we'll do a scaled
            // resize of it to fit perfectly.
            //
            // Here's the formaula:
            //
            // target_feerate = (current_input_value - current_output_value + scale * value_resized_input) / (current_weight + scale * weight_resized_input)
            //
            // Rearranging to find `scale` we find that:
            //
            // scale = remaining_value_to_reach_feerate / effective_value_of_resized_input
            //
            // This should be intutive since we're finding out how to scale the input we're resizing to get the effective value we need.
            let rate_excess = cs.rate_excess(self.target, Drain::none()) as f32;
            let mut scale = Ordf32(0.0);

            if rate_excess < 0.0 {
                let remaining_value_to_reach_feerate = rate_excess.abs();
                let effective_value_of_resized_input =
                    to_resize.effective_value(self.target.fee.rate);
                if effective_value_of_resized_input > 0.0 {
                    let feerate_scale =
                        remaining_value_to_reach_feerate / effective_value_of_resized_input;
                    scale = scale.max(Ordf32(feerate_scale));
                } else {
                    return None; // we can never satisfy the constraint
                }
            }

            // We can use the same approach for replacement we just have to use the
            // incremental_relay_feerate.
            if let Some(replace) = self.target.fee.replace {
                let replace_excess = cs.replacement_excess(self.target, Drain::none()) as f32;
                if replace_excess < 0.0 {
                    let remaining_value_to_reach_feerate = replace_excess.abs();
                    let effective_value_of_resized_input =
                        to_resize.effective_value(replace.incremental_relay_feerate);
                    if effective_value_of_resized_input > 0.0 {
                        let replace_scale =
                            remaining_value_to_reach_feerate / effective_value_of_resized_input;
                        scale = scale.max(Ordf32(replace_scale));
                    } else {
                        return None; // we can never satisfy the constraint
                    }
                }
            }

            assert!(scale.0 > 0.0);
            let ideal_fee = scale.0 * to_resize.value as f32 + cs.selected_value() as f32
                - self.target.value() as f32;
            assert!(ideal_fee >= 0.0);

            Some(Ordf32(ideal_fee))
        }
    }

    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        true
    }
}
