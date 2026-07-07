//! [`SelectionView`] — a read-only, O(1)-queryable view over a
//! [`CoinSelector`]'s current selection.
//!
//! BnB maintains an internal cache of running aggregates so that
//! [`crate::BnbMetric`] implementations can answer queries like
//! "what's the selected value?" or "what's the selection's input weight?"
//! in O(1) rather than walking the bitset each time. Outside BnB, callers
//! get a view via [`CoinSelector::compute_view`].

use alloc::borrow::Cow;
use core::ops::Deref;

#[allow(unused)] // needed for `f32::ceil` under no_std; std provides it inherently
use crate::float::FloatExt;
use crate::{
    varint_size, Candidate, ChangePolicy, CoinSelector, Drain, DrainWeights, FeeRate, Target,
    TargetOutputs, TX_FIXED_FIELD_WEIGHT,
};

/// Running aggregates over a selection. Internal to the crate — external
/// callers go through [`SelectionView`] via [`CoinSelector::compute_view`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SelectionCache {
    value_sum: u64,
    weight_sum: u64,
    input_count: usize,
    segwit_count: usize,
    candidate_count: usize,
}

impl SelectionCache {
    /// Build a cache by walking the selector's currently selected candidates.
    ///
    /// O(|selected|). After this, prefer [`add`](Self::add) / [`sub`](Self::sub)
    /// to keep it in sync rather than rebuilding from scratch.
    pub(crate) fn from_selector(cs: &CoinSelector<'_>) -> Self {
        let mut c = Self::default();
        for (_, cand) in cs.selected() {
            c.add(cand);
        }
        c
    }

    /// Equivalent to `CoinSelector::input_weight`, computed in O(1) from the
    /// cached aggregates.
    fn input_weight(&self) -> u64 {
        let is_segwit_tx = self.segwit_count > 0;
        let witness_header_extra_weight = is_segwit_tx as u64 * 2;
        let nonsegwit_count = self.candidate_count - self.segwit_count;
        let segwit_adjust = if is_segwit_tx {
            nonsegwit_count as u64
        } else {
            0
        };
        let input_varint_weight = varint_size(self.input_count) * 4;
        input_varint_weight + self.weight_sum + segwit_adjust + witness_header_extra_weight
    }

    /// Apply the effect of selecting `candidate`.
    pub(crate) fn add(&mut self, candidate: Candidate) {
        self.value_sum += candidate.value;
        self.weight_sum += candidate.weight;
        self.input_count += candidate.input_count;
        if candidate.is_segwit {
            self.segwit_count += 1;
        }
        self.candidate_count += 1;
    }

    /// Apply the effect of deselecting `candidate`.
    pub(crate) fn sub(&mut self, candidate: Candidate) {
        self.value_sum -= candidate.value;
        self.weight_sum -= candidate.weight;
        self.input_count -= candidate.input_count;
        if candidate.is_segwit {
            self.segwit_count -= 1;
        }
        self.candidate_count -= 1;
    }
}

/// Read-only view of a [`CoinSelector`]'s current selection, with O(1)
/// accessors for `selected_value`, `input_weight`, `excess`, `is_funded`,
/// `drain`, and friends.
///
/// Obtained via [`CoinSelector::compute_view`] for ad-hoc queries, or as the
/// argument to [`BnbMetric::score`](crate::BnbMetric::score) /
/// [`BnbMetric::bound`](crate::BnbMetric::bound) in the BnB hot path.
///
/// Methods on this type read pre-computed aggregates rather than walking the
/// selected bitset, so they are constant-time (except
/// [`is_fundable`](Self::is_fundable), which iterates the
/// unselected candidates). During branch-and-bound search, the cache is
/// maintained incrementally as branches are explored, which is what makes the
/// metric evaluator "delta-aware".
///
/// `SelectionView` implements [`Deref<Target = CoinSelector>`](Deref), so every
/// `&self` method of [`CoinSelector`] is reachable directly on the view.
/// Mutating methods take `&mut self` and are *not* reachable through `Deref`,
/// so the view stays read-only.
#[derive(Clone, Debug)]
pub struct SelectionView<'a> {
    selector: &'a CoinSelector<'a>,
    cache: Cow<'a, SelectionCache>,
}

impl<'a> Deref for SelectionView<'a> {
    type Target = CoinSelector<'a>;
    fn deref(&self) -> &CoinSelector<'a> {
        self.selector
    }
}

