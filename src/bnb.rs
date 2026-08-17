use crate::{float::Ordf32, Drain, SelectionCache, SelectionView};
use super::CoinSelector;
use alloc::vec::Vec;

/// An [`Iterator`] that iterates over rounds of branch and bound to minimize the score of the
/// provided [`BnbMetric`].
#[derive(Debug)]
pub(crate) struct BnbIter<'a, M: BnbMetric> {
    selector: CoinSelector<'a>,
    cache: SelectionCache,
    stack: Vec<Frame>,
    best: Option<Ordf32>,
    /// The greedy selection, yielded before the first node is expanded. Its score is `best`:
    /// nothing else can have run yet, so the two are set together. See
    /// [`seed_greedy_incumbent`](BnbIter::seed_greedy_incumbent).
    seed: Option<CoinSelector<'a>>,
    exhausted: bool,
    /// Iterative deepening: the current pass's ceiling on the bound. `None` disables deepening
    /// entirely, which is the traversal exactly as it was before.
    threshold: Option<Ordf32>,
    /// Smallest bound rejected *by the threshold* this pass — the next pass's floor.
    ///
    /// Children rejected for being no better than the incumbent are deliberately not recorded:
    /// they can never become interesting, and folding them in would waste passes.
    next_threshold: Option<Ordf32>,
    /// Relative step for the threshold schedule. The schedule is a speed knob, never a correctness
    /// one: raising the threshold past the next rejected bound only ever *adds* nodes to a pass.
    deepening: Option<f32>,
    /// Still in the opening dive, before any threshold applies.
    ///
    /// Deepening reconstructs the queue's ordering but is not anytime: under a ceiling low enough to
    /// be useful, the early passes may not reach a complete selection at all. On a pool too large to
    /// exhaust that is strictly worse than diving, which reaches leaves immediately. So dive first,
    /// and deepen only once the dive stops paying — the incumbent carries over, so the deepening
    /// phase cannot return anything worse than the dive already found.
    diving: bool,
    /// Smallest number of nodes the opening dive is always given before it may hand over.
    dive_floor: u64,
    /// Nodes expanded so far, and the count when the incumbent last improved.
    nodes: u64,
    last_improvement: u64,
    /// The `BnBMetric` that will score each selection
    pub(crate) metric: M,
}

#[derive(Debug)]
struct Frame {
    is_inclusion: bool,
    index: usize,
    cursor: usize,
    next_cursor: usize,
    banned: Vec<usize>,
    sibling_pending: bool,
}

impl<'a, M: BnbMetric> Iterator for BnbIter<'a, M> {
    type Item = Option<(CoinSelector<'a>, Ordf32)>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(seed) = self.seed.take() {
            let score = self.best.expect("the seed and `best` are set together");
            return Some(Some((seed, score)));
        }

        if self.exhausted {
            return None;
        }

        self.nodes += 1;
        if self.diving && self.dive_is_stalled() {
            self.stop_diving();
        }

        // {
        //     println!("=========================== {:?}", self.best);
        //     println!("{} {:?}", &self.selector, self.bound_of_current());
        //     for frame in self.stack.iter() {
        //         println!(
        //             "\t{} [{}] cursor={} sibling_pending={}",
        //             if frame.is_inclusion { "IN " } else { "EX " },
        //             frame.index,
        //             frame.cursor,
        //             frame.sibling_pending,
        //         );
        //     }
        //     let _ = std::io::stdin().read_line(&mut alloc::string::String::new());
        // }

        let return_val = if !self.is_exclusion_node() {
            self.try_record_best()
                .map(|score| (self.selector.clone(), score))
        } else {
            None
        };

        if !self.descend() && !self.backtrack_to_next_branch() && !self.start_next_pass() {
            self.exhausted = true;
        }

        Some(return_val)
    }
}

impl<'a, M: BnbMetric> BnbIter<'a, M> {
    pub(crate) fn new(selector: CoinSelector<'a>, metric: M) -> Self {
        Self::with_deepening(selector, metric, None)
    }

