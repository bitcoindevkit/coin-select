use crate::{bnb::BnbMetric, float::Ordf32, CoinSelector, Drain, DrainWeights, FeeRate, Target};
use alloc::vec::Vec;

/// Metric that minimizes the [waste metric] subject to the constraint that the selection produces
/// no change output.
///
/// For a changeless selection, waste reduces to:
///
/// > `input_weight * (feerate - long_term_feerate) + max(0, excess)`
///
/// Excess in a changeless transaction goes to the miner as fees and is therefore fully counted as
/// waste.
///
/// Restricting to changeless solutions removes the non-monotonic discontinuity that the general
/// (with-change) waste metric has when an input flips the change output on or off, which makes a
/// correct bound much easier to construct.
///
/// Like [`LowestFee`], `ChangelessWaste` decides for itself whether a selection *would* have a
/// change output (using the same rule: change is worthwhile when the recovered excess outweighs the
/// future cost of spending it and clears the dust threshold). Selections that would have change are
/// rejected, so only genuinely changeless selections are scored.
///
/// [waste metric]: https://bitcoin.stackexchange.com/questions/113622/what-does-waste-metric-mean-in-the-context-of-coin-selection
/// [`LowestFee`]: crate::metrics::LowestFee
#[derive(Clone, Copy, Debug)]
pub struct ChangelessWaste {
    /// The estimated feerate needed to spend a change output later. This is used by the metric
    /// even though the scored selections do not have a change output — the long-term feerate
    /// defines the `feerate - long_term_feerate` weight cost of each input.
    pub long_term_feerate: FeeRate,
    /// The feerate used to determine the dust threshold of the change output.
    pub dust_relay_feerate: FeeRate,
    /// The weights of the change output that would be added.
    pub drain_weights: DrainWeights,
}

impl ChangelessWaste {
    /// The value the change output would have, or `None` if this selection should be changeless.
    ///
    /// This is the same change decision as [`LowestFee`]: the metric owns its change policy instead
    /// of taking one as input.
    ///
    /// [`LowestFee`]: crate::metrics::LowestFee
    fn drain_value(&self, cs: &CoinSelector<'_>, target: Target) -> Option<u64> {
        let excess_with_drain_weight = self.excess_with_drain_weight(cs, target);

        // Adding change is only worth it if the value we'd recover exceeds the future cost of
        // spending it (i.e. it lowers the long-term fee).
        if excess_with_drain_weight <= self.drain_spend_cost() as i64 {
            return None;
        }

        // ...and only if the change output would not be dust.
        if excess_with_drain_weight < self.dust_threshold() as i64 {
            return None;
        }

        // ...and only if the change output would not push the tx over `max_weight`. If it would,
        // we refuse the drain and the excess goes to fee instead (a slightly conservative choice:
        // it can refuse change even when a no-change tx of this selection would fit).
        if !cs.is_within_max_weight(target, self.drain_weights) {
            return None;
        }

        Some(excess_with_drain_weight.unsigned_abs())
    }

    /// The excess of `cs` after accounting for the weight (but not value) of a would-be change
    /// output. This is the quantity the change decision (see [`drain_value`]) is made on.
    ///
    /// [`drain_value`]: Self::drain_value
    fn excess_with_drain_weight(&self, cs: &CoinSelector<'_>, target: Target) -> i64 {
        // The change output pays for its own weight, so the value we'd actually recover is the
        // excess remaining after accounting for that weight.
        cs.excess(
            target,
            Drain {
                weights: self.drain_weights,
                value: 0,
            },
        )
    }

    /// The future fee of spending a would-be change output. Change below this is never worthwhile.
    fn drain_spend_cost(&self) -> u64 {
        self.long_term_feerate
            .implied_fee_wu(self.drain_weights.spend_weight)
    }

