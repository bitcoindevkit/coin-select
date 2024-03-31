use super::*;
#[allow(unused)] // some bug in <= 1.48.0 sees this as unused when it isn't
use crate::float::FloatExt;
use crate::{bnb::BnbMetric, float::Ordf32, ChangePolicy, FeeRate, Target};
use alloc::{borrow::Cow, collections::BTreeSet, vec::Vec};

/// [`CoinSelector`] selects/deselects coins from a set of canididate coins.
///
/// You can manually select coins using methods like [`select`], or automatically with methods such
/// as [`bnb_solutions`].
///
/// [`select`]: CoinSelector::select
/// [`bnb_solutions`]: CoinSelector::bnb_solutions
#[derive(Debug, Clone)]
pub struct CoinSelector<'a> {
    candidates: &'a [Candidate],
    selected: Cow<'a, BTreeSet<usize>>,
    banned: Cow<'a, BTreeSet<usize>>,
    candidate_order: Cow<'a, Vec<usize>>,
}

impl<'a> CoinSelector<'a> {
    /// Creates a new coin selector from some candidate inputs and a `base_weight`.
    ///
    /// The `base_weight` is the weight of the transaction without any inputs and without a change
    /// output.
    ///
    /// The `CoinSelector` does not keep track of the final transaction's output count. The caller
    /// is responsible for including the potential output-count varint weight change in the
    /// corresponding [`DrainWeights`].
    ///
    /// Note that methods in `CoinSelector` will refer to inputs by the index in the `candidates`
    /// slice you pass in.
    pub fn new(candidates: &'a [Candidate]) -> Self {
        Self {
            candidates,
            selected: Cow::Owned(Default::default()),
            banned: Cow::Owned(Default::default()),
            candidate_order: Cow::Owned((0..candidates.len()).collect()),
        }
    }

