//! Cached selection queries and hypothetical updates.

use alloc::{borrow::Cow, vec::Vec};
use core::ops::Deref;

#[allow(unused)]
use crate::float::FloatExt;
use crate::{
    varint_size, Bitset, Candidate, ChangePolicy, CoinSelector, Drain, DrainWeights, FeeRate,
    SelectionProblem, TargetOutputs, TX_FIXED_FIELD_WEIGHT,
};

/// Running aggregates used by branch and bound.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SelectionCache {
    value_sum: u64,
    weight_sum: u64,
    segwit_count: usize,
    legacy_count: usize,
    private_weight: u64,
    private_fee: u64,
    shared_refcounts: Vec<u32>,
    shared_weight: u64,
    shared_fee: u64,
    private_reachable_surplus: f64,
    shared_reachable_refcounts: Vec<u32>,
    shared_reachable_surplus: f64,
    ancestor_fee_precision_slack: u64,
    /// Value and weight of the still-undecided candidates worth selecting, i.e. those neither
    /// selected nor banned whose standalone effective value is positive. Candidates that cost more
    /// weight than they bring are left out because they only ever lower the total, so the pair
    /// stays an upper bound on what the rest of this branch can still contribute.
    undecided_value: u64,
    undecided_weight: u64,
    /// Undecided candidates that weigh nothing but carry value. They make every fee deficit
    /// closable at zero added weight, so the bound must know whether any remain.
    undecided_weightless_value: usize,
    selected: Bitset,
}

impl SelectionCache {
    pub(crate) fn from_selector(selector: &CoinSelector<'_>) -> Self {
        let mut cache = Self {
            value_sum: 0,
            weight_sum: 0,
            segwit_count: 0,
            legacy_count: 0,
            private_weight: 0,
            private_fee: 0,
            shared_refcounts: alloc::vec![
                0;
                if selector.problem().has_shared_ancestors() {
                    selector.problem().ancestors().len()
                } else {
                    0
                }
            ],
            shared_weight: 0,
            shared_fee: 0,
            private_reachable_surplus: 0.0,
            shared_reachable_refcounts: alloc::vec![
                0;
                if selector.problem().has_shared_ancestors() {
                    selector.problem().ancestors().len()
                } else {
                    0
                }
            ],
            shared_reachable_surplus: 0.0,
            ancestor_fee_precision_slack: selector.problem().ancestor_fee_precision_slack(),
            undecided_value: 0,
            undecided_weight: 0,
            undecided_weightless_value: 0,
            // BnB transitions each candidate exactly once, so it needs no duplicate-tracking
            // bitset. Public hypothetical updates allocate this lazily in `track_selected`.
            selected: Bitset::default(),
        };
        // Every candidate starts undecided and reachable; the two loops below then account for the
        // ones that are already selected or already banned.
        for (index, _) in selector.candidates() {
            cache.add_reachable(selector.problem(), index);
        }
        for (index, candidate) in selector.selected() {
            cache.add(selector.problem(), index, candidate, true);
        }
        for index in selector.banned().iter() {
            if !selector.is_selected(index) {
                cache.ban(selector.problem(), index);
            }
        }
        cache
    }

    fn ancestor_surplus(problem: &SelectionProblem, (weight, fee): (u64, u64)) -> f64 {
        (fee as f64 - weight as f64 * problem.target().fee.rate.spwu() as f64).max(0.0)
    }

    /// Whether this candidate brings in more value than its own weight costs at the target
    /// feerate. One that doesn't can only ever lower a running total, so the undecided aggregate
    /// leaves it out and stays an upper bound.
    fn is_worth_selecting(problem: &SelectionProblem, index: usize) -> bool {
        problem
            .candidate(index)
            .effective_value(problem.target().fee.rate)
            > 0.0
    }

    fn add_reachable(&mut self, problem: &SelectionProblem, index: usize) {
        if Self::is_worth_selecting(problem, index) {
            let candidate = problem.candidate(index);
            self.undecided_value += candidate.value;
            self.undecided_weight += candidate.weight;
            if candidate.weight == 0 && candidate.value > 0 {
                self.undecided_weightless_value += 1;
            }
        }
        if !problem.has_ancestors() {
            return;
        }
        if problem.has_private_ancestors() {
            self.private_reachable_surplus +=
                Self::ancestor_surplus(problem, problem.private_ancestors(index));
        }
        if problem.has_shared_ancestors() {
            for &ancestor in problem.shared_drags_in(index) {
                let ancestor = ancestor as usize;
                if self.shared_reachable_refcounts[ancestor] == 0
                    && self.shared_refcounts[ancestor] == 0
                {
                    self.shared_reachable_surplus +=
                        Self::ancestor_surplus(problem, problem.ancestors()[ancestor]);
                }
                self.shared_reachable_refcounts[ancestor] += 1;
            }
        }
    }

