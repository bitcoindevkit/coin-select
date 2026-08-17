use super::*;
#[allow(unused)] // some bug in <= 1.48.0 sees this as unused when it isn't
use crate::float::FloatExt;
use crate::{bitset::Bitset, bnb::BnbMetric, float::Ordf32, FeeRate, SelectionProblem, Target};
use alloc::{sync::Arc, vec::Vec};

/// The minimum change amount Bitcoin Core's `SelectCoinsSRD` targets; a sensible default for the
/// `change_lower` argument of [`CoinSelector::select_srd`].
pub const CHANGE_LOWER: u64 = 50_000;

/// [`CoinSelector`] selects or deselects coins from a set of candidate coins.
///
/// You can manually select coins using methods like [`select`], or automatically with methods such
/// as [`bnb_solutions`].
///
/// [`select`]: CoinSelector::select
/// [`bnb_solutions`]: CoinSelector::bnb_solutions
#[derive(Debug, Clone)]
pub struct CoinSelector<'a> {
    problem: &'a SelectionProblem,
    selected: Bitset,
    banned: Bitset,
    candidate_order: Arc<Vec<usize>>,
}

impl<'a> CoinSelector<'a> {
    /// Creates a new coin selector for `problem`.
    ///
    /// The [`SelectionProblem`] is fixed for the life of the selector: target, candidates, and any
    /// ancestor-bump data. Methods refer to candidates by index into
    /// [`SelectionProblem::candidates`].
    ///
    /// Record the number of potential change outputs in [`DrainWeights::n_outputs`]. The selector
    /// then accounts for the resulting output-count varint weight change automatically.
    pub fn new(problem: &'a SelectionProblem) -> Self {
        let n = problem.len();
        Self {
            problem,
            selected: Bitset::with_capacity(n),
            banned: Bitset::with_capacity(n),
            candidate_order: Arc::new((0..n).collect::<Vec<_>>()),
        }
    }

    /// What this selector is funding.
    pub fn target(&self) -> Target {
        self.problem.target()
    }

