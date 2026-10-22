use crate::{float::Ordf64, BnbMetric, CoinSelector, Drain, DrainWeights, FeeRate};

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
    fn drain_value(&self, cs: &CoinSelector<'_>) -> Option<u64> {
        // The change output pays for its own weight, so the value we'd actually recover is the
        // excess remaining after accounting for that weight.
        let excess_with_drain_weight = cs.excess(Drain {
            weights: self.drain_weights,
            value: 0,
        });

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
        if !cs.is_within_max_weight(self.drain_weights) {
            return None;
        }

        Some(excess_with_drain_weight.unsigned_abs())
    }

    /// The long-term-fee score together with the drain it assumes. `None` iff the value target
    /// isn't met.
    ///
    /// This does **not** reject an over-cap *changeless* selection — only [`score`](BnbMetric::score)
    /// does — though the drain it returns is never over-cap (`drain_value` refuses that). Used
    /// inside [`bound`](BnbMetric::bound): deferring the changeless rejection only loosens the lower
    /// bound and never makes it inadmissible, and `score` reuses the returned drain for its cap
    /// check so the drain is decided once.
    fn fee_score(&self, cs: &CoinSelector<'_>) -> Option<(Ordf64, Drain)> {
        if !cs.is_funded() {
            return None;
        }
        let drain = self.drain_value(cs).map_or(Drain::NONE, |value| Drain {
            weights: self.drain_weights,
            value,
        });
        let fee_for_the_tx = cs.fee(drain.value);
        assert!(
            fee_for_the_tx >= 0,
            "must not be called unless selection has met target: fee={}",
            fee_for_the_tx
        );
        let fee_for_spending_drain = drain.weights.spend_fee(self.long_term_feerate);
        Some((
            Ordf64((fee_for_the_tx as u64 + fee_for_spending_drain) as f64),
            drain,
        ))
    }
}

impl BnbMetric for LowestFee {
    fn drain(&mut self, cs: &CoinSelector<'_>) -> Drain {
        self.drain_value(cs).map_or(Drain::NONE, |value| Drain {
            weights: self.drain_weights,
            value,
        })
    }