    fn remove_reachable(&mut self, problem: &SelectionProblem, index: usize) {
        if Self::is_worth_selecting(problem, index) {
            let candidate = problem.candidate(index);
            self.undecided_value -= candidate.value;
            self.undecided_weight -= candidate.weight;
            if candidate.weight == 0 && candidate.value > 0 {
                self.undecided_weightless_value -= 1;
            }
        }
        if !problem.has_ancestors() {
            return;
        }
        if problem.has_private_ancestors() {
            self.private_reachable_surplus -=
                Self::ancestor_surplus(problem, problem.private_ancestors(index));
        }
        if problem.has_shared_ancestors() {
            for &ancestor in problem.shared_drags_in(index) {
                let ancestor = ancestor as usize;
                self.shared_reachable_refcounts[ancestor] -= 1;
                if self.shared_reachable_refcounts[ancestor] == 0
                    && self.shared_refcounts[ancestor] == 0
                {
                    self.shared_reachable_surplus -=
                        Self::ancestor_surplus(problem, problem.ancestors()[ancestor]);
                }
            }
        }
    }

    fn input_weight(&self) -> u64 {
        let is_segwit_tx = self.segwit_count > 0;
        let witness_header_extra_weight = is_segwit_tx as u64 * 2;
        let input_count = self.segwit_count + self.legacy_count;
        let input_varint_weight = varint_size(input_count) * 4;
        let legacy_witness_lengths = is_segwit_tx as u64 * self.legacy_count as u64;
        input_varint_weight + self.weight_sum + legacy_witness_lengths + witness_header_extra_weight
    }

    pub(crate) fn add(
        &mut self,
        problem: &SelectionProblem,
        index: usize,
        candidate: Candidate,
        was_reachable: bool,
    ) {
        if self.selected.capacity() > 0 && !self.selected.insert(index) {
            return;
        }
        self.value_sum += candidate.value;
        self.weight_sum += candidate.weight;
        self.segwit_count += candidate.segwit_count;
        self.legacy_count += candidate.legacy_count;
        if was_reachable {
            self.remove_reachable(problem, index);
        }

        if problem.has_private_ancestors() {
            let (weight, fee) = problem.private_ancestors(index);
            self.private_weight += weight;
            self.private_fee += fee;
        }
        if problem.has_shared_ancestors() {
            for &ancestor in problem.shared_drags_in(index) {
                let ancestor = ancestor as usize;
                if self.shared_refcounts[ancestor] == 0 {
                    let (weight, fee) = problem.ancestors()[ancestor];
                    self.shared_weight += weight;
                    self.shared_fee += fee;
                    if self.shared_reachable_refcounts[ancestor] > 0 {
                        self.shared_reachable_surplus -=
                            Self::ancestor_surplus(problem, (weight, fee));
                    }
                }
                self.shared_refcounts[ancestor] += 1;
            }
        }
    }

    pub(crate) fn sub(
        &mut self,
        problem: &SelectionProblem,
        index: usize,
        candidate: Candidate,
        is_addable: bool,
    ) {
        if self.selected.capacity() > 0 && !self.selected.remove(index) {
            return;
        }
        self.value_sum -= candidate.value;
        self.weight_sum -= candidate.weight;
        self.segwit_count -= candidate.segwit_count;
        self.legacy_count -= candidate.legacy_count;

        if problem.has_private_ancestors() {
            let (weight, fee) = problem.private_ancestors(index);
            self.private_weight -= weight;
            self.private_fee -= fee;
        }
        if problem.has_shared_ancestors() {
            for &ancestor in problem.shared_drags_in(index) {
                let ancestor = ancestor as usize;
                self.shared_refcounts[ancestor] -= 1;
                if self.shared_refcounts[ancestor] == 0 {
                    let (weight, fee) = problem.ancestors()[ancestor];
                    self.shared_weight -= weight;
                    self.shared_fee -= fee;
                    if self.shared_reachable_refcounts[ancestor] > 0 {
                        self.shared_reachable_surplus +=
                            Self::ancestor_surplus(problem, (weight, fee));
                    }
                }
            }
        }
        if is_addable {
            self.add_reachable(problem, index);
        }
    }