    /// The selection problem this selector is solving.
    pub fn problem(&self) -> &'a SelectionProblem {
        self.problem
    }

    /// Build a cached view of the current selection for aggregate queries and hypothetical updates.
    pub fn compute_view(&'a self) -> SelectionView<'a> {
        SelectionView::from_selector(self)
    }

    /// Iterate over all the candidates in their currently sorted order. Each item has the original
    /// index with the candidate.
    pub fn candidates(
        &self,
    ) -> impl DoubleEndedIterator<Item = (usize, Candidate)> + ExactSizeIterator + '_ {
        let cands = self.problem.candidates();
        self.candidate_order.iter().map(move |i| (*i, cands[*i]))
    }

    /// [`candidates`](Self::candidates), skipping the first `from_position` of the sorted order.
    ///
    /// `from_position` is a position in that order, not an index into
    /// [`SelectionProblem::candidates`] — unlike the `index` each item carries. It may equal the
    /// candidate count, which yields nothing; past that is a caller bug and panics.
    pub(crate) fn candidates_from(
        &self,
        from_position: usize,
    ) -> impl DoubleEndedIterator<Item = (usize, Candidate)> + ExactSizeIterator + '_ {
        let cands = self.problem.candidates();
        self.candidate_order[from_position..]
            .iter()
            .map(move |i| (*i, cands[*i]))
    }

    /// Get the candidate at `index`. `index` refers to its position in
    /// [`SelectionProblem::candidates`].
    pub fn candidate(&self, index: usize) -> Candidate {
        self.problem.candidate(index)
    }

    /// Deselect a candidate at `index`, its position in [`SelectionProblem::candidates`].
    pub fn deselect(&mut self, index: usize) -> bool {
        self.selected.remove(index)
    }

    /// Convenience method to pick elements of a slice by the indices that are currently selected.
    ///
    /// The slice must contain one element per [`SelectionProblem::candidates`] entry in construction
    /// order.
    pub fn apply_selection<T>(&self, candidates: &'a [T]) -> impl Iterator<Item = &'a T> + '_ {
        self.selected.iter().map(move |i| &candidates[i])
    }

    /// Select the candidate at `index`, its position in [`SelectionProblem::candidates`].
    pub fn select(&mut self, index: usize) -> bool {
        assert!(index < self.problem.len());
        self.selected.insert(index)
    }

    /// Select the next unselected candidate in the current candidate order.
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
    /// `index` is its position in [`SelectionProblem::candidates`].
    ///
    /// [`unselected`]: Self::unselected
    /// [`unselected_indices`]: Self::unselected_indices
    pub fn ban(&mut self, index: usize) {
        self.banned.insert(index);
    }

    pub(crate) fn unban(&mut self, index: usize) {
        self.banned.remove(index);
    }

    /// Gets the list of inputs that have been banned by [`ban`].
    ///
    /// [`ban`]: Self::ban
    pub fn banned(&self) -> &Bitset {
        &self.banned
    }

    /// Whether the candidate at `index` in [`SelectionProblem::candidates`] is selected.
    pub fn is_selected(&self, index: usize) -> bool {
        self.selected.contains(index)
    }

    /// Returns true if no candidates have been selected.
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// The unconfirmed ancestors the current selection drags in (indices into
    /// [`SelectionProblem::ancestors`]).
    ///
    /// This is the **union** over the selected candidates, so an ancestor shared by several of them
    /// appears once. Derived from `selected` on demand: deselecting a candidate keeps an ancestor
    /// that another selected candidate still drags in.
    pub fn selected_ancestors(&self) -> Bitset {
        let mut union = Bitset::with_capacity(self.problem.ancestors().len());
        if self.problem.has_ancestors() {
            for cand_index in self.selected.iter() {
                for &anc_index in self.problem.drags_in(cand_index) {
                    let anc_index = anc_index as usize;
                    union.insert(anc_index);
                }
            }
        }
        union
    }

    /// The unconfirmed ancestors that are not dragged in yet but could still be, i.e. those of the
    /// [`unselected`](Self::unselected) candidates. Respects [`ban`](Self::ban).
    ///
    /// These are exactly the ancestors a descendant of this selection can add.
    pub fn addable_ancestors(&self) -> Bitset {
        let mut union = Bitset::with_capacity(self.problem.ancestors().len());
        if self.problem.has_ancestors() {
            let already = self.selected_ancestors();
            for cand_index in self.unselected_indices() {
                for &anc_index in self.problem.drags_in(cand_index) {
                    let anc_index = anc_index as usize;
                    if !already.contains(anc_index) {
                        union.insert(anc_index);
                    }
                }
            }
        }
        union
    }

    /// Sorts the candidates by the comparison function.
    ///
    /// The comparison function takes the candidate's index and the [`Candidate`].
    ///
    /// Note this function does not change the index of the candidates after sorting, just the order
    /// in which they will be returned when iterating over them in [`candidates`] and [`unselected`].
    ///
    /// [`candidates`]: CoinSelector::candidates
    /// [`unselected`]: CoinSelector::unselected
    pub fn sort_candidates_by<F>(&mut self, mut cmp: F)
    where
        F: FnMut((usize, Candidate), (usize, Candidate)) -> core::cmp::Ordering,
    {
        let candidates = self.problem.candidates();
        Arc::make_mut(&mut self.candidate_order)
            .sort_by(|a, b| cmp((*a, candidates[*a]), (*b, candidates[*b])))
    }

    /// Sorts the candidates by the key function.
    ///
    /// The key function takes the candidate's index and the [`Candidate`].
    ///
    /// Note this function does not change the index of the candidates after sorting, just the order
    /// in which they will be returned when iterating over them in [`candidates`] and [`unselected`].
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

    /// Shuffle the candidates with Fisher-Yates algorithm.
    ///
    /// `rng` should yield uniform `u64`s.
    pub fn shuffle_candidates(&mut self, mut rng: impl FnMut() -> u64) {
        let candidates = Arc::make_mut(&mut self.candidate_order);
        for i in (1..candidates.len()).rev() {
            let j = (rng() % (i as u64 + 1)) as usize;
            candidates.swap(i, j);
        }
    }

    /// The selected candidates with their index.
    pub fn selected(
        &self,
    ) -> impl ExactSizeIterator<Item = (usize, Candidate)> + DoubleEndedIterator + '_ {
        let cands = self.problem.candidates();
        self.selected.iter().map(move |index| (index, cands[index]))
    }

    /// The unselected candidates with their index.
    ///
    /// The candidates are returned in sorted order. See [`sort_candidates_by`].
    ///
    /// [`sort_candidates_by`]: Self::sort_candidates_by
    /// Note [`SelectionView`](crate::SelectionView) shadows this with a version that skips the
    /// prefix branch and bound has already decided. A method *on `CoinSelector`* that calls
    /// `self.unselected()` gets this one even when reached through a view, so anything on the
    /// search's hot path belongs on the view instead.
    pub fn unselected(&self) -> impl DoubleEndedIterator<Item = (usize, Candidate)> + '_ {
        let cands = self.problem.candidates();
        self.unselected_indices().map(move |i| (i, cands[i]))
    }

    /// The weight of the lightest unselected (addable) candidate, or `None` when nothing is left to
    /// add.
    ///
    /// This is a lower bound on the extra input weight any descendant selection must take on to add
    /// more value, which weight-aware branch-and-bound bounds use to reason about `max_weight`.
    pub fn min_input_weight(&self) -> Option<u64> {
        self.unselected()
            .map(|(_, candidate)| candidate.weight)
            .min()
    }

    /// The indices of the selected candidates.
    pub fn selected_indices(&self) -> &Bitset {
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
            .copied()
            .filter(move |&index| !(self.selected.contains(index) || self.banned.contains(index)))
    }

    /// Whether there are any unselected candidates left.
    pub fn is_exhausted(&self) -> bool {
        self.unselected_indices().next().is_none()
    }

    /// Select all unselected candidates
    pub fn select_all(&mut self) {
        loop {
            if !self.select_next() {
                break;
            }
        }
    }

    /// Select all candidates with an *effective value* greater than 0 at the provided `feerate`.
    ///
    /// A candidate is effective if it provides more value than it costs at `feerate`.
    ///
    /// This looks at each candidate's own value and weight only: a candidate that pays for itself
    /// but drags in an unconfirmed ancestor still counts as effective, even if the resulting
    /// [`ancestor_bump`](SelectionView::ancestor_bump) outweighs it. Selection-dependent input-count and
    /// witness serialization overhead are also excluded from this standalone calculation.
    pub fn select_all_effective(&mut self, feerate: FeeRate) {
        for i in 0..self.candidate_order.len() {
            let cand_index = self.candidate_order[i];
            if self.selected.contains(cand_index)
                || self.banned.contains(cand_index)
                || self.problem.candidates()[cand_index].effective_value(feerate) <= 0.0
            {
                continue;
            }
            self.selected.insert(cand_index);
        }
    }

    /// Select candidates until `target` has been met.
    ///
    /// # Errors
    ///
    /// - [`SelectError::InsufficientFunds`] if this in-order greedy selection exhausts the candidates
    ///   without covering the target value. Another subset may still work; use branch and bound to
    ///   search for one.
    /// - [`SelectError::MaxWeightExceeded`] if the value is met but the resulting selection exceeds
    ///   [`Target::max_weight`]. Note this only reflects *this* in-order greedy selection; a
    ///   different selection might still fit the cap (use branch and bound to search for one).
    ///
    /// This is especially relevant with unconfirmed ancestors: selecting everything can fail while
    /// a subset that drags in less ancestor fee debt would meet the target.
    pub fn select_until_target_met(&mut self) -> Result<(), SelectError> {
        let mut excess = 0_i64;
        let mut is_within_max_weight = true;
        self.select_until(|view| {
            excess = view.excess(Drain::NONE);
            is_within_max_weight = view.is_within_max_weight(DrainWeights::NONE);
            excess >= 0
        })
        .ok_or_else(|| {
            SelectError::InsufficientFunds(InsufficientFunds {
                missing: excess.unsigned_abs(),
            })
        })?;
        if !is_within_max_weight {
            return Err(SelectError::MaxWeightExceeded);
        }
        Ok(())
    }

    /// Select candidates until some predicate has been satisfied.
    ///
    /// The predicate is handed a [`SelectionView`] whose aggregates are updated incrementally as
    /// candidates are selected, so a predicate built from cached queries costs the same at every
    /// step regardless of how much is already selected.
    #[must_use]
    pub fn select_until(
        &mut self,
        mut predicate: impl FnMut(&SelectionView<'_>) -> bool,
    ) -> Option<()> {
        let mut cache = SelectionCache::from_selector(self);
        loop {
            if predicate(&SelectionView::with_cache(self, &cache)) {
                break Some(());
            }

            let index = self.unselected_indices().next()?;
            let candidate = self.candidate(index);
            self.select(index);
            cache.add(self.problem, index, candidate, true);
        }
    }

    /// Select candidates in random order ("Single Random Draw") until the change would be at least
    /// `change_lower`.
    ///
    /// Unlike [`run_bnb`] with [`LowestFee`], this doesn't minimize fees — it deliberately produces
    /// a healthy-sized (privacy-friendly) change output, avoiding tiny "toxic" change. It's a port
    /// of Bitcoin Core's `SelectCoinsSRD`; pass [`CHANGE_LOWER`] for Core's value.
    ///
    /// The change *amount* comes out random on its own: because candidates are added in random order
    /// and we stop as soon as the change reaches `change_lower`, the final change is wherever the
    /// last (random) input pushed it — at or above `change_lower`. So, like Core, we use a fixed
    /// lower bound rather than randomizing the self.target().
    ///
    /// On success it returns the [`Drain`] to attach, whose value is the achieved change (at least
    /// `change_lower`). Returns [`SelectError::InsufficientFunds`] if the target plus `change_lower`
    /// can't be met with the available candidates, or [`SelectError::MaxWeightExceeded`] if it can be
    /// met but the resulting selection exceeds the weight cap.
    ///
    /// `rng` shuffles the candidates; it yields uniform `u64`s, e.g. `|| my_rng.next_u64()`. Any
    /// already-selected candidates are kept and counted toward the self.target().
    ///
    /// [`run_bnb`]: Self::run_bnb
    /// [`LowestFee`]: crate::metrics::LowestFee
    // TODO: recover from exceeding `max_weight` by evicting the least-valuable inputs (matching
    // Core's `max_selection_weight`) instead of erroring with `MaxWeightExceeded`. Deferred until
    // the max-weight PR lands.
    pub fn select_srd(
        &mut self,
        drain_weights: DrainWeights,
        change_lower: u64,
        rng: impl FnMut() -> u64,
    ) -> Result<Drain, SelectError> {
        self.shuffle_candidates(rng);

        let mut is_within_max_weight = false;
        let mut excess = 0_i64;

        self.select_until(|cs| {
            is_within_max_weight = cs.is_within_max_weight(drain_weights);
            excess = cs.excess(Drain {
                weights: drain_weights,
                value: 0,
            });
            excess >= change_lower as i64 || !is_within_max_weight
        })
        .ok_or_else(|| {
            SelectError::InsufficientFunds(InsufficientFunds {
                missing: (change_lower as i64 - excess).unsigned_abs(),
            })
        })?;

        if !is_within_max_weight {
            return Err(SelectError::MaxWeightExceeded);
        }

        Ok(Drain {
            weights: drain_weights,
            value: excess as u64,
        })
    }

    /// Return an iterator that can be used to select candidates.
    pub fn select_iter(self) -> SelectIter<'a> {
        SelectIter { cs: self.clone() }
    }

    /// Iterates over rounds of branch and bound to minimize the score of the provided
    /// [`BnbMetric`].
    ///
    /// Not every iteration will return a solution. If a solution is found, we return the selection
    /// and score. Each subsequent solution guarantees a lower (better) score than the last.
    ///
    /// Most callers should use [`CoinSelector::run_bnb`] instead, especially when they need the
    /// change output selected by the metric.
    /// On a problem with unconfirmed ancestors this dives and then deepens on the bound (see
    /// [`bnb_solutions_hybrid`](Self::bnb_solutions_hybrid)); otherwise it is a plain depth-first
    /// dive, which is what [`bnb_solutions_dive_only`](Self::bnb_solutions_dive_only) always gives.
    ///
    /// The gate is not a tuning choice, it is where the problem being solved exists. Deepening is
    /// there to escape a dive that stalled because the candidate order — value per weight — cannot
    /// see what a coin's unconfirmed parents cost to bump. With no ancestors there is no such
    /// blindness, the dive does not stall, and paying to re-expand from the root is a straight loss:
    /// measured over 4,000 random ancestor-free pools it is worse on 54 and better on 1.
    pub fn bnb_solutions<M: BnbMetric>(
        &self,
        metric: M,
    ) -> impl Iterator<Item = Option<(CoinSelector<'a>, Ordf32)>> {
        self.default_bnb_iter(metric)
    }

    /// The iterator [`bnb_solutions`](Self::bnb_solutions) and [`run_bnb`](Self::run_bnb) share.
    ///
    /// `run_bnb` needs the concrete type to reach the metric for its drain, so the choice of
    /// traversal lives here rather than being made twice.
    pub(crate) fn default_bnb_iter<M: BnbMetric>(&self, metric: M) -> crate::bnb::BnbIter<'a, M> {
        let deepen = self.problem.has_ancestors();
        crate::bnb::BnbIter::configured(
            self.clone(),
            metric,
            if deepen { Some(Self::DEFAULT_DEEPENING_EPS) } else { None },
            if deepen {
                Some(
                    Self::DEFAULT_DIVE_FLOOR_PER_CANDIDATE
                        .saturating_mul(self.candidates().count() as u64),
                )
            } else {
                None
            },
        )
    }

    /// [`bnb_solutions`](Self::bnb_solutions) as a plain depth-first dive, with no deepening.
    pub fn bnb_solutions_dive_only<M: BnbMetric>(
        &self,
        metric: M,
    ) -> impl Iterator<Item = Option<(CoinSelector<'a>, Ordf32)>> {
        crate::bnb::BnbIter::new(self.clone(), metric)
    }

    /// Relative step between deepening thresholds.
    ///
    /// A strict schedule — one pass per distinct bound — costs up to 194x more nodes on the
    /// fixtures that already match a priority queue node for node, because they pay for
    /// re-expansion and buy nothing. 0.1 holds that overhead near 1.5x while collapsing the pass
    /// count to single digits.
    pub const DEFAULT_DEEPENING_EPS: f32 = 0.1;

    /// [`bnb_solutions`](Self::bnb_solutions), dived first and then deepened, with both knobs given.
    ///
    /// Depth-first reaches complete selections immediately but prunes against whatever its dive
    /// order found; deepening recovers a priority queue's node ordering but reaches complete
    /// selections late. This takes both: dive until the incumbent stops improving, then deepen from
    /// the root keeping that incumbent.
    ///
    /// Note what that does *not* promise. Against a dive stopped at the same handover point the
    /// incumbent only improves, so the hybrid cannot be worse. Against a dive that keeps spending
    /// the whole budget on descending, it can be: the rounds spent re-expanding from the root are
    /// rounds the dive would have spent going deeper. On pools with no unconfirmed ancestors, where
    /// the dive has no ordering pathology to escape, that trade is a loss — which is why
    /// [`bnb_solutions`](Self::bnb_solutions) only picks this when the problem has ancestors.
    ///
    /// `eps` is the relative step between deepening thresholds and `floor_per_candidate` is how long
    /// the opening dive is protected; see [`DEFAULT_DEEPENING_EPS`](Self::DEFAULT_DEEPENING_EPS) and
    /// [`DEFAULT_DIVE_FLOOR_PER_CANDIDATE`](Self::DEFAULT_DIVE_FLOOR_PER_CANDIDATE).
    pub fn bnb_solutions_hybrid<M: BnbMetric>(
        &self,
        metric: M,
        eps: f32,
        floor_per_candidate: u64,
    ) -> impl Iterator<Item = Option<(CoinSelector<'a>, Ordf32)>> {
        debug_assert!(eps > 0.0, "a non-positive `eps` silently buys the strict schedule");
        let floor = floor_per_candidate.saturating_mul(self.candidates().count() as u64);
        crate::bnb::BnbIter::configured(self.clone(), metric, Some(eps), Some(floor))
    }

    /// How long the opening dive is protected for, per candidate.
    ///
    /// The dive needs a floor or it hands over before it has found anything, because the greedy
    /// incumbent is set before the first node and so leaves the "time since last improvement" rule
    /// with nothing to measure against. The floor has to scale with something, and the budget is not
    /// visible here — a caller may be spending rounds or wall clock. Candidate count is: a dive to a
    /// leaf costs at most one node per candidate, so this is that depth times a constant.
    ///
    /// Swept over 42 fixtures at 0, 5, 20, 50 and 200, under a 100,000-round budget and at 3 ms,
    /// 10 ms, 100 ms and 1000 ms of wall clock. 5 is the best worst case: 2.59% of total package fee
    /// better than 50 at 3 ms and 0.28% better at 10 ms, against losing by under 0.1% at 100 ms and
    /// 1000 ms. Dropping the floor to 0 — handing over before the dive completes a single selection
    /// — costs 10.7%, so the floor is doing real work; it just does not need to be large.
    ///
    /// That the right value is small follows from what the dive is for. It only has to reach one
    /// complete selection, which is one root-to-leaf path, so a few nodes per candidate is the
    /// natural scale and anything beyond that is the dive refusing to hand over.
    ///
    /// This was 200 when the dive was measured against a node that cost several times more. The
    /// floor is counted in nodes, so making nodes cheaper made the same floor a longer dive in wall
    /// clock, and the tuning moved with it.
    pub const DEFAULT_DIVE_FLOOR_PER_CANDIDATE: u64 = 5;

    /// Run branch and bound to minimize the score of the provided [`BnbMetric`].
    ///
    /// The method keeps trying until no better solution can be found, or we reach `max_rounds`. If a
    /// solution is found, the score and the change output ([`Drain`]) that the metric decided on are
    /// returned. Otherwise, we error with [`NoBnbSolution`].
    ///
    /// Use [`CoinSelector::bnb_solutions`] to access the branch and bound iterator directly.
    pub fn run_bnb<M: BnbMetric>(
        &mut self,
        metric: M,
        max_rounds: usize,
    ) -> Result<(Ordf32, Drain), NoBnbSolution> {
        let mut iter = self.default_bnb_iter(metric);
        let mut rounds = 0_usize;
        let best = iter
            .by_ref()
            .take(max_rounds)
            .inspect(|_| rounds += 1)
            .flatten()
            .last();
        if let Some((selector, score)) = best {
            let drain = iter.metric.drain(&selector.compute_view());
            *self = selector;
            return Ok((score, drain));
        }

        // No solution. If the iterator still has an item we stopped at the round limit and a
        // solution may still exist with a larger `max_rounds`. Otherwise the tree was fully
        // explored, so no selection satisfies the target — a genuine infeasibility, split into
        // value vs weight. (With unconfirmed ancestors `is_fundable` is only a heuristic, so the
        // split between the two can be wrong — the infeasibility itself is not.)
        if iter.next().is_some() {
            assert_eq!(rounds, max_rounds); // still-yielding ⟹ we truncated at the cap
            return Err(NoBnbSolution::RoundLimit { max_rounds, rounds });
        }
        if !self.compute_view().is_fundable() {
            return Err(NoBnbSolution::InsufficientFunds);
        }
        Err(NoBnbSolution::MaxWeightExceeded)
    }
}