impl<'a> SelectionView<'a> {
    /// Construct a view that borrows an externally maintained cache. Caller is
    /// responsible for keeping the cache in sync with `selector`'s selection.
    pub(crate) fn with_cache(selector: &'a CoinSelector<'a>, cache: &'a SelectionCache) -> Self {
        Self {
            selector,
            cache: Cow::Borrowed(cache),
        }
    }

    /// Construct a view by building a fresh cache from the selector. Used by
    /// [`CoinSelector::compute_view`]; O(|selected|) one-time cache build.
    pub(crate) fn from_selector(selector: &'a CoinSelector<'a>) -> Self {
        Self {
            selector,
            cache: Cow::Owned(SelectionCache::from_selector(selector)),
        }
    }

    /// Access the underlying [`CoinSelector`] reference for cases where the
    /// `Deref` impl doesn't suffice (e.g. cloning the selector, or passing it
    /// where a `&'a CoinSelector` with the original lifetime is required).
    pub fn selector(&self) -> &'a CoinSelector<'a> {
        self.selector
    }

    fn cache(&self) -> &SelectionCache {
        &self.cache
    }

    /// Update the cached aggregates as if `cand` were selected, without
    /// modifying the underlying [`CoinSelector`]. Useful for exploring
    /// hypothetical extensions of the current selection.
    ///
    /// On a borrowed view this triggers a one-time cache clone (via
    /// [`Cow::to_mut`](alloc::borrow::Cow::to_mut)); subsequent calls mutate
    /// in place. The view's selector-iteration methods (e.g. `unselected()`)
    /// still reflect the original selection, so advance a single iterator
    /// across the loop rather than re-creating one per step.
    pub fn add(&mut self, cand: Candidate) {
        self.cache.to_mut().add(cand);
    }

    /// Update the cached aggregates as if `cand` were deselected. See
    /// [`add`](Self::add) for caveats.
    pub fn sub(&mut self, cand: Candidate) {
        self.cache.to_mut().sub(cand);
    }

    /// Absolute value sum of all selected inputs.
    pub fn selected_value(&self) -> u64 {
        self.cache().value_sum
    }

    /// The weight of the inputs including the witness header and the varint
    /// for the number of inputs.
    pub fn input_weight(&self) -> u64 {
        self.cache().input_weight()
    }

    /// Current weight of transaction implied by the selection.
    ///
    /// If you don't have any drain outputs (only target outputs) just set
    /// `drain_weight` to [`DrainWeights::NONE`].
    pub fn weight(&self, target_outputs: TargetOutputs, drain_weight: DrainWeights) -> u64 {
        TX_FIXED_FIELD_WEIGHT
            + self.input_weight()
            + target_outputs.output_weight_with_drain(drain_weight)
    }

    /// How much the current selection overshoots the value need to satisfy
    /// `target.fee.rate` and `target.value` (while ignoring
    /// `target.fee.absolute`).
    pub fn rate_excess(&self, target: Target, drain: Drain) -> i64 {
        self.selected_value() as i64
            - target.value() as i64
            - drain.value as i64
            - target
                .fee
                .rate
                .implied_fee(self.weight(target.outputs, drain.weights)) as i64
    }

    /// Same as [`rate_excess`](Self::rate_excess) except `target.fee.rate` is
    /// applied to the implied transaction's weight units directly without any
    /// conversion to vbytes.
    pub fn rate_excess_wu(&self, target: Target, drain: Drain) -> i64 {
        self.selected_value() as i64
            - target.value() as i64
            - drain.value as i64
            - target
                .fee
                .rate
                .implied_fee_wu(self.weight(target.outputs, drain.weights)) as i64
    }

    /// How much the current selection overshoots the value needed to satisfy
    /// `target.fee.absolute` and `target.value` (while ignoring
    /// `target.fee.rate`).
    pub fn absolute_excess(&self, target: Target, drain: Drain) -> i64 {
        self.selected_value() as i64
            - target.value() as i64
            - drain.value as i64
            - target.fee.absolute as i64
    }

    /// How much the current selection overshoots the value needed to satisfy
    /// RBF's rule 4.
    pub fn replacement_excess(&self, target: Target, drain: Drain) -> i64 {
        let mut replacement_excess_needed = 0;
        if let Some(replace) = target.fee.replace {
            replacement_excess_needed =
                replace.min_fee_to_do_replacement(self.weight(target.outputs, drain.weights))
        }
        self.selected_value() as i64
            - target.value() as i64
            - drain.value as i64
            - replacement_excess_needed as i64
    }