    pub(crate) fn ban(&mut self, problem: &SelectionProblem, index: usize) {
        self.remove_reachable(problem, index);
    }

    pub(crate) fn unban(&mut self, problem: &SelectionProblem, index: usize) {
        self.add_reachable(problem, index);
    }
}

/// A cached view over a [`CoinSelector`] that supports hypothetical updates.
///
/// [`add`](Self::add) and [`sub`](Self::sub) update this view's copy-on-write aggregates without
/// changing the underlying selector. Aggregate methods on `SelectionView` reflect those updates,
/// while [`selector`](Self::selector) and methods reached through [`Deref`] still reflect the base
/// selector's selected set. Branch and bound maintains the cache incrementally. For ad-hoc use,
/// [`CoinSelector::compute_view`] builds it from the current selection.
#[derive(Clone, Debug)]
pub struct SelectionView<'a> {
    selector: &'a CoinSelector<'a>,
    cache: Cow<'a, SelectionCache>,
    /// Position in the selector's candidate order before which every candidate is already decided
    /// — selected or banned. Zero is always correct; a larger value is a promise from whoever built
    /// the view, and only lets the undecided scan skip a prefix it would have filtered out anyway.
    ///
    /// Branch and bound decides candidates in order, so at depth `d` the first `d` positions are
    /// all decided. Without this, every query for "the best undecided candidate" walks those `d`
    /// entries first, which makes the per-node cost grow with the pool rather than with the answer.
    decided_before: usize,
}

impl<'a> Deref for SelectionView<'a> {
    type Target = CoinSelector<'a>;

    fn deref(&self) -> &Self::Target {
        self.selector
    }
}

impl<'a> SelectionView<'a> {
    pub(crate) fn with_cache(selector: &'a CoinSelector<'a>, cache: &'a SelectionCache) -> Self {
        Self::with_cache_from(selector, cache, 0)
    }

    /// [`with_cache`](Self::with_cache), promising that every candidate before position
    /// `decided_before` in the candidate order is already selected or banned.
    ///
    /// The promise is only ever an optimisation: it lets the undecided scan start past a prefix it
    /// would otherwise filter away one entry at a time. A debug assertion checks it.
    pub(crate) fn with_cache_from(
        selector: &'a CoinSelector<'a>,
        cache: &'a SelectionCache,
        decided_before: usize,
    ) -> Self {
        debug_assert!(
            selector
                .candidates()
                .take(decided_before)
                .all(|(index, _)| selector.is_selected(index)
                    || selector.banned().contains(index)),
            "an undecided candidate sits before `decided_before`, so skipping the prefix would hide it",
        );
        Self {
            selector,
            cache: Cow::Borrowed(cache),
            decided_before,
        }
    }

    pub(crate) fn from_selector(selector: &'a CoinSelector<'a>) -> Self {
        Self {
            selector,
            cache: Cow::Owned(SelectionCache::from_selector(selector)),
            decided_before: 0,
        }
    }