    /// Iterate over all the candidates in their currently sorted order. Each item has the original
    /// index with the candidate.
    pub fn candidates(
        &self,
    ) -> impl DoubleEndedIterator<Item = (usize, Candidate)> + ExactSizeIterator + '_ {
        self.candidate_order
            .iter()
            .map(move |i| (*i, self.candidates[*i]))
    }

    /// Get the candidate at `index`. `index` refers to its position in the original `candidates`
    /// slice passed into [`CoinSelector::new`].
    pub fn candidate(&self, index: usize) -> Candidate {
        self.candidates[index]
    }

    /// Deselect a candidate at `index`. `index` refers to its position in the original `candidates`
    /// slice passed into [`CoinSelector::new`].
    pub fn deselect(&mut self, index: usize) -> bool {
        self.selected.to_mut().remove(&index)
    }

    /// Convienince method to pick elements of a slice by the indexes that are currently selected.
    /// Obviously the slice must represent the inputs ordered in the same way as when they were
    /// passed to `Candidates::new`.
    pub fn apply_selection<T>(&self, candidates: &'a [T]) -> impl Iterator<Item = &'a T> + '_ {
        self.selected.iter().map(move |i| &candidates[*i])
    }

    /// Select the input at `index`. `index` refers to its position in the original `candidates`
    /// slice passed into [`CoinSelector::new`].
    pub fn select(&mut self, index: usize) -> bool {
        assert!(index < self.candidates.len());
        self.selected.to_mut().insert(index)
    }

    /// Select the next unselected candidate in the sorted order fo the candidates.
    pub fn select_next(&mut self) -> bool {
        let next = self.unselected_indices().next();
        if let Some(next) = next {
            self.select(next);
            true
        } else {
            false
        }
    }

    /// Ban an input from being selected. Banning the input means it won't show up in [`unselected`]
    /// or [`unselected_indices`]. Note it can still be manually selected.
    ///
    /// `index` refers to its position in the original `candidates` slice passed into [`CoinSelector::new`].
    ///
    /// [`unselected`]: Self::unselected
    /// [`unselected_indices`]: Self::unselected_indices
    pub fn ban(&mut self, index: usize) {
        self.banned.to_mut().insert(index);
    }

    /// Gets the list of inputs that have been banned by [`ban`].
    ///
    /// [`ban`]: Self::ban
    pub fn banned(&self) -> &BTreeSet<usize> {
        &self.banned
    }

    /// Is the input at `index` selected. `index` refers to its position in the original
    /// `candidates` slice passed into [`CoinSelector::new`].
    pub fn is_selected(&self, index: usize) -> bool {
        self.selected.contains(&index)
    }

    /// Is meeting this `target` possible with the current selection with this `drain` (i.e. change output).
    /// Note this will respect [`ban`]ned candidates.
    ///
    /// This simply selects all effective inputs at the target's feerate and checks whether we have
    /// enough value.
    ///
    /// [`ban`]: Self::ban
    pub fn is_selection_possible(&self, target: Target) -> bool {
        let mut test = self.clone();
        test.select_all_effective(target.fee.rate);
        test.is_target_met(target)
    }

    /// Returns true if no candidates have been selected.
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// The weight of the inputs including the witness header and the varint for the number of
    /// inputs.
    pub fn input_weight(&self) -> u32 {
        let is_segwit_tx = self.selected().any(|(_, wv)| wv.is_segwit);
        let witness_header_extra_weight = is_segwit_tx as u32 * 2;

        let input_count = self.selected().map(|(_, wv)| wv.input_count).sum::<usize>();
        let input_varint_weight = varint_size(input_count) * 4;

        let selected_weight: u32 = self
            .selected()
            .map(|(_, candidate)| {
                let mut weight = candidate.weight;
                if is_segwit_tx && !candidate.is_segwit {
                    // non-segwit candidates do not have the witness length field included in their
                    // weight field so we need to add 1 here if it's in a segwit tx.
                    weight += 1;
                }
                weight
            })
            .sum();

        input_varint_weight + selected_weight + witness_header_extra_weight
    }

    /// Absolute value sum of all selected inputs.
    pub fn selected_value(&self) -> u64 {
        self.selected
            .iter()
            .map(|&index| self.candidates[index].value)
            .sum()
    }

    /// Current weight of transaction implied by the selection.
    ///
    /// If you don't have any drain outputs (only target outputs) just set drain_weights to
    /// [`DrainWeights::NONE`].
    pub fn weight(&self, target_ouputs: TargetOutputs, drain_weight: DrainWeights) -> u32 {
        TX_FIXED_FIELD_WEIGHT
            + self.input_weight()
            + target_ouputs.output_weight_with_drain(drain_weight)
    }

    /// How much the current selection overshoots the value needed to achieve `target`.
    ///
    /// In order for the resulting transaction to be valid this must be 0 or above. If it's above 0
    /// this means the transaction will overpay for what it needs to reach `target`.
    pub fn excess(&self, target: Target, drain: Drain) -> i64 {
        self.rate_excess(target, drain)
            .min(self.replacement_excess(target, drain))
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

    /// How much the current selection overshoots the value need to satisfy `target.fee.rate` and
    /// `target.value` (while ignoring `target.min_fee`).
    pub fn rate_excess(&self, target: Target, drain: Drain) -> i64 {
        self.selected_value() as i64
            - target.value() as i64
            - drain.value as i64
            - self.implied_fee_from_feerate(target, drain.weights) as i64
    }

    /// How much the current selection overshoots the value needed to satisfy RBF's rule 4.
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

    /// The feerate the transaction would have if we were to use this selection of inputs to achieve
    /// the `target`'s value and weight. It is essentially telling you what target feerate you currently have.
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

    /// The fee the current selection and `drain_weight` should pay to satisfy `target_fee`.
    ///
    /// This compares the fee calculated from the target feerate with the fee calculated from the
    /// [`Replace`] constraints and returns the larger of the two.
    ///
    /// `drain_weight` can be 0 to indicate no draining output.
    pub fn implied_fee(&self, target: Target, drain_weights: DrainWeights) -> u64 {
        let mut implied_fee = self.implied_fee_from_feerate(target, drain_weights);

        if let Some(replace) = target.fee.replace {
            implied_fee = Ord::max(
                implied_fee,
                replace.min_fee_to_do_replacement(self.weight(target.outputs, drain_weights)),
            );
        }

        implied_fee
    }

    fn implied_fee_from_feerate(&self, target: Target, drain_weights: DrainWeights) -> u64 {
        (self.weight(target.outputs, drain_weights) as f32 * target.fee.rate.spwu()).ceil() as u64
    }

    /// The actual fee the selection would pay if it was used in a transaction that had
    /// `target_value` value for outputs and change output of `drain_value`.
    ///
    /// This can be negative when the selection is invalid (outputs are greater than inputs).
    pub fn fee(&self, target_value: u64, drain_value: u64) -> i64 {
        self.selected_value() as i64 - target_value as i64 - drain_value as i64
    }

    /// The value of the current selected inputs minus the fee needed to pay for the selected inputs
    pub fn effective_value(&self, feerate: FeeRate) -> i64 {
        self.selected_value() as i64 - (self.input_weight() as f32 * feerate.spwu()).ceil() as i64
    }

    // /// Waste sum of all selected inputs.
    fn input_waste(&self, feerate: FeeRate, long_term_feerate: FeeRate) -> f32 {
        self.input_weight() as f32 * (feerate.spwu() - long_term_feerate.spwu())
    }

    /// Sorts the candidates by the comparision function.
    ///
    /// The comparision function takes the candidates's index and the [`Candidate`].
    ///
    /// Note this function does not change the index of the candidates after sorting, just the order
    /// in which they will be returned when interating over them in [`candidates`] and [`unselected`].
    ///
    /// [`candidates`]: CoinSelector::candidates
    /// [`unselected`]: CoinSelector::unselected
    pub fn sort_candidates_by<F>(&mut self, mut cmp: F)
    where
        F: FnMut((usize, Candidate), (usize, Candidate)) -> core::cmp::Ordering,
    {
        let order = self.candidate_order.to_mut();
        let candidates = &self.candidates;
        order.sort_by(|a, b| cmp((*a, candidates[*a]), (*b, candidates[*b])))
    }

    /// Sorts the candidates by the key function.
    ///
    /// The key function takes the candidates's index and the [`Candidate`].
    ///
    /// Note this function does not change the index of the candidates after sorting, just the order
    /// in which they will be returned when interating over them in [`candidates`] and [`unselected`].
    ///
    /// [`candidates`]: CoinSelector::candidates
    /// [`unselected`]: CoinSelector::unselected
    pub fn sort_candidates_by_key<F, K>(&mut self, mut key_fn: F)
    where
        F: FnMut((usize, Candidate)) -> K,
        K: Ord,
    {
        self.sort_candidates_by(|a, b| key_fn(a).cmp(&key_fn(b)))
    }

    /// Sorts the candidates by descending value per weight unit, tie-breaking with value.
    pub fn sort_candidates_by_descending_value_pwu(&mut self) {
        self.sort_candidates_by_key(|(_, wv)| {
            core::cmp::Reverse((Ordf32(wv.value_pwu()), wv.value))
        });
    }

    /// The waste created by the current selection as measured by the [waste metric].
    ///
    /// You can pass in an `excess_discount` which must be between `0.0..1.0`. Passing in `1.0` gives you no discount
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
        let mut waste = self.input_waste(target.fee.rate, long_term_feerate);

        if drain.is_none() {
            // We don't allow negative excess waste since negative excess just means you haven't
            // satisified target yet in which case you probably shouldn't be calling this function.
            let mut excess_waste = self.excess(target, drain).max(0) as f32;
            // we allow caller to discount this waste depending on how wasteful excess actually is
            // to them.
            excess_waste *= excess_discount.max(0.0).min(1.0);
            waste += excess_waste;
        } else {
            waste +=
                drain
                    .weights
                    .waste(target.fee.rate, long_term_feerate, target.outputs.n_outputs);
        }

        waste
    }

    /// The selected candidates with their index.
    pub fn selected(
        &self,
    ) -> impl ExactSizeIterator<Item = (usize, Candidate)> + DoubleEndedIterator + '_ {
        self.selected
            .iter()
            .map(move |&index| (index, self.candidates[index]))
    }

    /// The unselected candidates with their index.
    ///
    /// The candidates are returned in sorted order. See [`sort_candidates_by`].
    ///
    /// [`sort_candidates_by`]: Self::sort_candidates_by
    pub fn unselected(&self) -> impl DoubleEndedIterator<Item = (usize, Candidate)> + '_ {
        self.unselected_indices()
            .map(move |i| (i, self.candidates[i]))
    }

    /// The indices of the selelcted candidates.
    pub fn selected_indices(&self) -> &BTreeSet<usize> {
        &self.selected
    }

    /// The indices of the unselected candidates.
    ///
    /// This excludes candidates that have been selected or [`banned`].
    ///
    /// [`banned`]: Self::ban
    pub fn unselected_indices(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        self.candidate_order
            .iter()
            .filter(move |index| !(self.selected.contains(index) || self.banned.contains(index)))
            .copied()
    }

    /// Whether there are any unselected candidates left.
    pub fn is_exhausted(&self) -> bool {
        self.unselected_indices().next().is_none()
    }

    /// Whether the constraints of `Target` have been met if we include a specific `drain` ouput.
    ///
    /// Note if [`is_target_met`] is true and the `drain` is produced from the [`drain`] method then
    /// this method will also always be true.
    ///
    /// [`is_target_met`]: Self::is_target_met
    /// [`drain`]: Self::drain
    pub fn is_target_met_with_drain(&self, target: Target, drain: Drain) -> bool {
        self.excess(target, drain) >= 0
    }

    /// Whether the constraints of `Target` have been met.
    pub fn is_target_met(&self, target: Target) -> bool {
        self.is_target_met_with_drain(target, Drain::NONE)
    }

    /// Select all unselected candidates
    pub fn select_all(&mut self) {
        loop {
            if !self.select_next() {
                break;
            }
        }
    }

    /// The value of the change output should have to drain the excess value while maintaining the
    /// constraints of `target` and respecting `change_policy`.
    ///
    /// If not change output should be added according to policy then it will return `None`.
    pub fn drain_value(&self, target: Target, change_policy: ChangePolicy) -> Option<u64> {
        let excess = self.excess(
            target,
            Drain {
                weights: change_policy.drain_weights,
                value: 0,
            },
        );
        if excess > change_policy.min_value as i64 {
            debug_assert_eq!(
                self.is_target_met(target),
                self.is_target_met_with_drain(
                    target,
                    Drain {
                        weights: change_policy.drain_weights,
                        value: excess as u64
                    }
                ),
                "if the target is met without a drain it must be met after adding the drain"
            );
            Some(excess as u64)
        } else {
            None
        }
    }

    /// Figures out whether the current selection should have a change output given the
    /// `change_policy`. If it should not, then it will return [`Drain::NONE`]. The value of the
    /// `Drain` will be the same as [`drain_value`].
    ///
    /// If [`is_target_met`] returns true for this selection then [`is_target_met_with_drain`] will
    /// also be true if you pass in the drain returned from this method.
    ///
    /// [`drain_value`]: Self::drain_value
    /// [`is_target_met_with_drain`]: Self::is_target_met_with_drain
    /// [`is_target_met`]: Self::is_target_met
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

    /// Select all candidates with an *effective value* greater than 0 at the provided `feerate`.
    ///
    /// A candidate if effective if it provides more value than it takes to pay for at `feerate`.
    pub fn select_all_effective(&mut self, feerate: FeeRate) {
        for cand_index in self.candidate_order.iter() {
            if self.selected.contains(cand_index)
                || self.banned.contains(cand_index)
                || self.candidates[*cand_index].effective_value(feerate) <= 0.0
            {
                continue;
            }
            self.selected.to_mut().insert(*cand_index);
        }
    }

    /// Select candidates until `target` has been met assuming the `drain` output is attached.
    ///
    /// Returns an error if the target was unable to be met.
    pub fn select_until_target_met(&mut self, target: Target) -> Result<(), InsufficientFunds> {
        self.select_until(|cs| cs.is_target_met(target))
            .ok_or_else(|| InsufficientFunds {
                missing: self.excess(target, Drain::NONE).unsigned_abs(),
            })
    }

    /// Select candidates until some predicate has been satisfied.
    #[must_use]
    pub fn select_until(
        &mut self,
        mut predicate: impl FnMut(&CoinSelector<'a>) -> bool,
    ) -> Option<()> {
        loop {
            if predicate(&*self) {
                break Some(());
            }

            if !self.select_next() {
                break None;
            }
        }
    }

    /// Return an iterator that can be used to select candidates.
    pub fn select_iter(self) -> SelectIter<'a> {
        SelectIter { cs: self.clone() }
    }

    /// Iterates over rounds of branch and bound to minimize the score of the provided
    /// [`BnbMetric`].
    ///
    /// Not every iteration will return a solution. If a solution is found, we return the selection
    /// and score. Each subsequent solution of the iterator guarantees a higher score than the last.
    ///
    /// Most of the time, you would want to use [`CoinSelector::run_bnb`] instead.
    pub fn bnb_solutions<M: BnbMetric>(
        &self,
        metric: M,
    ) -> impl Iterator<Item = Option<(CoinSelector<'a>, Ordf32)>> {
        crate::bnb::BnbIter::new(self.clone(), metric)
    }

    /// Run branch and bound to minimize the score of the provided [`BnbMetric`].
    ///
    /// The method keeps trying until no better solution can be found, or we reach `max_rounds`. If
    /// a solution is found, the score is returned. Otherwise, we error with [`NoBnbSolution`].
    ///
    /// Use [`CoinSelector::bnb_solutions`] to access the branch and bound iterator directly.
    pub fn run_bnb<M: BnbMetric>(
        &mut self,
        metric: M,
        max_rounds: usize,
    ) -> Result<Ordf32, NoBnbSolution> {
        let mut rounds = 0_usize;
        let (selector, score) = self
            .bnb_solutions(metric)
            .inspect(|_| rounds += 1)
            .take(max_rounds)
            .flatten()
            .last()
            .ok_or(NoBnbSolution { max_rounds, rounds })?;
        *self = selector;
        Ok(score)
    }
}