    /// Same as [`replacement_excess`](Self::replacement_excess) except the
    /// replacement fee is calculated using weight units directly without any
    /// conversion to vbytes.
    pub fn replacement_excess_wu(&self, target: Target, drain: Drain) -> i64 {
        let mut replacement_excess_needed = 0;
        if let Some(replace) = target.fee.replace {
            replacement_excess_needed =
                replace.min_fee_to_do_replacement_wu(self.weight(target.outputs, drain.weights))
        }
        self.selected_value() as i64
            - target.value() as i64
            - drain.value as i64
            - replacement_excess_needed as i64
    }

    /// How much the current selection overshoots the value needed to achieve
    /// `target`.
    ///
    /// In order for the resulting transaction to be valid this must be 0 or
    /// above. If it's above 0 the transaction will overpay for what it needs
    /// to reach `target`.
    pub fn excess(&self, target: Target, drain: Drain) -> i64 {
        self.rate_excess(target, drain)
            .min(self.absolute_excess(target, drain))
            .min(self.replacement_excess(target, drain))
    }

    /// Whether the selection covers the target value (i.e. [`excess`](Self::excess) is
    /// non-negative) with the specific `drain` included, ignoring [`Target::max_weight`].
    ///
    /// This is **monotone**: selecting more never un-meets it. It deliberately does *not* include
    /// the weight cap — see [`is_within_max_weight`](Self::is_within_max_weight).
    ///
    /// If [`is_funded`](Self::is_funded) is true and `drain` is produced by
    /// [`drain`](Self::drain) for this selection, this method will also be true.
    pub fn is_funded_with_drain(&self, target: Target, drain: Drain) -> bool {
        self.excess(target, drain) >= 0
    }

    /// Whether the selection covers the target **value** (net of input fees), i.e. [`excess`] is
    /// non-negative. **Monotone** (selecting more never un-meets it), and it deliberately does
    /// *not* check [`Target::max_weight`] — that is the separate, anti-monotone
    /// [`is_within_max_weight`]. See [`is_funded_with_drain`] for the version that
    /// accounts for a specific `drain`.
    ///
    /// [`excess`]: Self::excess
    /// [`is_within_max_weight`]: Self::is_within_max_weight
    /// [`is_funded_with_drain`]: Self::is_funded_with_drain
    pub fn is_funded(&self, target: Target) -> bool {
        self.is_funded_with_drain(target, Drain::NONE)
    }

    /// Whether the tx implied by the current selection plus a drain of `drain_weights` is within
    /// [`Target::max_weight`]. Pass [`DrainWeights::NONE`] for a changeless tx.
    ///
    /// Always `true` when `max_weight` is `None`. Note this is the *anti-monotone* half of
    /// feasibility (adding inputs adds weight), so it is kept separate from the monotone
    /// value-only [`is_funded`](Self::is_funded).
    pub fn is_within_max_weight(&self, target: Target, drain_weights: DrainWeights) -> bool {
        match target.max_weight {
            Some(max_weight) => self.weight(target.outputs, drain_weights) <= max_weight,
            None => true,
        }
    }

    /// Whether the candidates can cover this `target`'s **value** (net of input fees) — i.e.
    /// whether enough value is reachable for [`is_funded`](Self::is_funded) to hold. Respects
    /// [`banned`](CoinSelector::banned) candidates.
    ///
    /// Greedily accounts for every additional effective candidate at the target's feerate in a
    /// cloned view's cache, then checks whether the target value is met. Selecting *all* effective
    /// inputs maximizes the value available, so if that can't meet the target value, nothing can.
    /// Monotone, hence exact. Unlike the other methods on this type, this is O(|unselected|),
    /// not O(1).
    ///
    /// NOTE: this does **not** account for [`Target::max_weight`] — a `true` result can still be
    /// infeasible under the weight cap. Use [`select_until_target_met`] or branch and bound (both
    /// of which enforce the cap) to actually build a selection.
    ///
    /// [`select_until_target_met`]: CoinSelector::select_until_target_met
    pub fn is_fundable(&self, target: Target) -> bool {
        let mut local = self.clone();
        let feerate = target.fee.rate;
        for (_, cand) in self.unselected() {
            if cand.effective_value(feerate) > 0.0 {
                local.add(cand);
            }
        }
        local.is_funded(target)
    }