    /// The undecided candidates, in the selector's candidate order.
    ///
    /// Shadows [`CoinSelector::unselected`], which always starts at the front of the order. This
    /// one starts past the prefix the view was built knowing is already decided, so a caller that
    /// knows it does not pay to rediscover it. Views built outside branch and bound know nothing,
    /// and get the same answer by the same work.
    pub fn unselected(&self) -> impl DoubleEndedIterator<Item = (usize, Candidate)> + '_ {
        self.selector
            .candidates_from(self.decided_before)
            .filter(move |(index, _)| {
                !(self.selector.is_selected(*index) || self.selector.banned().contains(*index))
            })
    }

    /// Shadows [`CoinSelector::min_input_weight`] so it uses the view's undecided scan.
    ///
    /// `LowestFee`'s bound asks for this at every funded node once `Target::max_weight` is set, and
    /// the inherited version walks the whole candidate order to answer.
    pub fn min_input_weight(&self) -> Option<u64> {
        self.unselected().map(|(_, candidate)| candidate.weight).min()
    }

    /// The underlying selector, which is not changed by hypothetical view updates.
    pub fn selector(&self) -> &'a CoinSelector<'a> {
        self.selector
    }

    fn track_selected(&mut self) {
        if self.cache.selected.capacity() > 0 || self.selector.problem().is_empty() {
            return;
        }
        let mut selected = Bitset::with_capacity(self.selector.problem().len());
        for (index, _) in self.selector.selected() {
            selected.insert(index);
        }
        self.cache.to_mut().selected = selected;
    }

    /// Apply a hypothetical selection to this view's cached aggregates.
    ///
    /// Does nothing if the candidate was already selected in the view. Aggregate query methods on
    /// this view reflect the update; selection-set methods reached through [`Deref`] do not.
    pub fn add(&mut self, index: usize) {
        self.track_selected();
        let candidate = self.selector.candidate(index);
        self.cache.to_mut().add(
            self.selector.problem(),
            index,
            candidate,
            !self.selector.banned().contains(index),
        );
    }

    /// Apply a hypothetical deselection to this view's cached aggregates.
    ///
    /// Does nothing if the candidate was not selected in the view. Aggregate query methods on this
    /// view reflect the update; selection-set methods reached through [`Deref`] do not.
    pub fn sub(&mut self, index: usize) {
        self.track_selected();
        let candidate = self.selector.candidate(index);
        self.cache.to_mut().sub(
            self.selector.problem(),
            index,
            candidate,
            !self.selector.banned().contains(index),
        );
    }

    pub(crate) fn add_unchecked(&mut self, index: usize) {
        let candidate = self.selector.candidate(index);
        self.cache
            .to_mut()
            .add(self.selector.problem(), index, candidate, true);
    }

    pub(crate) fn sub_unchecked(&mut self, index: usize) {
        let candidate = self.selector.candidate(index);
        self.cache.to_mut().sub(
            self.selector.problem(),
            index,
            candidate,
            !self.selector.banned().contains(index),
        );
    }

    /// Absolute value sum of selected inputs.
    pub fn selected_value(&self) -> u64 {
        self.cache.value_sum
    }

    /// Input weight including the input-count varint and witness serialization overhead.
    pub fn input_weight(&self) -> u64 {
        self.cache.input_weight()
    }

    /// Current child transaction weight.
    pub fn weight(&self, target_outputs: TargetOutputs, drain_weights: DrainWeights) -> u64 {
        TX_FIXED_FIELD_WEIGHT
            + self.input_weight()
            + target_outputs.output_weight_with_drain(drain_weights)
    }

    /// Ancestor fee bump owed by the current selection, with shared ancestors charged once.
    pub fn ancestor_bump(&self) -> u64 {
        let weight = self.cache.private_weight + self.cache.shared_weight;
        let fee = self.cache.private_fee + self.cache.shared_fee;
        self.target()
            .fee
            .rate
            .implied_fee_wu(weight)
            .saturating_sub(fee)
    }

    /// Whether any undecided candidate weighs nothing but carries value.
    ///
    /// Such a candidate closes any fee deficit at zero added weight, so no relaxation may claim a
    /// deficit is unreachable while one remains.
    pub(crate) fn has_weightless_undecided_value(&self) -> bool {
        self.cache.undecided_weightless_value > 0
    }

    /// The greatest value-per-weight among undecided candidates, in `f64`.
    ///
    /// Candidates are ordered by *`f32`* value-per-weight, so the `f64` maximum can only lie inside
    /// the run sharing the first undecided candidate's `f32` key: two exact ratios can tie in `f32`
    /// and be ordered either way, and picking the lower one would overstate the weight a deficit
    /// needs. Scanning that run rather than the whole tail keeps the exact answer while making the
    /// query independent of the pool size, which matters because branch and bound asks it at every
    /// unfunded node.
    ///
    /// Zero-weight candidates sort first (their ratio is infinite) and are skipped here; use
    /// [`has_weightless_undecided_value`](Self::has_weightless_undecided_value) for those.
    ///
    /// Assumes the descending value-per-weight order that
    /// [`BnbMetric::requires_ordering_by_descending_value_pwu`](crate::BnbMetric::requires_ordering_by_descending_value_pwu)
    /// asks for; a debug assertion checks the result against a full scan.
    pub(crate) fn best_undecided_value_pwu(&self) -> f64 {
        let mut best = 0.0_f64;
        let mut key: Option<crate::float::Ordf32> = None;
        for (_, candidate) in self.unselected() {
            if candidate.weight == 0 {
                continue;
            }
            let candidate_key = crate::float::Ordf32(candidate.value_pwu());
            match key {
                None => key = Some(candidate_key),
                Some(first) if candidate_key != first => break,
                _ => {}
            }
            best = best.max(candidate.value as f64 / candidate.weight as f64);
        }
        debug_assert_eq!(
            best,
            self.unselected()
                .filter(|(_, c)| c.weight > 0)
                .map(|(_, c)| c.value as f64 / c.weight as f64)
                .fold(0.0_f64, f64::max),
            "candidates are not in descending value-per-weight order, so the tie-run scan is wrong"
        );
        best
    }

    /// Lower bound on the ancestor bump owed by this branch or any descendant.
    ///
    /// Branch and bound maintains both selected obligations and still-reachable surplus in the
    /// cache, so this query is constant-time.
    pub fn ancestor_bump_lower_bound(&self) -> u64 {
        if !self.selector.problem().has_ancestors() {
            return 0;
        }

        let spwu = self.target().fee.rate.spwu() as f64;
        let owed = (self.cache.private_weight + self.cache.shared_weight) as f64 * spwu
            - (self.cache.private_fee + self.cache.shared_fee) as f64;
        let bound =
            owed - self.cache.private_reachable_surplus - self.cache.shared_reachable_surplus;
        if bound <= 0.0 {
            0
        } else {
            (bound as u64).saturating_sub(self.cache.ancestor_fee_precision_slack)
        }
    }

    /// The most any descendant of this branch could still improve the feerate constraint.
    ///
    /// This is Bitcoin Core's `SelectCoinsBnB` lookahead (`curr_available_value`): the search keeps
    /// a running total of what the undecided candidates can contribute, and a node whose total
    /// still cannot close the gap has an empty subtree. Constant-time against the cache.
    ///
    /// Both terms are one-sided, so the result is an over-estimate and never prunes a branch that
    /// holds a solution. The undecided pair counts only candidates worth selecting, and the current
    /// ancestor bump is swapped for [`ancestor_bump_lower_bound`](Self::ancestor_bump_lower_bound),
    /// which holds for this branch and every descendant — so a subsidizing ancestor that a
    /// descendant might drag in is credited here rather than assumed away. The input-count varint
    /// and witness overhead those candidates would add is ignored for the same reason: leaving it
    /// out can only make this larger.
    pub(crate) fn best_reachable_rate_excess_wu(&self) -> i64 {
        self.rate_excess_wu(Drain::NONE) + self.ancestor_bump() as i64
            - self.ancestor_bump_lower_bound() as i64
            + self.cache.undecided_value as i64
            - self
                .target()
                .fee
                .rate
                .implied_fee_wu(self.cache.undecided_weight) as i64
    }

    fn implied_fee_from_feerate(&self, drain_weights: DrainWeights) -> u64 {
        self.target()
            .fee
            .rate
            .implied_fee(self.weight(self.target().outputs, drain_weights))
            + self.ancestor_bump()
    }

    fn implied_fee_from_feerate_wu(&self, drain_weights: DrainWeights) -> u64 {
        self.target()
            .fee
            .rate
            .implied_fee_wu(self.weight(self.target().outputs, drain_weights))
            + self.ancestor_bump()
    }

    /// Excess against all target fee constraints.
    pub fn excess(&self, drain: Drain) -> i64 {
        self.rate_excess(drain)
            .min(self.absolute_excess(drain))
            .min(self.replacement_excess(drain))
    }

    /// Excess against the target feerate, including ancestor bumping.
    pub fn rate_excess(&self, drain: Drain) -> i64 {
        self.selected_value() as i64
            - self.target().value() as i64
            - drain.value as i64
            - self.implied_fee_from_feerate(drain.weights) as i64
    }

    /// Weight-unit version of [`rate_excess`](Self::rate_excess).
    pub fn rate_excess_wu(&self, drain: Drain) -> i64 {
        self.selected_value() as i64
            - self.target().value() as i64
            - drain.value as i64
            - self.implied_fee_from_feerate_wu(drain.weights) as i64
    }

    /// Excess against the absolute fee target.
    pub fn absolute_excess(&self, drain: Drain) -> i64 {
        self.selected_value() as i64
            - self.target().value() as i64
            - drain.value as i64
            - self.target().fee.absolute as i64
    }

    /// Excess against replacement rule 4.
    pub fn replacement_excess(&self, drain: Drain) -> i64 {
        let fee = self.target().fee.replace.map_or(0, |replace| {
            replace.min_fee_to_do_replacement(self.weight(self.target().outputs, drain.weights))
        });
        self.selected_value() as i64
            - self.target().value() as i64
            - drain.value as i64
            - fee as i64
    }

    /// Weight-unit version of [`replacement_excess`](Self::replacement_excess).
    pub fn replacement_excess_wu(&self, drain: Drain) -> i64 {
        let fee = self.target().fee.replace.map_or(0, |replace| {
            replace.min_fee_to_do_replacement_wu(self.weight(self.target().outputs, drain.weights))
        });
        self.selected_value() as i64
            - self.target().value() as i64
            - drain.value as i64
            - fee as i64
    }

    /// Whether the value and fee target is met with `drain`.
    pub fn is_funded_with_drain(&self, drain: Drain) -> bool {
        self.excess(drain) >= 0
    }

    /// Whether the value and fee target is met without change.
    pub fn is_funded(&self) -> bool {
        self.is_funded_with_drain(Drain::NONE)
    }

    /// Whether the target appears reachable after adding every remaining candidate with positive
    /// standalone effective value.
    ///
    /// The current selection is checked first because transaction framing can make adding a
    /// standalone-positive candidate reduce actual excess. This remains a heuristic: framing and
    /// ancestors make marginal effective values selection-dependent.
    pub fn is_fundable(&self) -> bool {
        if self.is_funded() {
            return true;
        }
        let mut local = self.clone();
        local.track_selected();
        for (index, candidate) in self.selector.candidates() {
            if !local.cache.selected.contains(index)
                && !self.selector.banned().contains(index)
                && candidate.effective_value(self.target().fee.rate) > 0.0
            {
                local.add(index);
            }
        }
        local.is_funded()
    }

    /// Additional value needed to meet the target.
    pub fn missing(&self) -> u64 {
        let excess = self.excess(Drain::NONE);
        if excess < 0 {
            excess.unsigned_abs()
        } else {
            0
        }
    }

    /// Whether this child transaction fits its target weight cap.
    pub fn is_within_max_weight(&self, drain_weights: DrainWeights) -> bool {
        self.target().max_weight.map_or(true, |max_weight| {
            self.weight(self.target().outputs, drain_weights) <= max_weight
        })
    }

    /// Actual child fee for the supplied output values.
    pub fn fee(&self, target_value: u64, drain_value: u64) -> i64 {
        self.selected_value() as i64 - target_value as i64 - drain_value as i64
    }

    /// Fee required by all target fee constraints for this selection.
    pub fn implied_fee(&self, drain_weights: DrainWeights) -> u64 {
        let mut fee = self
            .implied_fee_from_feerate(drain_weights)
            .max(self.target().fee.absolute);
        if let Some(replace) = self.target().fee.replace {
            fee = fee.max(
                replace
                    .min_fee_to_do_replacement(self.weight(self.target().outputs, drain_weights)),
            );
        }
        fee
    }

    /// Child transaction feerate implied by the selection and outputs.
    pub fn implied_feerate(&self, target_outputs: TargetOutputs, drain: Drain) -> Option<FeeRate> {
        let fee =
            self.selected_value() as i64 - target_outputs.value_sum as i64 - drain.value as i64;
        let weight = self.weight(target_outputs, drain.weights);
        if fee < 0 || weight == 0 {
            return None;
        }
        Some(FeeRate::from_sat_per_wu(fee as f32 / weight as f32))
    }

    /// A lower bound on the child fee for this branch and every descendant.
    pub(crate) fn fee_floor(&self) -> u64 {
        let weight = self.weight(self.target().outputs, DrainWeights::NONE);
        let rate_floor = self
            .target()
            .fee
            .rate
            .implied_fee_wu(weight)
            .min(self.target().fee.rate.implied_fee(weight));
        let mut floor =
            (rate_floor + self.ancestor_bump_lower_bound()).max(self.target().fee.absolute);
        if let Some(replace) = self.target().fee.replace {
            floor = floor.max(
                replace
                    .min_fee_to_do_replacement_wu(weight)
                    .min(replace.min_fee_to_do_replacement(weight)),
            );
        }
        floor
    }

    /// The value of a policy-controlled change output, if one should be added.
    pub fn drain_value(&self, change_policy: ChangePolicy) -> Option<u64> {
        let excess = self.excess(Drain {
            weights: change_policy.drain_weights,
            value: 0,
        });
        if excess > change_policy.min_value as i64 {
            Some(excess as u64)
        } else {
            None
        }
    }

    /// A policy-controlled change output.
    pub fn drain(&self, change_policy: ChangePolicy) -> Drain {
        self.drain_value(change_policy)
            .map_or(Drain::NONE, |value| Drain {
                weights: change_policy.drain_weights,
                value,
            })
    }

    /// The current selection's effective value at `feerate`.
    pub fn effective_value(&self, feerate: FeeRate) -> i64 {
        self.selected_value() as i64 - (self.input_weight() as f32 * feerate.spwu()).ceil() as i64
    }

    /// Waste created by this selection.
    pub fn waste(&self, long_term_feerate: FeeRate, drain: Drain, excess_discount: f32) -> f32 {
        debug_assert!((0.0..=1.0).contains(&excess_discount));
        let mut waste =
            self.input_weight() as f32 * (self.target().fee.rate.spwu() - long_term_feerate.spwu());
        if drain.is_none() {
            waste += self.excess(drain).max(0) as f32 * excess_discount.clamp(0.0, 1.0);
        } else {
            waste += drain.weights.waste(
                self.target().fee.rate,
                long_term_feerate,
                self.target().outputs.n_outputs,
            );
        }
        waste
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AncestorToBump, Input, Target, TargetFee, TargetOutputs};

    fn target() -> Target {
        Target {
            fee: TargetFee::ZERO,
            outputs: TargetOutputs {
                value_sum: 0,
                weight_sum: 0,
                n_outputs: 0,
            },
            max_weight: None,
        }
    }

    /// Straightforward iteration over the selected set, kept here as an independent reference for
    /// the running aggregates the cache maintains.
    fn expected_input_weight(selector: &CoinSelector<'_>) -> u64 {
        let is_segwit_tx = selector.selected().any(|(_, c)| c.segwit_count > 0);
        let input_count: usize = selector
            .selected()
            .map(|(_, c)| c.segwit_count + c.legacy_count)
            .sum();
        let selected_weight: u64 = selector
            .selected()
            .map(|(_, c)| c.weight + is_segwit_tx as u64 * c.legacy_count as u64)
            .sum();
        varint_size(input_count) * 4 + selected_weight + is_segwit_tx as u64 * 2
    }

    #[test]
    fn shared_ancestor_is_cached_once_and_removed_on_last_sub() {
        let ancestors = [AncestorToBump {
            txid: 1,
            weight: 400,
            fee: 0,
            parents: alloc::vec![],
        }];
        let inputs = [
            Input {
                value: 1_000,
                weight: 100,
                is_segwit: true,
                residing_txid: 1,
            },
            Input {
                value: 2_000,
                weight: 100,
                is_segwit: false,
                residing_txid: 1,
            },
        ];
        let problem = SelectionProblem::new(target(), inputs, ancestors);
        let selector = problem.selector();
        let mut view = selector.compute_view();
        view.add(0);
        let once = view.ancestor_bump();
        view.add(1);
        assert_eq!(view.ancestor_bump(), once);
        view.sub(0);
        assert_eq!(view.ancestor_bump(), once);
        view.sub(1);
        assert_eq!(view.ancestor_bump(), 0);
    }

    #[test]
    fn hypothetical_banned_candidate_does_not_change_reachability() {
        let ancestors = [AncestorToBump {
            txid: 1,
            weight: 400,
            fee: 1_000,
            parents: alloc::vec![],
        }];
        let inputs = [
            Input {
                value: 1_000,
                weight: 100,
                is_segwit: true,
                residing_txid: 1,
            },
            Input {
                value: 2_000,
                weight: 100,
                is_segwit: true,
                residing_txid: 1,
            },
        ];
        let problem = SelectionProblem::new(target(), inputs, ancestors);
        let mut selector = problem.selector();
        selector.ban(0);
        selector.ban(1);
        let mut view = selector.compute_view();
        let bound = view.ancestor_bump_lower_bound();

        view.add(0);
        view.sub(0);
        assert_eq!(view.ancestor_bump_lower_bound(), bound);
    }

    #[test]
    fn cache_matches_iteration_for_every_mixed_selection() {
        let candidates = [
            Candidate::new_segwit(1_000, 100),
            Candidate::new_legacy(2_000, 200),
            Candidate {
                value: 3_000,
                weight: 350,
                segwit_count: 2,
                legacy_count: 3,
            },
        ];
        let problem = SelectionProblem::new_no_ancestors(target(), candidates);
        for mask in 0..1 << candidates.len() {
            let mut selector = problem.selector();
            for index in 0..candidates.len() {
                if mask & (1 << index) != 0 {
                    selector.select(index);
                }
            }
            let view = selector.compute_view();
            assert_eq!(
                view.selected_value(),
                selector.selected().map(|(_, c)| c.value).sum::<u64>()
            );
            assert_eq!(view.input_weight(), expected_input_weight(&selector));
        }
    }

    #[test]
    fn hypothetical_updates_have_set_semantics_without_ancestors() {
        let candidates = [
            Candidate::new_segwit(1_000, 100),
            Candidate::new_legacy(2_000, 200),
        ];
        let problem = SelectionProblem::new_no_ancestors(target(), candidates);
        let mut selector = problem.selector();
        selector.select(0);
        let mut view = selector.compute_view();

        let initial_value = view.selected_value();
        let initial_weight = view.input_weight();
        view.add(0);
        assert_eq!(view.selected_value(), initial_value);
        assert_eq!(view.input_weight(), initial_weight);

        view.sub(1);
        assert_eq!(view.selected_value(), initial_value);
        assert_eq!(view.input_weight(), initial_weight);

        view.sub(0);
        view.sub(0);
        assert_eq!(view.selected_value(), 0);

        view.add(1);
        view.add(1);
        assert_eq!(view.selected_value(), candidates[1].value);
        assert_eq!(view.input_weight(), {
            let mut expected = problem.selector();
            expected.select(1);
            expected_input_weight(&expected)
        });
    }

    #[test]
    fn is_fundable_uses_hypothetical_selection_state() {
        let mut target = target();
        target.outputs.value_sum = 2_000;
        let candidates = [
            Candidate::new_segwit(1_000, 100),
            Candidate::new_segwit(1_000, 100),
        ];
        let problem = SelectionProblem::new_no_ancestors(target, candidates);
        let mut selector = problem.selector();
        selector.select(0);
        let mut view = selector.compute_view();

        view.sub(0);
        assert!(view.is_fundable());
        view.add(1);
        assert!(view.is_fundable());
    }

    #[test]
    fn is_fundable_never_rejects_an_already_funded_mixed_selection() {
        let mut target = target();
        target.outputs.value_sum = 1_000;
        target.fee = TargetFee::from_feerate(FeeRate::from_sat_per_vb(4.0));
        let candidates = [
            Candidate {
                value: 1_201,
                weight: 158,
                segwit_count: 1,
                legacy_count: 0,
            },
            Candidate {
                value: 165,
                weight: 164,
                segwit_count: 0,
                legacy_count: 3,
            },
        ];
        let problem = SelectionProblem::new_no_ancestors(target, candidates);
        let mut selector = problem.selector();
        selector.select(0);
        assert!(selector.compute_view().is_funded());

        let mut all = selector.clone();
        all.select(1);
        assert!(!all.compute_view().is_funded());
        assert!(selector.compute_view().is_fundable());
    }

    #[test]
    fn hypothetical_ancestor_queries_match_selector_mutations() {
        let mut target = target();
        target.fee = TargetFee::from_feerate(FeeRate::from_sat_per_vb(4.0));
        let ancestors = [
            AncestorToBump {
                txid: 1,
                weight: 400,
                fee: 0,
                parents: alloc::vec![],
            },
            AncestorToBump {
                txid: 2,
                weight: 400,
                fee: 800,
                parents: alloc::vec![],
            },
        ];
        let groups = [
            alloc::vec![
                Input {
                    value: 1_000,
                    weight: 100,
                    is_segwit: true,
                    residing_txid: 1,
                },
                Input {
                    value: 1_000,
                    weight: 100,
                    is_segwit: false,
                    residing_txid: 2,
                },
            ],
            alloc::vec![Input {
                value: 2_000,
                weight: 100,
                is_segwit: true,
                residing_txid: 1,
            }],
        ];
        let problem = SelectionProblem::new(target, groups, ancestors);
        let base = problem.selector();
        let mut actual = base.clone();
        let mut hypothetical = base.compute_view();

        for index in 0..2 {
            actual.select(index);
            hypothetical.add(index);
            assert_eq!(
                hypothetical.ancestor_bump(),
                actual.compute_view().ancestor_bump()
            );
            assert_eq!(
                hypothetical.ancestor_bump_lower_bound(),
                actual.compute_view().ancestor_bump_lower_bound()
            );
            assert_eq!(
                hypothetical.excess(Drain::NONE),
                actual.compute_view().excess(Drain::NONE)
            );
        }

        actual.deselect(0);
        hypothetical.sub(0);
        assert_eq!(
            hypothetical.ancestor_bump(),
            actual.compute_view().ancestor_bump()
        );
        assert_eq!(
            hypothetical.ancestor_bump_lower_bound(),
            actual.compute_view().ancestor_bump_lower_bound()
        );

        actual.ban(0);
        hypothetical
            .cache
            .to_mut()
            .ban(hypothetical.selector.problem(), 0);
        assert_eq!(
            hypothetical.ancestor_bump_lower_bound(),
            actual.compute_view().ancestor_bump_lower_bound()
        );
    }
}