impl<'a> core::fmt::Display for CoinSelector<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[")?;
        let mut candidates = self.candidates().peekable();

        while let Some((i, _)) = candidates.next() {
            write!(f, "{}", i)?;
            if self.is_selected(i) {
                write!(f, "✔")?;
            } else if self.banned().contains(&i) {
                write!(f, "✘")?
            } else {
                write!(f, "☐")?;
            }

            if candidates.peek().is_some() {
                write!(f, ", ")?;
            }
        }

        write!(f, "]")
    }
}

/// The `SelectIter` allows you to select candidates by calling [`Iterator::next`].
///
/// The [`Iterator::Item`] is a tuple of `(selector, last_selected_index, last_selected_candidate)`.
pub struct SelectIter<'a> {
    cs: CoinSelector<'a>,
}

impl<'a> Iterator for SelectIter<'a> {
    type Item = (CoinSelector<'a>, usize, Candidate);

    fn next(&mut self) -> Option<Self::Item> {
        let (index, wv) = self.cs.unselected().next()?;
        self.cs.select(index);
        Some((self.cs.clone(), index, wv))
    }
}

impl<'a> DoubleEndedIterator for SelectIter<'a> {
    fn next_back(&mut self) -> Option<Self::Item> {
        let (index, wv) = self.cs.unselected().next_back()?;
        self.cs.select(index);
        Some((self.cs.clone(), index, wv))
    }
}

/// Error type that occurs when the target amount cannot be met.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub struct InsufficientFunds {
    /// The missing amount in satoshis.
    pub missing: u64,
}