    pub(crate) fn with_deepening(
        selector: CoinSelector<'a>,
        metric: M,
        deepening: Option<f32>,
    ) -> Self {
        Self::configured(selector, metric, deepening, None)
    }

    pub(crate) fn configured(
        mut selector: CoinSelector<'a>,
        metric: M,
        deepening: Option<f32>,
        dive_first: Option<u64>,
    ) -> Self {
        if metric.requires_ordering_by_descending_value_pwu() {
            selector.sort_candidates_by_descending_value_pwu();
        }

        let cache = SelectionCache::from_selector(&selector);
        let mut iter = BnbIter {
            selector,
            cache,
            stack: Vec::new(),
            best: None,
            seed: None,
            exhausted: false,
            threshold: None,
            next_threshold: None,
            deepening,
            diving: dive_first.is_some() && deepening.is_some(),
            dive_floor: dive_first.unwrap_or(0),
            nodes: 0,
            last_improvement: 0,
            metric,
        };

        iter.seed_greedy_incumbent();

        // Incumbent-only: the root must not be rejected by a threshold that is derived from it.
        if !iter.bound_is_promising() {
            iter.exhausted = true;
        }

        // The first pass admits exactly the root. A hybrid search sets this when it stops diving.
        if deepening.is_some() && !iter.diving {
            iter.threshold = iter.bound_of_current();
        }

        iter
    }

    /// Score the greedy prefix and adopt it as the incumbent.
    ///
    /// Without this the search is not anytime: a caller that runs out of rounds before the first
    /// complete selection gets nothing back and falls through to whatever fallback it has, which on
    /// a large pool is far worse than the selection a single greedy pass would have handed it. The
    /// seed costs one round and one scored selection, and since it is only an incumbent — the bound
    /// is unchanged and still admissible — the optimum stays reachable.
    ///
    /// It yields nothing for a metric that rejects the greedy prefix outright: overshooting the
    /// target is exactly what a greedy pass does.
    fn seed_greedy_incumbent(&mut self) {
        let mut seed = self.selector.clone();
        if seed.select_until_target_met().is_err() {
            return;
        }
        if let Some(score) = self.metric.score(&seed.compute_view()) {
            self.best = Some(score);
            self.seed = Some(seed);
        }
        self.seed_ancestry_aware_incumbent();
    }