    /// The dust threshold of a would-be change output. Change below this is never created.
    fn dust_threshold(&self) -> u64 {
        self.drain_weights.dust_threshold(self.dust_relay_feerate)
    }

    /// The largest `excess_with_drain_weight` for which a selection is still changeless: it is
    /// changeless (see [`drain_value`]) when its excess is `<= drain_spend_cost` OR `<
    /// dust_threshold`, and the union of those two regions is `excess <= max(drain_spend_cost,
    /// dust_threshold - 1)`.
    ///
    /// [`drain_value`]: Self::drain_value
    fn changeless_max_excess(&self) -> i64 {
        (self.drain_spend_cost() as i64).max(self.dust_threshold() as i64 - 1)
    }

    /// Whether every selection reachable down this branch (the current one and any superset of it)
    /// would have a change output — so no changeless solution exists here and the branch can be
    /// pruned.
    ///
    /// The change decision is monotone in the excess (see [`drain_value`]), so the reachable
    /// selection least likely to have change is the one with the smallest excess: the current
    /// selection plus every remaining negative-effective-value candidate. If even that selection's
    /// excess clears the changeless edge by more than a conservative rounding/correction slack
    /// (see the body), then every reachable selection has change.
    ///
    /// NOTE: this relies on candidates being sorted so that all negative effective value candidates
    /// are next to each other, which [`requires_ordering_by_descending_value_pwu`] guarantees.
    ///
    /// [`drain_value`]: Self::drain_value
    /// [`requires_ordering_by_descending_value_pwu`]: BnbMetric::requires_ordering_by_descending_value_pwu
    fn change_unavoidable(
        &mut self,
        cs: &CoinSelector<'_>,
        d_all: &CoinSelector<'_>,
        target: Target,
    ) -> bool {
        // The "least excess" construction below adds every negative-*rate*-effective-value
        // candidate to minimise the excess, which is only sound when the rate feerate is the
        // binding fee constraint. With an RBF replacement (`incremental_relay_feerate < feerate`) a
        // candidate can be negative at the rate yet positive at the replacement rate, so adding it
        // *raises* `replacement_excess`; the rate-driven construction then no longer minimises the
        // true `min(rate, absolute, replacement)` excess and could wrongly conclude change is
        // unavoidable. Never prune in that case — returning `false` is always safe, it only costs
        // extra search. (An absolute fee is safe here: `absolute_excess` only grows as inputs are
        // added, so it can never be the constraint that a *superset* brings back under threshold.)
        if target.fee.replace.is_some() {
            return false;
        }

        if self.drain_value(cs, target).is_none() {
            return false;
        }

        // With a `max_weight` cap, `drain_value` can refuse change for a descendant whose change
        // output would bust the cap (see the cap clause there) — that descendant is changeless no
        // matter its excess, so the least-excess reasoning below does not cover it. Only prune
        // when no descendant can trigger the refusal, i.e. when even the heaviest descendant
        // still fits the cap with the drain added.
        if !d_all.is_within_max_weight(target, self.drain_weights) {
            return false;
        }

        let mut least_excess = cs.clone();
        cs.unselected()
            .rev()
            .take_while(|(_, wv)| wv.effective_value(target.fee.rate) < 0.0)
            .for_each(|(index, _)| {
                least_excess.select(index);
            });

        // The construction above minimises the *weight-unit-linear* excess (a candidate's
        // `effective_value` is `value - raw_weight * spwu`), but the true excess comes from the
        // vbyte-rounded fee on the corrected weight: `fee(W) ∈ [W*spwu, W*spwu + sat_vb + 1]`,
        // where `W` includes `input_weight()`'s additions on top of raw weights (segwit header
        // +2, +1 per legacy input in a segwit tx, varint growth). A candidate with a small
        // *positive* linear ev can therefore still lower a descendant's true excess, so comparing
        // the linear-least construction against the edge is only sound with a slack covering both
        // effects. Corrections are monotone under selection, so for every descendant D:
        //
        //     excess(D) >= excess(least_excess)
        //                  - (corrections(D_all) - corrections(cs)) * spwu - (sat_vb + 1)
        //
        // (f64 everywhere sat values appear: f32's 24-bit mantissa is coarser than a sat above
        // ~0.17 BTC, and a cancellation-flipped comparison here would be an invalid prune. f64 is
        // exact for all sat amounts.)
        let corrections = |s: &CoinSelector<'_>| -> u64 {
            s.input_weight() - s.selected().map(|(_, c)| c.weight).sum::<u64>()
        };
        let slack = (corrections(d_all) - corrections(cs)) as f64 * target.fee.rate.spwu() as f64
            + target.fee.rate.as_sat_vb() as f64
            + 1.0;