impl core::fmt::Display for InsufficientFunds {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Insufficient funds. Missing {} sats.", self.missing)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for InsufficientFunds {}

/// Error type for when a solution cannot be found by branch-and-bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoBnbSolution {
    /// Maximum rounds set by the caller.
    pub max_rounds: usize,
    /// Number of branch-and-bound rounds performed.
    pub rounds: usize,
}

impl core::fmt::Display for NoBnbSolution {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "No bnb solution found after {} rounds (max rounds is {}).",
            self.rounds, self.max_rounds
        )
    }
}

#[cfg(feature = "std")]
impl std::error::Error for NoBnbSolution {}

/// A `Candidate` represents an input candidate for [`CoinSelector`].
///
/// This can either be a single UTXO, or a group of UTXOs that should be spent together.
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    /// Total value of the UTXO(s) that this [`Candidate`] represents.
    pub value: u64,
    /// Total weight of including this/these UTXO(s).
    /// `txin` fields: `prevout`, `nSequence`, `scriptSigLen`, `scriptSig`, `scriptWitnessLen`,
    /// `scriptWitness` should all be included.
    pub weight: u32,
    /// Total number of inputs; so we can calculate extra `varint` weight due to `vin` len changes.
    pub input_count: usize,
    /// Whether this [`Candidate`] contains at least one segwit spend.
    pub is_segwit: bool,
}

