use crate::{float::Ordf32, BnbMetric, Drain, DrainWeights, FeeRate, SelectionView};

/// Metric that aims to minimize transaction fees. The future fee for spending the change output is
/// included in this calculation.
///
/// The fee is simply:
///
/// > `inputs - outputs` where `outputs = cs.target().value + change_value`
///
/// But the total value includes the cost of spending the change output if it exists:
///
/// > `change_spend_weight * long_term_feerate`
///
/// Unlike other metrics, `LowestFee` decides for itself whether a selection should have a change
/// output: change is added whenever doing so lowers the long-term fee (i.e. the recovered excess
/// outweighs the future cost of spending the change), the resulting value is at least the dust
/// threshold implied by `dust_relay_feerate`, and the transaction with change fits
/// [`Target::max_weight`](crate::Target::max_weight).
///
/// # Unconfirmed ancestors
///
/// When the [`SelectionProblem`] has unconfirmed ancestors, the fee a selection must pay includes
/// the [`SelectionView::ancestor_bump`](crate::SelectionView::ancestor_bump) of the ancestors it drags
/// in, so the search naturally prefers coins that drag in nothing or share an already-paid-for
/// ancestor. Ancestor fees are netted over the union, allowing an overpaying ancestor to offset an
/// underpaying one without subsidizing the child itself. The score remains the child transaction's
/// fee: the bump is inside it, not added on top.
///
/// The bound uses a child-weight relaxation when ancestors are present (see
/// [`bound`](BnbMetric::bound)): a funded node credits reachable ancestor surplus and possible future
/// change, clamped to the monotone fee floor, while an unfunded one estimates the least child weight
/// needed to meet each fee constraint. Neither reasons from "select everything and it is still
/// unfunded", which does not follow while funding is not monotone. Infeasibility comes from the two
/// prunes that can prove it: the lookahead in [`bound`](BnbMetric::bound), which relaxes the
/// ancestor bump to its branch-wide floor rather than assuming it only grows, and the relaxation's
/// own test for a deficit the most optimistic input still available cannot close at any weight.
///
/// [`SelectionProblem`]: crate::SelectionProblem
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
    pub(super) fn drain_value(&self, cs: &SelectionView<'_>) -> Option<u64> {
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
    ///
    /// The score is the *child* transaction's fee (plus the future cost of spending its change).
    /// Any [`SelectionView::ancestor_bump`] is not added on top: it is already inside the child's fee,
    /// because covering it is what [`SelectionView::is_funded`] demands and what the change
    /// calculation gives up.
    fn fee_score(&self, cs: &SelectionView<'_>) -> Option<(Ordf32, Drain)> {
        if !cs.is_funded() {
            return None;
        }
        let drain = self.drain_value(cs).map_or(Drain::NONE, |value| Drain {
            weights: self.drain_weights,
            value,
        });
        let fee_for_the_tx = cs.fee(cs.target().value(), drain.value);
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

    /// Whether a descendant of `cs` could still add both a change output and at least one more
    /// input under `max_weight`. Same test as the no-ancestor funded path.
    fn change_is_reachable(&self, cs: &SelectionView<'_>) -> bool {
        match cs.target().max_weight {
            None => true,
            Some(max_weight) => cs.min_input_weight().map_or(false, |min_input_weight| {
                cs.weight(cs.target().outputs, self.drain_weights) + min_input_weight <= max_weight
            }),
        }
    }

    /// Tighter than [`SelectionView::fee_floor`] once the value shortfall proves that every funded
    /// descendant must add some child input weight.
    ///
    /// Returns `None` only for the one infeasibility this relaxation can actually prove: a fee
    /// constraint whose deficit the best input still available cannot close at any weight. It is
    /// *not* allowed to reason from "select everything and it is still unfunded" — a fat private
    /// deficit can un-fund a prefix that a subset would have funded. (The caller has already
    /// hard-pruned on child `max_weight`, which is monotone.)
    ///
    /// The three fee constraints get independent fractional relaxations. Their maximum is still a
    /// lower bound on the real added child weight. Candidate ancestry is ignored and the global bump
    /// floor is used instead, avoiding package-surplus double counting. Flooring the fractional
    /// weight keeps floating-point error in the safe direction.
    fn bound_with_ancestors(&self, cs: &SelectionView<'_>) -> Option<Ordf32> {
        if cs.is_funded() {
            let (_, drain) = self.fee_score(cs).unwrap();
            let current_score = cs.fee(cs.target().value(), drain.value) as u64
                + drain.weights.spend_fee(self.long_term_feerate);
            let surplus = cs
                .ancestor_bump()
                .saturating_sub(cs.ancestor_bump_lower_bound());
            let mut bound = current_score.saturating_sub(surplus);
            if drain.is_none() {
                let cost_of_adding_change = self.drain_weights.waste(
                    cs.target().fee.rate,
                    self.long_term_feerate,
                    cs.target().outputs.n_outputs,
                );
                // Subtract the large integer terms before converting anything to float. Casting the
                // non-negative waste to u64 floors it, keeping the bound conservative.
                let with_change = current_score
                    .saturating_sub(surplus)
                    .saturating_sub(cs.excess(Drain::NONE) as u64)
                    .saturating_add(cost_of_adding_change as u64);
                if self.change_is_reachable(cs) {
                    bound = bound.min(with_change);
                }
            }
            return Some(Ordf32(bound.max(cs.fee_floor()) as f32));
        }

        let target = cs.target();
        let bump = cs.ancestor_bump_lower_bound();
        let current_weight = cs.weight(target.outputs, DrainWeights::NONE);
        let selected_value = cs.selected_value() as f64;
        let value_target = target.value() as f64;
        let target_rate = target.fee.rate.spwu() as f64;
        let rate_deficit = (value_target + target_rate * current_weight as f64 + bump as f64
            - selected_value)
            .max(0.0);
        let absolute_deficit =
            (value_target + target.fee.absolute as f64 - selected_value).max(0.0);
        let (replace_deficit, replace_rate) = target.fee.replace.map_or((0.0, 0.0), |replace| {
            let rate = replace.incremental_relay_feerate.spwu() as f64;
            (
                (value_target + replace.fee as f64 + rate * current_weight as f64 - selected_value)
                    .max(0.0),
                rate,
            )
        });

        let best_value = cs.best_undecided_value_pwu();
        let weightless_value = cs.has_weightless_undecided_value();
        let best_rate_gain = (best_value - target_rate).max(0.0);
        let best_replace_gain = (best_value - replace_rate).max(0.0);

        // Bitcoin Core computes `is_feerate_high` once and lets it decide whether a prune that is
        // only sometimes valid may fire, rather than dropping the prune outright. Same shape here.
        // A deficit that no available input can close at any weight is not a claim about
        // monotonicity: descendants only add, the deficit already uses the branch-wide bump floor,
        // and the gain already ignores whatever ancestors those inputs would drag in. So this much
        // infeasibility is provable even though the general case is not, and saying so prunes the
        // subtree instead of ranking it.
        let unreachable =
            |deficit: f64, gain_pwu: f64| !weightless_value && deficit > 0.0 && gain_pwu <= 0.0;
        if unreachable(rate_deficit, best_rate_gain)
            || unreachable(absolute_deficit, best_value)
            || unreachable(replace_deficit, best_replace_gain)
        {
            return None;
        }

        // Treat the best candidate as unlimited fractional input. If no positive gain is available,
        // or a positive-value zero-weight candidate exists, fall back to zero added weight rather
        // than claiming infeasibility.
        let weight_for = |deficit: f64, gain_pwu: f64| match (deficit, gain_pwu) {
            (deficit, gain) if !weightless_value && deficit > 0.0 && gain > 0.0 => deficit / gain,
            _ => 0.0,
        };
        let added_weight = weight_for(rate_deficit, best_rate_gain)
            .max(weight_for(absolute_deficit, best_value))
            .max(weight_for(replace_deficit, best_replace_gain));

        // `added_weight` is non-negative, so conversion to u64 truncates (floors) it.
        let added_weight = added_weight as u64;
        let weight = match current_weight.checked_add(added_weight) {
            Some(weight) if added_weight != u64::MAX => weight,
            _ => return Some(Ordf32(cs.fee_floor() as f32)),
        };
        let mut bound = (target.fee.rate.implied_fee_wu(weight) + bump).max(target.fee.absolute);
        if let Some(replace) = target.fee.replace {
            bound = bound.max(replace.min_fee_to_do_replacement_wu(weight));
        }
        Some(Ordf32(bound as f32))
    }
}

impl BnbMetric for LowestFee {
    fn drain(&mut self, cs: &SelectionView<'_>) -> Drain {
        self.drain_value(cs).map_or(Drain::NONE, |value| Drain {
            weights: self.drain_weights,
            value,
        })
    }

    fn score(&mut self, cs: &SelectionView<'_>) -> Option<Ordf32> {
        let (score, drain) = self.fee_score(cs)?;
        // A final selection must fit the weight cap. `drain_value` already refuses an over-cap
        // change, but a changeless selection can still be too heavy on its own. Reuse the drain
        // `fee_score` already decided rather than recomputing it here.
        if !cs.is_within_max_weight(drain.weights) {
            return None;
        }
        Some(score)
    }

    fn bound(&mut self, cs: &SelectionView<'_>) -> Option<Ordf32> {
        // Weight hard-prune: input weight only grows as this branch is extended, so the lightest
        // solution in the subtree is this selection with no drain. If even that busts `max_weight`,
        // the whole subtree is infeasible -> prune. (Also keeps `fee_score(cs).unwrap()` below
        // sound: a value-met but over-cap node would otherwise score `None`.)
        //
        // Ancestor weight is *not* part of this: `max_weight` caps the child transaction only.
        if !cs.is_within_max_weight(DrainWeights::NONE) {
            return None;
        }

        // Lookahead hard-prune (Bitcoin Core's `curr_available_value` test): if everything still
        // undecided cannot close the feerate gap, no descendant is funded, so the subtree is empty.
        // Funding needs every fee constraint met, so failing this one alone is enough to prune.
        // Constant-time, and it fires before either relaxation below does any work.
        if cs.best_reachable_rate_excess_wu() < 0 {
            return None;
        }

        // With unconfirmed ancestors, funding is not monotone, so neither this path nor the one
        // below may reason from "select everything and it is still unfunded". Emptiness is claimed
        // only where it is provable: by the lookahead above, which relaxes the bump to its
        // branch-wide floor, and by `bound_with_ancestors`' own unclosable-deficit test.
        if cs.problem().has_ancestors() {
            return self.bound_with_ancestors(cs);
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
                    Ordf32(current_score.0 - cost_of_no_change as f32 + cost_of_adding_change);
                // max_weight-aware: realizing that improvement requires a change output AND at
                // least one more input to lift the excess over the dust/worthwhile threshold, both
                // of which only make the tx heavier. If there's no room for both under the cap the
                // improvement is unreachable down this branch, so don't credit it — keep
                // `current_score` (a tighter, still-admissible bound).
                if self.change_is_reachable(cs) && best_score_with_change < current_score {
                    return Some(best_score_with_change);
                }
            }

            Some(current_score)
        } else {
            // Step 1: select everything up until the input that hits the cs.target().
            let mut local = cs.clone();
            let mut unselected = cs.unselected();
            let (resize_index, to_resize) = loop {
                let (index, candidate) = unselected.next()?;
                local.add_unchecked(index);
                if local.is_funded() {
                    break (index, candidate);
                }
            };

            // If this selection is already perfect, return its score directly.
            if local.excess(Drain::NONE) == 0 {
                return Some(self.fee_score(&local).unwrap().0);
            };
            local.sub_unchecked(resize_index);
            let cs = &local;

            // We need to find the minimum fee we'd pay if we satisfy the feerate constraint. We do
            // this by imagining we had a perfect input that perfectly hit the cs.target(). The sats per
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
            let rate_excess = cs.rate_excess_wu(Drain::NONE) as f32;
            let mut scale = Ordf32(0.0);

            if rate_excess < 0.0 {
                let remaining_value_to_reach_feerate = rate_excess.abs();
                let effective_value_of_resized_input =
                    to_resize.effective_value(cs.target().fee.rate);
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
            if let Some(replace) = cs.target().fee.replace {
                let replace_excess = cs.replacement_excess_wu(Drain::NONE) as f32;
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
            let absolute_excess = cs.absolute_excess(Drain::NONE) as f32;
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
            // so if the current weight plus even that (fractional) minimum already busts the cap,
            // no within-cap selection down this branch reaches the target -> prune. This is the
            // fractional relaxation, so it never prunes a branch with an (integer) within-cap
            // solution.
            if let Some(max_weight) = cs.target().max_weight {
                if cs.weight(cs.target().outputs, DrainWeights::NONE) as f32
                    + scale.0 * to_resize.weight as f32
                    > max_weight as f32
                {
                    return None;
                }
            }

            // `scale` could be 0 even if `is_funded` is `false` due to the latter being based on
            // rounded-up vbytes.
            let ideal_fee = scale.0 * to_resize.value as f32 + cs.selected_value() as f32
                - cs.target().value() as f32;
            assert!(ideal_fee >= 0.0);

            Some(Ordf32(ideal_fee))
        }
    }

    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        true
    }
}