    fn score(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf64> {
        let (score, drain) = self.fee_score(cs)?;
        // A final selection must fit the weight cap. `drain_value` already refuses an over-cap
        // change, but a changeless selection can still be too heavy on its own. Reuse the drain
        // `fee_score` already decided rather than recomputing it here.
        if !cs.is_within_max_weight(drain.weights) {
            return None;
        }
        Some(score)
    }

    fn bound(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf64> {
        // Weight hard-prune: input weight only grows as this branch is extended, so the lightest
        // solution in the subtree is this selection with no drain. If even that busts `max_weight`,
        // the whole subtree is infeasible -> prune. (Also keeps `fee_score(cs).unwrap()` below
        // sound: a value-met but over-cap node would otherwise score `None`.)
        if !cs.is_within_max_weight(DrainWeights::NONE) {
            return None;
        }

        if cs.is_funded() {
            let current_score = self.fee_score(cs).unwrap().0;

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
            if self.drain_value(cs).is_none() {
                // But a descendant might *add* a change output that improves the metric. This
                // happens when the current selection is changeless only because the change would be
                // dust: a descendant with more excess could clear the dust threshold and recover
                // value that is currently burned to fees.
                let cost_of_adding_change = self.drain_weights.waste(
                    cs.target().fee.rate,
                    self.long_term_feerate,
                    cs.target().outputs.n_outputs,
                );
                let cost_of_no_change = cs.excess(Drain::NONE);

                let best_score_with_change =
                    Ordf64(current_score.0 - cost_of_no_change as f64 + cost_of_adding_change);
                // max_weight-aware: realizing that improvement requires a change output AND at
                // least one more input to lift the excess over the dust/worthwhile threshold, both
                // of which only make the tx heavier. If there's no room for both under the cap the
                // improvement is unreachable down this branch, so don't credit it — keep
                // `current_score` (a tighter, still-admissible bound).
                let change_is_reachable = match cs.target().max_weight {
                    None => true,
                    Some(max_weight) => cs.min_input_weight().map_or(false, |min_input_weight| {
                        cs.weight(self.drain_weights) + min_input_weight <= max_weight
                    }),
                };
                if change_is_reachable && best_score_with_change < current_score {
                    return Some(best_score_with_change);
                }
            }

            Some(current_score)
        } else {
            // Step 1: select everything up until the input that hits the target.
            let (mut cs, resize_index, to_resize) =
                cs.clone().select_iter().find(|(cs, _, _)| cs.is_funded())?;

            // If this selection is already perfect, return its score directly.
            if cs.excess(Drain::NONE) == 0 {
                return Some(self.fee_score(&cs).unwrap().0);
            };
            cs.deselect(resize_index);

            // We need to find the minimum fee we'd pay if we satisfy the feerate constraint. We do
            // this by imagining we had a perfect input that perfectly hit the target. The sats per
            // weight unit of this perfect input is that of `to_resize` but we'll do a scaled
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
            let rate_excess = cs.rate_excess_wu(Drain::NONE) as f64;
            let mut scale = 0.0_f64;

            if rate_excess < 0.0 {
                let remaining_value_to_reach_feerate = rate_excess.abs();
                let effective_value_of_resized_input =
                    to_resize.effective_value(cs.target().fee.rate);
                if effective_value_of_resized_input > 0.0 {
                    let feerate_scale =
                        remaining_value_to_reach_feerate / effective_value_of_resized_input;
                    scale = scale.max(feerate_scale);
                } else {
                    return None; // we can never satisfy the constraint
                }
            }

            // We can use the same approach for replacement we just have to use the
            // incremental_relay_feerate.
            if let Some(replace) = cs.target().fee.replace {
                let replace_excess = cs.replacement_excess_wu(Drain::NONE) as f64;
                if replace_excess < 0.0 {
                    let remaining_value_to_reach_feerate = replace_excess.abs();
                    let effective_value_of_resized_input =
                        to_resize.effective_value(replace.incremental_relay_feerate);
                    if effective_value_of_resized_input > 0.0 {
                        let replace_scale =
                            remaining_value_to_reach_feerate / effective_value_of_resized_input;
                        scale = scale.max(replace_scale);
                    } else {
                        return None; // we can never satisfy the constraint
                    }
                }
            }
            // Handle absolute fee constraint. Unlike feerate and replacement, the
            // absolute fee is a fixed amount (not weight-proportional), so we just
            // need enough raw value to cover the gap.
            let absolute_excess = cs.absolute_excess(Drain::NONE) as f64;
            if absolute_excess < 0.0 {
                let remaining = absolute_excess.abs();
                if to_resize.value > 0 {
                    let absolute_scale = remaining / to_resize.value as f64;
                    scale = scale.max(absolute_scale);
                } else {
                    return None; // we can never satisfy the constraint
                }
            }

            // max_weight-aware: reaching the feerate needs a perfect input weighing
            // `scale * to_resize.weight`. `to_resize` is the best value-per-weight input available,
            // so if the current weight plus even that (fractional) minimum already busts the cap,
            // no within-cap selection down this branch reaches the target -> prune. This is the
            // fractional relaxation, so it never prunes a branch with an (integer) within-cap
            // solution.
            if let Some(max_weight) = cs.target().max_weight {
                if cs.weight(DrainWeights::NONE) as f64 + scale * to_resize.weight as f64
                    > max_weight as f64
                {
                    return None;
                }
            }

            // `scale` could be 0 even if `is_funded` is `false` due to the latter being based on
            // rounded-up vbytes. Rounding can also push the sum slightly below zero; 0 is always an
            // admissible lower bound for a fee, so clamp. Rounding can just as well push it a
            // fraction of a sat above a reachable fee, which is harmless: scores are whole sats, so
            // it can't overtake a strictly cheaper selection.
            let ideal_fee = (scale * to_resize.value as f64
                + (cs.selected_value() as i64 - cs.target().value() as i64) as f64)
                .max(0.0);

            Some(Ordf64(ideal_fee))
        }
    }

    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        true
    }
}