impl Candidate {
    /// Create a [`Candidate`] input that spends a single taproot keyspend output.
    pub fn new_tr_keyspend(value: u64) -> Self {
        let weight = TXIN_BASE_WEIGHT + TR_KEYSPEND_SATISFACTION_WEIGHT;
        Self::new(value, weight, true)
    }

    /// Create a new [`Candidate`] that represents a single input.
    ///
    /// `satisfaction_weight` is the weight of `scriptSigLen + scriptSig + scriptWitnessLen +
    /// scriptWitness`.
    pub fn new(value: u64, satisfaction_weight: u32, is_segwit: bool) -> Candidate {
        let weight = TXIN_BASE_WEIGHT + satisfaction_weight;
        Candidate {
            value,
            weight,
            input_count: 1,
            is_segwit,
        }
    }

    /// Effective value of this input candidate: `actual_value - input_weight * feerate (sats/wu)`.
    pub fn effective_value(&self, feerate: FeeRate) -> f32 {
        self.value as f32 - (self.weight as f32 * feerate.spwu())
    }

    /// Value per weight unit
    pub fn value_pwu(&self) -> f32 {
        self.value as f32 / self.weight as f32
    }

    /// The amount of *effective value* you receive per weight unit from adding this candidate as an
    /// input.
    pub fn effective_value_pwu(&self, feerate: FeeRate) -> f32 {
        self.value_pwu() - feerate.spwu()
    }

    /// The (minimum) fee you'd have to pay to add this input to a transaction as implied by the
    /// `feerate`.
    pub fn implied_fee(&self, feerate: FeeRate) -> f32 {
        self.weight as f32 * feerate.spwu()
    }

    /// The amount of fee you have to pay per satoshi of value you add from this input.
    ///
    /// The value is always positive but values below 1.0 mean the input has negative [*effective
    /// value*](Self::effective_value) at this `feerate`.
    pub fn fee_per_value(&self, feerate: FeeRate) -> f32 {
        self.implied_fee(feerate) / self.value as f32
    }
}