    /// How much extra value needs to be selected to reach the target.
    pub fn missing(&self, target: Target) -> u64 {
        let excess = self.excess(target, Drain::NONE);
        if excess < 0 {
            excess.unsigned_abs()
        } else {
            0
        }
    }

    /// The actual fee the selection would pay if it was used in a transaction
    /// with `target_value` value for outputs and a change output of
    /// `drain_value`.
    ///
    /// Can be negative when the selection is invalid (outputs greater than
    /// inputs).
    pub fn fee(&self, target_value: u64, drain_value: u64) -> i64 {
        self.selected_value() as i64 - target_value as i64 - drain_value as i64
    }

    /// The fee the current selection and `drain_weights` should pay to satisfy
    /// `target.fee`.
    ///
    /// This compares the fee calculated from the target feerate with the fee
    /// calculated from the [`Replace`](crate::Replace) constraints and returns
    /// the larger of the two.
    pub fn implied_fee(&self, target: Target, drain_weights: DrainWeights) -> u64 {
        let tx_weight = self.weight(target.outputs, drain_weights);
        let mut implied_fee = target
            .fee
            .rate
            .implied_fee(tx_weight)
            .max(target.fee.absolute);
        if let Some(replace) = target.fee.replace {
            implied_fee = Ord::max(implied_fee, replace.min_fee_to_do_replacement(tx_weight));
        }
        implied_fee
    }

    /// The feerate the transaction would have if we were to use this selection
    /// of inputs to achieve the `target`'s value and weight. It is essentially
    /// telling you what target feerate you currently have.
    ///
    /// Returns `None` if the feerate would be negative or infinity.
    pub fn implied_feerate(&self, target_outputs: TargetOutputs, drain: Drain) -> Option<FeeRate> {
        let numerator =
            self.selected_value() as i64 - target_outputs.value_sum as i64 - drain.value as i64;
        let denom = self.weight(target_outputs, drain.weights);
        if numerator < 0 || denom == 0 {
            return None;
        }
        Some(FeeRate::from_sat_per_wu(numerator as f32 / denom as f32))
    }

    /// The value of the current selected inputs minus the fee needed to pay
    /// for them at `feerate`.
    pub fn effective_value(&self, feerate: FeeRate) -> i64 {
        self.selected_value() as i64 - (self.input_weight() as f32 * feerate.spwu()).ceil() as i64
    }

    /// The value the change output should have to drain the excess while
    /// maintaining the constraints of `target` and respecting `change_policy`.
    ///
    /// Returns `None` if no change output should be added according to the
    /// policy.
    pub fn drain_value(&self, target: Target, change_policy: ChangePolicy) -> Option<u64> {
        let excess = self.excess(
            target,
            Drain {
                weights: change_policy.drain_weights,
                value: 0,
            },
        );
        if excess > change_policy.min_value as i64 {
            Some(excess as u64)
        } else {
            None
        }
    }

    /// Figures out whether the current selection should have a change output
    /// given the `change_policy`. If it should not, returns [`Drain::NONE`].
    /// Otherwise the returned drain has the value of
    /// [`drain_value`](Self::drain_value).
    ///
    /// If [`is_funded`](Self::is_funded) is true for this selection
    /// then [`is_funded_with_drain`](Self::is_funded_with_drain) will
    /// also be true with the drain returned here.
    #[must_use]
    pub fn drain(&self, target: Target, change_policy: ChangePolicy) -> Drain {
        match self.drain_value(target, change_policy) {
            Some(value) => Drain {
                weights: change_policy.drain_weights,
                value,
            },
            None => Drain::NONE,
        }
    }

    /// The waste created by the current selection as measured by the
    /// [waste metric].
    ///
    /// `excess_discount` must be between `0.0..=1.0`; passing `1.0` gives no
    /// discount.
    ///
    /// [waste metric]: https://bitcoin.stackexchange.com/questions/113622/what-does-waste-metric-mean-in-the-context-of-coin-selection
    pub fn waste(
        &self,
        target: Target,
        long_term_feerate: FeeRate,
        drain: Drain,
        excess_discount: f32,
    ) -> f32 {
        debug_assert!((0.0..=1.0).contains(&excess_discount));
        let mut waste =
            self.input_weight() as f32 * (target.fee.rate.spwu() - long_term_feerate.spwu());

        if drain.is_none() {
            // We don't allow negative excess waste since negative excess just means you haven't
            // satisified target yet in which case you probably shouldn't be calling this function.
            let mut excess_waste = self.excess(target, drain).max(0) as f32;
            // we allow caller to discount this waste depending on how wasteful excess actually is
            // to them.
            excess_waste *= excess_discount.clamp(0.0, 1.0);
            waste += excess_waste;
        } else {
            waste +=
                drain
                    .weights
                    .waste(target.fee.rate, long_term_feerate, target.outputs.n_outputs);
        }

        waste
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Candidate, CoinSelector, TXIN_BASE_WEIGHT};
    use proptest::prelude::*;