// Allow this for now due to MSRV
#[allow(clippy::uninlined_format_args)]
impl core::fmt::Display for CoinSelector<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[")?;
        let mut candidates = self.candidates().peekable();

        while let Some((i, _)) = candidates.next() {
            write!(f, "{}", i)?;
            if self.is_selected(i) {
                write!(f, "✔")?;
            } else if self.banned().contains(i) {
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

impl DoubleEndedIterator for SelectIter<'_> {
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

/// Error returned by [`CoinSelector::select_until_target_met`].
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum SelectError {
    /// The candidates can't cover the target value.
    InsufficientFunds(InsufficientFunds),
    /// The value target is met, but the resulting selection exceeds [`Target::max_weight`].
    MaxWeightExceeded,
}

impl From<InsufficientFunds> for SelectError {
    fn from(e: InsufficientFunds) -> Self {
        SelectError::InsufficientFunds(e)
    }
}

impl core::fmt::Display for SelectError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            SelectError::InsufficientFunds(e) => write!(f, "{}", e),
            SelectError::MaxWeightExceeded => {
                write!(
                    f,
                    "Selection meets the target value but exceeds `max_weight`."
                )
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for SelectError {}

/// Error returned by [`CoinSelector::run_bnb`] when it yields no solution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoBnbSolution {
    /// The candidates can't cover the target value, so no selection is possible.
    ///
    /// With unconfirmed ancestors this is decided by the heuristic
    /// [`SelectionView::is_fundable`](crate::SelectionView::is_fundable), so it may be reported where
    /// [`MaxWeightExceeded`](Self::MaxWeightExceeded) fits better, and vice versa. Either way the
    /// search was exhaustive: there is no solution.
    InsufficientFunds,
    /// Some selection covers the target value, but every one of them exceeds
    /// [`Target::max_weight`].
    ///
    /// Only reachable with a metric that enforces the cap (e.g. [`LowestFee`]); a cap-blind metric
    /// returns an over-cap selection rather than failing.
    ///
    /// [`LowestFee`]: crate::metrics::LowestFee
    MaxWeightExceeded,
    /// The round limit was reached before the search finished — a solution may still exist with a
    /// larger `max_rounds`.
    RoundLimit {
        /// Maximum rounds set by the caller.
        max_rounds: usize,
        /// Number of branch-and-bound rounds performed.
        rounds: usize,
    },
}

// Allow this for now due to MSRV
#[allow(clippy::uninlined_format_args)]
impl core::fmt::Display for NoBnbSolution {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NoBnbSolution::InsufficientFunds => {
                write!(
                    f,
                    "no bnb solution: candidates cannot cover the target value"
                )
            }
            NoBnbSolution::MaxWeightExceeded => {
                write!(
                    f,
                    "no bnb solution: no selection meets the target within max_weight"
                )
            }
            NoBnbSolution::RoundLimit { max_rounds, rounds } => write!(
                f,
                "no bnb solution found after {} rounds (max rounds is {})",
                rounds, max_rounds
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for NoBnbSolution {}

/// A `Candidate` represents an input candidate for [`CoinSelector`].
///
/// This can either be a single UTXO, or a group of UTXOs that should be spent together. A group
/// may mix legacy and segwit inputs; set [`legacy_count`] and [`segwit_count`] accordingly.
///
/// [`legacy_count`]: Candidate::legacy_count
/// [`segwit_count`]: Candidate::segwit_count
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    /// Total value of the UTXO(s) that this [`Candidate`] represents.
    pub value: u64,
    /// Total weight of including this/these UTXO(s).
    /// `txin` fields: `prevout`, `nSequence`, `scriptSigLen`, `scriptSig`, `scriptWitnessLen`,
    /// `scriptWitness` should all be included.
    ///
    /// For legacy inputs, do *not* include the `scriptWitnessLen` byte: a legacy input only
    /// serializes an (empty) witness when the transaction has a witness section, and
    /// [`SelectionView::input_weight`] adds that 1 WU per legacy input once any segwit input is
    /// selected.
    pub weight: u64,
    /// Total number of segwit inputs.
    ///
    /// If any selected candidate has a non-zero `segwit_count`, the transaction serializes a
    /// witness section (marker + flag, 2 WU) and every input — including legacy ones — pays for
    /// a witness.
    pub segwit_count: usize,
    /// Total number of legacy (non-segwit) inputs.
    ///
    /// Each legacy input serializes an empty witness (1 WU) when the transaction has a witness
    /// section; [`SelectionView::input_weight`] prices this per legacy input, so grouped legacy
    /// inputs are counted exactly.
    pub legacy_count: usize,
}

impl Candidate {
    /// Create a [`Candidate`] input that spends a single taproot keyspend output.
    pub fn new_tr_keyspend(value: u64) -> Self {
        let weight = TR_KEYSPEND_SATISFACTION_WEIGHT;
        Self::new_segwit(value, weight)
    }

    /// Create a new [`Candidate`] that represents a single segwit input.
    ///
    /// `satisfaction_weight` is the additional weight (in weight units) required to satisfy the input
    /// beyond [`TXIN_BASE_WEIGHT`] (e.g. `scriptWitnessLen + scriptWitness` in WU at 1 WU/byte, plus
    /// any `scriptSig` data and extra `scriptSigLen` varint bytes if nested/wrapped segwit).
    ///
    /// Note that [`TXIN_BASE_WEIGHT`] already accounts for the outpoint, `nSequence`, and 1 byte for
    /// `scriptSigLen`.
    pub fn new_segwit(value: u64, satisfaction_weight: u64) -> Candidate {
        let weight = TXIN_BASE_WEIGHT + satisfaction_weight;
        Candidate {
            value,
            weight,
            segwit_count: 1,
            legacy_count: 0,
        }
    }

    /// Create a new [`Candidate`] that represents a single legacy (non-segwit) input.
    ///
    /// `satisfaction_weight` is the additional weight (in weight units) required to satisfy the input
    /// beyond [`TXIN_BASE_WEIGHT`] (e.g. `scriptSig` at 4 WU/byte, plus 4 WU per extra `scriptSigLen`
    /// varint byte if `scriptSig` exceeds 252 bytes).
    ///
    /// Note that [`TXIN_BASE_WEIGHT`] already accounts for the outpoint, `nSequence`, and 1 byte for
    /// `scriptSigLen`.
    pub fn new_legacy(value: u64, satisfaction_weight: u64) -> Candidate {
        let weight = TXIN_BASE_WEIGHT + satisfaction_weight;
        Candidate {
            value,
            weight,
            segwit_count: 0,
            legacy_count: 1,
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