        let least_excess_ewd = self.excess_with_drain_weight(&least_excess, target) as f64;
        least_excess_ewd - slack > self.changeless_max_excess() as f64
    }

    /// LP-relaxed upper bound on `D.input_weight` for changeless `D ⊇ cs` (used by the
    /// `rate_diff < 0` branch of [`bound`]).
    ///
    /// Construct `D_all = cs ∪ all unselected`. If `D_all` itself is changeless, the UB is
    /// `D_all.input_weight`. Otherwise we must exclude enough excess-contributing
    /// (positive-`effective_value`) candidates to drop `excess_with_drain_weight` down to the
    /// largest still-changeless excess. To MAXIMIZE the remaining `input_weight` we MINIMIZE the
    /// excluded weight, sorting positive-`ev` candidates by `ev / weight` descending and removing
    /// fractionally until the required `delta` is met.
    ///
    /// The LP relaxation gives a value `>=` any integer solution's excluded weight, so
    /// `D_all.input_weight - LP_min` is a safe UB for any feasible `D.input_weight`. The
    /// `input_weight()` segwit/varint corrections only ever ADD weight to the parent, never
    /// subtract from a subset — so the additive subtraction is safe in the UB direction.
    ///
    /// The knapsack credits each removed candidate its *rate*-based `effective_value`, so it is
    /// only valid when the rate feerate is the binding fee constraint. When an absolute fee or an
    /// RBF replacement is present it falls back to the trivial (always valid) `D_all` bound — see
    /// the guard below.
    ///
    /// [`bound`]: BnbMetric::bound
    fn ub_changeless_input_weight(
        &self,
        cs: &CoinSelector<'_>,
        d_all: &CoinSelector<'_>,
        target: Target,
    ) -> f64 {
        let d_all_iw = d_all.input_weight() as f64;

        // With a `max_weight` cap, `drain_value` can refuse a heavy descendant's change (see the
        // cap clause there): such a descendant is changeless while keeping every
        // excess-contributing candidate, so the knapsack's premise below (changeless => must shed
        // positive-ev weight) fails. Whenever the refusal is reachable, fall back to the `D_all`
        // bound clamped by the cap (every scoreable descendant must fit the cap without a drain,
        // and the non-input weight is the same for every descendant).
        if let Some(max_weight) = target.max_weight {
            if !d_all.is_within_max_weight(target, self.drain_weights) {
                let non_input_weight =
                    cs.weight(target.outputs, DrainWeights::NONE) - cs.input_weight();
                return d_all_iw.min(max_weight.saturating_sub(non_input_weight) as f64);
            }
        }

        // The knapsack below credits each removed candidate its *rate*-based `effective_value`,
        // which equals the true excess reduction only when the rate feerate is the binding fee
        // constraint. But `excess` is `min(rate, absolute, replacement)`: with an absolute fee,
        // removing a candidate drops `absolute_excess` by its full `value`; with an RBF replacement
        // (`incremental_relay_feerate < feerate`) it drops `replacement_excess` by `value - weight *
        // incremental_relay_feerate` — both larger than its rate-ev. Crediting the smaller rate-ev
        // would over-remove weight and yield a *below*-true (invalid, too-tight) upper bound, so
        // fall back to the trivial `D_all` upper bound whenever a non-rate constraint can bind.
        if target.fee.absolute > 0 || target.fee.replace.is_some() {
            return d_all_iw;
        }

        let delta = self.excess_with_drain_weight(d_all, target) - self.changeless_max_excess();

        // The knapsack below reasons in weight-unit-linear effective values, but a descendant's
        // true excess comes from the vbyte-rounded fee (`fee(W) ∈ [W*spwu, W*spwu + sat_vb +
        // 1]`), so a descendant can become changeless while shedding up to `sat_vb + 1` sats less
        // than `delta` in linear terms. Demanding the full `delta` would over-remove weight and
        // yield a below-true (invalid) upper bound. (`input_weight()`'s corrections err in the
        // safe direction here: a removal sheds *at most* its linear ev.)
        // (f64 for the same reason as in `change_unavoidable`: sat-magnitude values must not lose
        // whole sats to float rounding.)
        let mut remaining = delta as f64 - (target.fee.rate.as_sat_vb() as f64 + 1.0);
        if remaining <= 0.0 {
            return d_all_iw;
        }

        let spwu = target.fee.rate.spwu() as f64;
        let mut pos: Vec<(f64, f64)> = cs
            .unselected()
            .filter_map(|(_, c)| {
                let ev = c.value as f64 - c.weight as f64 * spwu;
                if ev > 0.0 {
                    Some((ev, c.weight as f64))
                } else {
                    None
                }
            })
            .collect();
        pos.sort_by(|a, b| {
            let r_a = a.0 / a.1;
            let r_b = b.0 / b.1;
            r_b.partial_cmp(&r_a).unwrap_or(core::cmp::Ordering::Equal)
        });

        let mut removed_weight = 0.0_f64;
        for (ev, w) in pos {
            if remaining <= 0.0 {
                break;
            }
            if ev >= remaining {
                removed_weight += w * (remaining / ev);
                remaining = 0.0;
            } else {
                removed_weight += w;
                remaining -= ev;
            }
        }
        if remaining > 0.0 {
            // Unreachable when `change_unavoidable = false` (which the caller already checked).
            // Fall back to the loose `D_all`-based bound rather than fabricating a tight one.
            return d_all_iw;
        }
        d_all_iw - removed_weight
    }
}

