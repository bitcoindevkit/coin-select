use crate::{float::Ordf32, BnbMetric, CoinSelector, Drain, DrainWeights, FeeRate, Target};

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
/// Unlike other metrics, `LowestFee` decides for itself whether a selection should have a change
/// output: change is added whenever doing so lowers the long-term fee (i.e. the recovered excess
/// outweighs the future cost of spending the change) and the resulting change value is above the
/// dust threshold implied by `dust_relay_feerate`.
#[derive(Clone, Copy)]
pub struct LowestFee {
    /// The estimated feerate needed to spend our change output later.
    pub long_term_feerate: FeeRate,
    /// The feerate used to determine the dust threshold of the change output.
    pub dust_relay_feerate: FeeRate,
    /// The weights of the change output that would be added.
    pub drain_weights: DrainWeights,
}

impl LowestFee {
    /// The value the change output should have, or `None` if this selection should be changeless.
    fn drain_value(&self, cs: &CoinSelector<'_>, target: Target) -> Option<u64> {
        // The change output pays for its own weight, so the value we'd actually recover is the
        // excess remaining after accounting for that weight.
        let excess_with_drain_weight = cs.excess(
            target,
            Drain {
                weights: self.drain_weights,
                value: 0,
            },
        );

        // Adding change is only worth it if the value we'd recover exceeds the future cost of
        // spending it (i.e. it lowers the long-term fee).
        let drain_spend_cost = self
            .long_term_feerate
            .implied_fee_wu(self.drain_weights.spend_weight);
        if excess_with_drain_weight <= drain_spend_cost as i64 {
            return None;
        }

        // ...and only if the change output would not be dust.
        let dust_threshold = self.drain_weights.dust_threshold(self.dust_relay_feerate);
        if excess_with_drain_weight < dust_threshold as i64 {
            return None;
        }

        // ...and only if the change output would not push the tx over `max_weight`. If it would,
        // we refuse the drain and the excess goes to fee instead (a slightly conservative choice:
        // it can refuse change even when a no-change tx of this selection would fit).
        let drain = Drain {
            weights: self.drain_weights,
            value: excess_with_drain_weight.unsigned_abs(),
        };
        if !cs.is_within_max_weight(target, drain) {
            return None;
        }

        Some(excess_with_drain_weight.unsigned_abs())
    }

    /// The long-term-fee score **ignoring** the `max_weight` cap, together with the drain it
    /// assumes. `None` iff the value target isn't met. Used inside [`bound`](BnbMetric::bound),
    /// where ignoring the (feasibility) cap only loosens the lower bound and never makes it
    /// inadmissible; [`score`](BnbMetric::score) reuses the returned drain for its cap check so the
    /// drain is only decided once.
    fn fee_score(&self, cs: &CoinSelector<'_>, target: Target) -> Option<(Ordf32, Drain)> {
        if !cs.is_funded(target) {
            return None;
        }
        let drain = self
            .drain_value(cs, target)
            .map_or(Drain::NONE, |value| Drain {
                weights: self.drain_weights,
                value,
            });
        let fee_for_the_tx = cs.fee(target.value(), drain.value);
        assert!(
            fee_for_the_tx >= 0,
            "must not be called unless selection has met target: fee={}",
            fee_for_the_tx
        );
        let fee_for_spending_drain = drain.weights.spend_fee(self.long_term_feerate);
        Some((
            Ordf32((fee_for_the_tx as u64 + fee_for_spending_drain) as f32),
            drain,
        ))
    }
}

impl BnbMetric for LowestFee {
    fn drain(&mut self, cs: &CoinSelector<'_>, target: Target) -> Drain {
        self.drain_value(cs, target)
            .map_or(Drain::NONE, |value| Drain {
                weights: self.drain_weights,
                value,
            })
    }

    fn score(&mut self, cs: &CoinSelector<'_>, target: Target) -> Option<Ordf32> {
        let (score, drain) = self.fee_score(cs, target)?;
        // A final selection must fit the weight cap. `drain_value` already refuses an over-cap
        // change, but a changeless selection can still be too heavy on its own. Reuse the drain
        // `fee_score` already decided rather than recomputing it here.
        if !cs.is_within_max_weight(target, drain) {
            return None;
        }
        Some(score)
    }