    /// The original O(n) `CoinSelector::input_weight` definition, kept verbatim as an
    /// *independent* oracle for the cache's O(1) formula: `from_selector` is itself built on
    /// [`SelectionCache::add`], so comparing the incremental cache against a rebuilt one alone
    /// would validate the formula against itself.
    fn input_weight_oracle(cs: &CoinSelector<'_>) -> u64 {
        let is_segwit_tx = cs.selected().any(|(_, wv)| wv.is_segwit);
        let witness_header_extra_weight = is_segwit_tx as u64 * 2;

        let input_count = cs.selected().map(|(_, wv)| wv.input_count).sum::<usize>();
        let input_varint_weight = varint_size(input_count) * 4;

        let selected_weight: u64 = cs
            .selected()
            .map(|(_, candidate)| {
                let mut weight = candidate.weight;
                if is_segwit_tx && !candidate.is_segwit {
                    // non-segwit candidates do not have the witness length field included in
                    // their weight field so we need to add 1 here if it's in a segwit tx.
                    weight += 1;
                }
                weight
            })
            .sum();

        input_varint_weight + selected_weight + witness_header_extra_weight
    }

    fn synth_candidates(values: &[u64]) -> alloc::vec::Vec<Candidate> {
        values
            .iter()
            .enumerate()
            .map(|(i, &v)| Candidate {
                value: v,
                weight: TXIN_BASE_WEIGHT + 107,
                input_count: 1 + (i % 3),
                // Mix segwit and non-segwit to exercise the adjustment.
                is_segwit: i % 2 == 0,
            })
            .collect()
    }

    #[test]
    fn empty_cache_matches_empty_selector() {
        let candidates: alloc::vec::Vec<Candidate> = alloc::vec::Vec::new();
        let cs = CoinSelector::new(&candidates);
        let cache = SelectionCache::from_selector(&cs);
        assert_eq!(cache.value_sum, cs.compute_view().selected_value());
        assert_eq!(cache.input_weight(), cs.compute_view().input_weight());
    }

    #[test]
    fn add_matches_select() {
        let candidates = synth_candidates(&[100, 200, 300, 400, 500]);
        let mut cs = CoinSelector::new(&candidates);
        let mut cache = SelectionCache::default();
        for i in [0, 2, 4] {
            cs.select(i);
            cache.add(candidates[i]);
            assert_eq!(cache.value_sum, cs.compute_view().selected_value());
            assert_eq!(cache.input_weight(), cs.compute_view().input_weight());
        }
    }

    proptest! {
        /// Random sequence of select/deselect: incremental cache must match
        /// `SelectionCache::from_selector` rebuilt from scratch.
        #[test]
        fn matches_from_scratch_under_random_ops(
            values in prop::collection::vec(1u64..1_000_000, 1..32),
            ops in prop::collection::vec((any::<bool>(), 0usize..32), 0..200),
        ) {
            let candidates = synth_candidates(&values);
            let mut cs = CoinSelector::new(&candidates);
            let mut cache = SelectionCache::default();
            for (is_select, raw_idx) in ops {
                let idx = raw_idx % candidates.len();
                if is_select {
                    if cs.select(idx) {
                        cache.add(candidates[idx]);
                    }
                } else if cs.deselect(idx) {
                    cache.sub(candidates[idx]);
                }
                let rebuilt = SelectionCache::from_selector(&cs);
                prop_assert_eq!(&cache, &rebuilt);
                prop_assert_eq!(cache.value_sum, cs.compute_view().selected_value());
                prop_assert_eq!(cache.input_weight(), cs.compute_view().input_weight());
                // Independent oracles: straight sums/walks over the selection, sharing no
                // code with `SelectionCache`.
                prop_assert_eq!(
                    cache.value_sum,
                    cs.selected().map(|(_, c)| c.value).sum::<u64>()
                );
                prop_assert_eq!(cache.input_weight(), input_weight_oracle(&cs));
            }
        }
    }
}