impl BnbMetric for ChangelessWaste {
    fn drain(&mut self, _cs: &CoinSelector<'_>, _target: Target) -> Drain {
        // By definition a changeless selection never has a change output.
        Drain::NONE
    }

    fn score(&mut self, cs: &CoinSelector<'_>, target: Target) -> Option<Ordf32> {
        if !cs.is_funded(target) {
            return None;
        }
        if !cs.is_within_max_weight(target, DrainWeights::NONE) {
            return None;
        }
        // Reject selections that would have change — this metric only scores changeless solutions.
        if self.drain_value(cs, target).is_some() {
            return None;
        }
        let waste = cs.waste(target, self.long_term_feerate, Drain::NONE, 1.0);
        Some(Ordf32(waste))
    }

    fn bound(&mut self, cs: &CoinSelector<'_>, target: Target) -> Option<Ordf32> {
        if !cs.is_within_max_weight(target, DrainWeights::NONE) {
            return None;
        }

        // Both helpers below reason about the heaviest descendant; build it once, since `bound`
        // runs at every BnB node.
        let d_all = {
            let mut d_all = cs.clone();
            d_all.select_all();
            d_all
        };

        // Prune branches where every descendant is forced to have a change output.
        if self.change_unavoidable(cs, &d_all, target) {
            return None;
        }

        let rate_diff = target.fee.rate.spwu() - self.long_term_feerate.spwu();

        // For any changeless target-meeting descendant D ⊇ cs:
        //     score(D) = D.input_weight * rate_diff + max(0, D.excess)
        //
        // and `D.excess >= 0` (target met), so `score(D) >= D.input_weight * rate_diff`. The
        // bound therefore reduces to bounding `D.input_weight` in the right direction.

        if rate_diff < 0.0 {
            // rate_diff < 0: we want an UPPER bound on `D.input_weight`. `all_selected` is a
            // safe but loose UB; we tighten by LP-relaxed knapsack over candidates that
            // *must* be excluded to keep the selection changeless.
            let ub = self.ub_changeless_input_weight(cs, &d_all, target);
            return Some(Ordf32((ub * rate_diff as f64) as f32));
        }

        // rate_diff >= 0: we want a LOWER bound on `D.input_weight`, i.e. on the *additional raw*
        // input weight any target-meeting descendant must add on top of `cs`. Everything below is
        // weight-unit-linear arithmetic on raw candidate weights:
        //
        // - any D pays at least `W(D) * spwu` in fee (the vbyte rounding only ever adds), and
        // - `D.input_weight >= cs.input_weight + Σ raw(D∖cs)` (`cs`'s varint/segwit corrections
        //   are common to every descendant and corrections only grow under selection),
        //
        // so `target met => Σ ev_lin(D∖cs) >= gap` where `ev_lin = value - raw_weight * spwu` and
        // `gap = target.value + W(cs)*spwu - selected_value(cs)`. The fractional knapsack over
        // the highest-`value_pwu` candidates minimizes the added raw weight subject to that
        // (`ev_lin / raw = value_pwu - spwu` is monotone in `value_pwu`, so the required sorted
        // order is already the iteration order and positive-`ev_lin` candidates form a prefix).
        //
        // Note this deliberately does NOT reuse the `resize_bound` walk: that walk's crossing
        // point and scale are computed from the greedy prefix's *actual* (corrected, rounded)
        // fee and weight, which a descendant avoiding the prefix's segwit corrections can beat —
        // an invalid (too-high) lower bound. Raw-linear arithmetic can't be beaten.
        // Funded-ness at the boundary is decided in exact integer arithmetic, never by a float
        // sign: for a funded `cs` the baseline is already the bound and no walk is needed.
        if cs.is_funded(target) {
            return Some(Ordf32(cs.input_weight() as f32 * rate_diff));
        }

        // f64 keeps every sat amount exact (f32's 24-bit mantissa is coarser than a sat above
        // ~0.17 BTC, and this subtraction cancels catastrophically at the funding boundary).
        let spwu = target.fee.rate.spwu() as f64;
        let mut gap = target.value() as f64
            + cs.weight(target.outputs, DrainWeights::NONE) as f64 * spwu
            - cs.selected_value() as f64;
        let mut extra_raw = 0.0_f64;
        for (_, candidate) in cs.unselected() {
            if gap <= 0.0 {
                break;
            }
            let ev = candidate.value as f64 - candidate.weight as f64 * spwu;
            if ev <= 0.0 {
                // Sorted order: no later candidate can contribute value towards the gap either.
                break;
            }
            let raw = candidate.weight as f64;
            if ev >= gap {
                extra_raw += raw * (gap / ev);
                gap = 0.0;
            } else {
                extra_raw += raw;
                gap -= ev;
            }
        }
        if gap > 0.5 {
            // Even selecting every positive-ev candidate cannot reach the target's feerate: no
            // descendant is fundable, so prune. A fundable descendant requires a *linear* gap
            // `<= 0` (the true fee only rounds up from the linear fee), and the f64 arithmetic
            // above is exact to well under a sat, so any residual gap over half a sat is a true
            // shortfall. A sub-half-sat residual falls through and is bounded instead of pruned.
            return None;
        }
        Some(Ordf32(
            ((cs.input_weight() as f64 + extra_raw) * rate_diff as f64) as f32,
        ))
    }

    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        true
    }
}