    fn bound(&mut self, cs: &CoinSelector<'_>, target: Target) -> Option<Ordf32> {
        // Weight hard-prune: input weight only grows as this branch is extended, so the lightest
        // solution in the subtree is this selection with no drain. If even that busts `max_weight`,
        // the whole subtree is infeasible -> prune. (Also keeps `fee_score(cs).unwrap()` below
        // sound: a value-met but over-cap node would otherwise score `None`.)
        if !cs.is_within_max_weight(target, Drain::NONE) {
            return None;
        }

        if cs.is_funded(target) {
            let current_score = self.fee_score(cs, target).unwrap().0;

            // `current_score` is already a valid lower bound for a selection that has change: a
            // descendant can never lower the fee by removing an existing (worthwhile) change
            // output.
            //
            // Proof: let A be a selection with worthwhile change and let B = A + one extra input of
            // value `v >= 0` that makes B changeless. The long-term fee (LTF, i.e. the score) of
            // each is:
            //
            //     LTF_A = (selected_A - target - change_value) + spend_fee   // with change
            //     LTF_B =  selected_B - target                               // changeless
            //
            // Substituting selected_B = selected_A + v:
            //
            //     LTF_B - LTF_A = v + change_value - spend_fee
            //
            // Change is only added when it's worthwhile, i.e. `change_value > spend_fee` (see
            // `drain_value`, where `change_value` is `excess_with_drain_weight` and `spend_fee` is
            // `drain_spend_cost`). With `v >= 0` the difference is strictly positive: B always
            // costs more.
            if self.drain_value(cs, target).is_none() {
                // But a descendant might *add* a change output that improves the metric. This
                // happens when the current selection is changeless only because the change would be
                // dust: a descendant with more excess could clear the dust threshold and recover
                // value that is currently burned to fees.
                let cost_of_adding_change = self.drain_weights.waste(
                    target.fee.rate,
                    self.long_term_feerate,
                    target.outputs.n_outputs,
                );
                let cost_of_no_change = cs.excess(target, Drain::NONE);

                let best_score_with_change =
                    Ordf32(current_score.0 - cost_of_no_change as f32 + cost_of_adding_change);
                // max_weight-aware: recovering that value requires a change output (and more
                // inputs to clear the dust threshold), which only makes the tx heavier. If a change
                // output can't fit the cap now it never will down this branch, so don't credit the
                // improvement — keep `current_score` (a tighter, still-admissible bound).
                let change_output = Drain {
                    weights: self.drain_weights,
                    value: 0,
                };
                if best_score_with_change < current_score
                    && cs.is_within_max_weight(target, change_output)
                {
                    return Some(best_score_with_change);
                }
            }

            Some(current_score)
        } else {
            // Step 1: select everything up until the input that hits the target.
            let (mut cs, resize_index, to_resize) = cs
                .clone()
                .select_iter()
                .find(|(cs, _, _)| cs.is_funded(target))?;

            // If this selection is already perfect, return its score directly.
            if cs.excess(target, Drain::NONE) == 0 {
                return Some(self.fee_score(&cs, target).unwrap().0);
            };
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
            //
            // In the perfect scenario, no additional fee would be required to pay for rounding up when converting from weight units to
            // vbytes and so all fee calculations below are performed on weight units directly.
            let rate_excess = cs.rate_excess_wu(target, Drain::NONE) as f32;
            let mut scale = Ordf32(0.0);

            if rate_excess < 0.0 {
                let remaining_value_to_reach_feerate = rate_excess.abs();
                let effective_value_of_resized_input = to_resize.effective_value(target.fee.rate);
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
            if let Some(replace) = target.fee.replace {
                let replace_excess = cs.replacement_excess_wu(target, Drain::NONE) as f32;
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
            // Handle absolute fee constraint. Unlike feerate and replacement, the
            // absolute fee is a fixed amount (not weight-proportional), so we just
            // need enough raw value to cover the gap.
            let absolute_excess = cs.absolute_excess(target, Drain::NONE) as f32;
            if absolute_excess < 0.0 {
                let remaining = absolute_excess.abs();
                if to_resize.value > 0 {
                    let absolute_scale = remaining / to_resize.value as f32;
                    scale = scale.max(Ordf32(absolute_scale));
                } else {
                    return None; // we can never satisfy the constraint
                }
            }

            // max_weight-aware: reaching the feerate needs a perfect input weighing
            // `scale * to_resize.weight`. `to_resize` is the best value-per-weight input available,
            // so if even that (fractionally) can't fit the remaining weight budget, no within-cap
            // selection down this branch reaches the target -> prune. This is the fractional
            // relaxation, so it never prunes a branch that has an (integer) within-cap solution.
            if let Some(max_weight) = target.max_weight {
                let budget =
                    max_weight.saturating_sub(cs.weight(target.outputs, DrainWeights::NONE));
                if scale.0 * to_resize.weight as f32 > budget as f32 {
                    return None;
                }
            }

            // `scale` could be 0 even if `is_funded` is `false` due to the latter being based on
            // rounded-up vbytes.
            let ideal_fee = scale.0 * to_resize.value as f32 + cs.selected_value() as f32
                - target.value() as f32;
            assert!(ideal_fee >= 0.0);

            Some(Ordf32(ideal_fee))
        }
    }

    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        true
    }
}