    /// Take a second greedy prefix in descending `(value - own bump) / weight` order, and keep it if
    /// it scores better than the first.
    ///
    /// The search order is `value / weight`, which cannot see ancestry at all: a candidate whose
    /// unconfirmed parents cost more to bump than the next candidate is worth still sorts ahead of
    /// it. That is invisible on a pool the search can work through, because the search fixes it. On
    /// a pool it cannot — where it returns the prefix it started from — the ordering *is* the
    /// answer, and the blind one drags in parents it did not have to.
    ///
    /// So this pass reprices each candidate by what its own parents would cost, and takes the prefix
    /// in that order instead. `local_bump` overcounts a shared parent that some other selected
    /// candidate would have dragged in anyway, which is why the result is an incumbent and not the
    /// order the search runs in: the ordering the bound relies on is untouched, and the reordered
    /// prefix is adopted only when the metric scores it better.
    fn seed_ancestry_aware_incumbent(&mut self) {
        let problem = self.selector.problem();
        // With no unconfirmed ancestors every bump is zero, so this is the prefix already taken.
        if !problem.has_ancestors() {
            return;
        }

        // Keyed once per candidate rather than inside the comparator: `local_bump` walks a
        // candidate's ancestor set, and a sort would ask for it O(n log n) times.
        let keys = problem
            .candidates()
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let repriced = candidate.value.saturating_sub(problem.local_bump(index));
                core::cmp::Reverse((Ordf32(repriced as f32 / candidate.weight as f32), repriced))
            })
            .collect::<Vec<_>>();

        let mut seed = self.selector.clone();
        seed.sort_candidates_by_key(|(index, _)| keys[index]);
        if seed.select_until_target_met().is_err() {
            return;
        }
        if let Some(score) = self.metric.score(&seed.compute_view()) {
            if self.best.map_or(true, |best| score < best) {
                self.best = Some(score);
                self.seed = Some(seed);
            }
        }
    }

    fn is_exclusion_node(&self) -> bool {
        self.stack.last().map_or(false, |frame| !frame.is_inclusion)
    }

    fn try_record_best(&mut self) -> Option<Ordf32> {
        let view = SelectionView::with_cache_from(&self.selector, &self.cache, self.cursor());
        let score = self.metric.score(&view)?;
        let better = match self.best {
            Some(best_score) => score < best_score,
            None => true,
        };
        if better {
            self.best = Some(score);
            self.last_improvement = self.nodes;
            Some(score)
        } else {
            None
        }
    }

    /// Bound the current node, telling the view how much of the candidate order it can skip.
    ///
    /// Every candidate before the cursor has already been decided — included by an inclusion frame,
    /// or banned by an exclusion one — so a metric asking about undecided candidates never has to
    /// look at them. That is what keeps the cost of a node proportional to the answer rather than
    /// to the depth it was found at.
    fn bound_of_current(&mut self) -> Option<Ordf32> {
        let view = SelectionView::with_cache_from(&self.selector, &self.cache, self.cursor());
        self.metric.bound(&view)
    }

    fn is_promising(&self, bound: Option<Ordf32>) -> bool {
        match (bound, self.best) {
            (Some(bound), Some(best)) => best > bound,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    /// Whether to descend into a child, and the place the deepening threshold is applied.
    ///
    /// Takes `&mut self` because a child rejected by the threshold contributes the next pass's
    /// floor. Rejection by the incumbent contributes nothing.
    fn admit(&mut self, bound: Option<Ordf32>) -> bool {
        if !self.is_promising(bound) {
            return false;
        }
        let bound = match bound {
            Some(bound) => bound,
            None => return false,
        };
        if let Some(threshold) = self.threshold {
            if bound > threshold {
                self.next_threshold = Some(match self.next_threshold {
                    Some(next) if next <= bound => next,
                    _ => bound,
                });
                return false;
            }
        }
        true
    }

    fn bound_is_promising(&mut self) -> bool {
        let bound = self.bound_of_current();
        self.admit(bound)
    }

    /// Unwind every frame, leaving the selector and cache as they were at the root.
    ///
    /// In place, using the same undo paths backtracking uses — rebuilding the `CoinSelector` or the
    /// `SelectionCache` per pass is what would put the memory back.
    fn reset_to_root(&mut self) {
        while let Some(frame) = self.stack.pop() {
            if frame.is_inclusion {
                self.undo_include(frame.index);
            } else {
                self.undo_exclude(&frame.banned);
            }
        }
    }

    /// Whether the opening dive has stopped paying for itself.
    ///
    /// The rule is self-scaling rather than a tuned constant: give up once the search has gone as
    /// long without an improvement as it took to find the one it has. A dive that keeps creeping
    /// downward — which is what a pool too large to exhaust does — keeps its budget; a dive that is
    /// stuck, which is the failure this is here to fix, hands over quickly.
    fn dive_is_stalled(&self) -> bool {
        self.nodes.saturating_sub(self.last_improvement)
            > self.last_improvement.max(self.dive_floor)
    }

    /// Leave the opening dive and begin deepening, keeping the incumbent the dive found.
    fn stop_diving(&mut self) {
        self.diving = false;
        self.reset_to_root();
        self.threshold = self.bound_of_current();
    }

    /// Raise the threshold and restart from the root. `false` means the search is over.
    fn start_next_pass(&mut self) -> bool {
        // A dive that runs out of tree has explored everything; there is nothing to deepen into.
        if self.diving {
            return false;
        }
        let eps = match self.deepening {
            Some(eps) => eps,
            None => return false,
        };
        // The incumbent is proven optimal: every node that could beat it had a bound at or below
        // the threshold, so it was visited this pass or an earlier one.
        if let (Some(best), Some(threshold)) = (self.best, self.threshold) {
            if best <= threshold {
                return false;
            }
        }
        // Nothing was rejected by the threshold, so the whole tree is explored.
        let next = match self.next_threshold.take() {
            Some(next) => next,
            None => return false,
        };
        let grown = match self.threshold {
            Some(threshold) => {
                let stepped = threshold.0 * (1.0 + eps);
                Ordf32(if stepped > next.0 { stepped } else { next.0 })
            }
            None => next,
        };
        self.threshold = Some(grown);
        // No `reset_to_root` here: this runs only once `backtrack_to_next_branch` has popped every
        // frame, so the selector is already back at the root. `stop_diving` is the caller that
        // needs it, because it interrupts a dive mid-descent.
        debug_assert!(self.stack.is_empty(), "a pass ended without unwinding its stack");
        true
    }

    fn cursor(&self) -> usize {
        self.stack.last().map_or(0, |frame| frame.next_cursor)
    }

    fn next_candidate(&self, start: usize) -> Option<(usize, usize)> {
        for (cursor, (index, _)) in (start..).zip(self.selector.candidates_from(start)) {
            if !self.selector.is_selected(index) && !self.selector.banned().contains(index) {
                return Some((index, cursor));
            }
        }
        None
    }

    fn exclusion_plan(&self, index: usize, cursor: usize) -> (Vec<usize>, usize) {
        let next = self.selector.candidate(index);
        let to_ban = (
            next.value,
            next.weight,
            next.segwit_count,
            next.legacy_count,
        );
        let to_ban_drags_in = self.selector.problem().drags_in(index);
        let mut banned = alloc::vec![index];
        let mut next_cursor = cursor + 1;
        for (next_index, next) in self.selector.candidates_from(cursor + 1) {
            if self.selector.is_selected(next_index) || self.selector.banned().contains(next_index)
            {
                next_cursor += 1;
                continue;
            }
            if (
                next.value,
                next.weight,
                next.segwit_count,
                next.legacy_count,
            ) != to_ban
                || self.selector.problem().drags_in(next_index) != to_ban_drags_in
            {
                break;
            }
            // println!("banning: [{}] {:?}", next_index, next);
            banned.push(next_index);
            next_cursor += 1;
        }
        (banned, next_cursor)
    }

    fn apply_include(&mut self, index: usize) {
        let candidate = self.selector.candidate(index);
        self.selector.select(index);
        self.cache
            .add(self.selector.problem(), index, candidate, true);
    }

    fn undo_include(&mut self, index: usize) {
        let candidate = self.selector.candidate(index);
        self.selector.deselect(index);
        self.cache
            .sub(self.selector.problem(), index, candidate, true);
    }

    fn apply_exclude(&mut self, banned: &[usize]) {
        for &index in banned {
            self.selector.ban(index);
            self.cache.ban(self.selector.problem(), index);
        }
    }

    fn undo_exclude(&mut self, banned: &[usize]) {
        for &index in banned.iter().rev() {
            self.selector.unban(index);
            self.cache.unban(self.selector.problem(), index);
        }
    }

    fn push_include(&mut self, index: usize, cursor: usize, sibling_pending: bool) {
        self.apply_include(index);
        self.stack.push(Frame {
            is_inclusion: true,
            index,
            cursor,
            next_cursor: cursor + 1,
            banned: Vec::new(),
            sibling_pending,
        });
    }

    fn push_exclude(
        &mut self,
        index: usize,
        cursor: usize,
        banned: Vec<usize>,
        next_cursor: usize,
        sibling_pending: bool,
    ) {
        self.apply_exclude(&banned);
        self.stack.push(Frame {
            is_inclusion: false,
            index,
            cursor,
            next_cursor,
            banned,
            sibling_pending,
        });
    }

    fn descend(&mut self) -> bool {
        let (index, cursor) = match self.next_candidate(self.cursor()) {
            Some(next) => next,
            None => return false,
        };

        self.apply_include(index);
        let inc_bound = self.bound_of_current();
        let inc_ok = self.admit(inc_bound);
        self.undo_include(index);

        let (banned, exc_next_cursor) = self.exclusion_plan(index, cursor);
        self.apply_exclude(&banned);
        let exc_bound = self.bound_of_current();
        let exc_ok = self.admit(exc_bound);
        self.undo_exclude(&banned);

        // println!(
        //     "\t\t(DESC) branch={} next=[{}] inc_lb={:?}{} exc_lb={:?}{}",
        //     self.selector,
        //     index,
        //     inc_bound,
        //     if inc_ok { "" } else { " (REJ)" },
        //     exc_bound,
        //     if exc_ok { "" } else { " (REJ)" },
        // );

        match (inc_ok, exc_ok) {
            (false, false) => false,
            (true, false) => {
                self.push_include(index, cursor, false);
                true
            }
            (false, true) => {
                self.push_exclude(index, cursor, banned, exc_next_cursor, false);
                true
            }
            (true, true) => {
                // Equal bounds prefer inclusion, matching the previous best-first tie-break.
                let include_first = match (inc_bound, exc_bound) {
                    (Some(inc), Some(exc)) => inc <= exc,
                    _ => true,
                };
                if include_first {
                    self.push_include(index, cursor, true);
                } else {
                    self.push_exclude(index, cursor, banned, exc_next_cursor, true);
                }
                true
            }
        }
    }

    fn backtrack_to_next_branch(&mut self) -> bool {
        while let Some(frame) = self.stack.pop() {
            // println!(
            //     "\t\t(BACK) undo {} [{}] sibling_pending={}",
            //     if frame.is_inclusion { "IN " } else { "EX " },
            //     frame.index,
            //     frame.sibling_pending,
            // );
            if frame.is_inclusion {
                self.undo_include(frame.index);
                if frame.sibling_pending {
                    let (banned, next_cursor) = self.exclusion_plan(frame.index, frame.cursor);
                    self.apply_exclude(&banned);
                    if self.bound_is_promising() {
                        self.stack.push(Frame {
                            is_inclusion: false,
                            index: frame.index,
                            cursor: frame.cursor,
                            next_cursor,
                            banned,
                            sibling_pending: false,
                        });
                        return true;
                    }
                    self.undo_exclude(&banned);
                }
            } else {
                self.undo_exclude(&frame.banned);
                if frame.sibling_pending {
                    self.apply_include(frame.index);
                    if self.bound_is_promising() {
                        self.stack.push(Frame {
                            is_inclusion: true,
                            index: frame.index,
                            cursor: frame.cursor,
                            next_cursor: frame.cursor + 1,
                            banned: Vec::new(),
                            sibling_pending: false,
                        });
                        return true;
                    }
                    self.undo_include(frame.index);
                }
            }
        }
        false
    }
}

/// A branch and bound metric where we minimize the [`Ordf32`] score.
///
/// This is to be used as input for [`CoinSelector::run_bnb`] or [`CoinSelector::bnb_solutions`].
pub trait BnbMetric {
    /// Get the score of a given selection.
    ///
    /// If this returns `None`, the selection is invalid.
    fn score(&mut self, view: &SelectionView<'_>) -> Option<Ordf32>;

    /// Get the lower bound score using a heuristic.
    ///
    /// This represents the best possible score of all descendant branches (according to the
    /// heuristic).
    ///
    /// If this returns `None`, the current branch and all descendant branches will not have valid
    /// solutions.
    fn bound(&mut self, view: &SelectionView<'_>) -> Option<Ordf32>;

    /// The change output (a.k.a. drain) this metric decides on for the given selection,
    /// or [`Drain::NONE`] if it decides there should be no change.
    ///
    /// Call this on a branch-and-bound solution to get the change output the metric optimized against.
    fn drain(&mut self, view: &SelectionView<'_>) -> Drain;

    /// Returns whether the metric requies we order candidates by descending value per weight unit.
    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        false
    }
}
