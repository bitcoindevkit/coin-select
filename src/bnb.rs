use crate::{float::Ordf32, Drain};

use super::CoinSelector;
use alloc::vec::Vec;

/// An [`Iterator`] that iterates over rounds of branch and bound to minimize the score of the
/// provided [`BnbMetric`].
///
/// The tree is searched depth-first, visiting the child with the better bound first and
/// backtracking in place, so only the current path is held in memory.
#[derive(Debug)]
pub(crate) struct BnbIter<'a, M: BnbMetric> {
    selector: CoinSelector<'a>,
    stack: Vec<Frame>,
    best: Option<Ordf32>,
    /// The greedy selection, yielded before the first node is expanded. Its score is `best`:
    /// nothing else can have run yet, so the two are set together. See
    /// [`seed_greedy_incumbent`](BnbIter::seed_greedy_incumbent).
    seed: Option<CoinSelector<'a>>,
    exhausted: bool,
    /// The `BnBMetric` that will score each selection
    pub(crate) metric: M,
}

/// A decision on the current path: either `index` was selected, or `banned` (`index` and the
/// candidates interchangeable with it) were banned.
#[derive(Debug)]
struct Frame {
    is_inclusion: bool,
    index: usize,
    /// Position of `index` in the candidate order.
    cursor: usize,
    /// Position to resume scanning for the next undecided candidate below this frame.
    next_cursor: usize,
    banned: Vec<usize>,
    /// Whether the other child of this frame's parent still needs visiting.
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

        // {
        //     println!("=========================== {:?}", self.best);
        //     println!("{} {:?}", &self.selector, self.bound_of_current(self.cursor()));
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

        // An exclusion node has the same selection as its parent, which was already scored.
        let return_val = if !self.is_exclusion_node() {
            self.try_record_best()
                .map(|score| (self.selector.clone(), score))
        } else {
            None
        };

        if !self.descend() && !self.backtrack_to_next_branch() {
            self.exhausted = true;
        }

        Some(return_val)
    }
}

impl<'a, M: BnbMetric> BnbIter<'a, M> {
    pub(crate) fn new(mut selector: CoinSelector<'a>, metric: M) -> Self {
        if metric.requires_ordering_by_descending_value_pwu() {
            selector.sort_candidates_by_descending_value_pwu();
        }

        let mut iter = BnbIter {
            selector,
            stack: Vec::new(),
            best: None,
            seed: None,
            exhausted: false,
            metric,
        };

        iter.seed_greedy_incumbent();

        if !iter.bound_is_promising(0) {
            iter.exhausted = true;
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
    /// It yields nothing for a metric that rejects the greedy prefix outright.
    fn seed_greedy_incumbent(&mut self) {
        let mut seed = self.selector.clone();
        if seed.select_until_target_met().is_err() {
            return;
        }
        if let Some(score) = self.metric.score(&seed) {
            self.best = Some(score);
            self.seed = Some(seed);
        }
    }

    fn is_exclusion_node(&self) -> bool {
        self.stack.last().map_or(false, |frame| !frame.is_inclusion)
    }

    fn try_record_best(&mut self) -> Option<Ordf32> {
        let decided_before = self.cursor();
        self.selector.set_decided_before(decided_before);
        let score = self.metric.score(&self.selector)?;
        let better = match self.best {
            Some(best_score) => score < best_score,
            None => true,
        };
        if better {
            self.best = Some(score);
            Some(score)
        } else {
            None
        }
    }

    fn is_promising(&self, bound: Option<Ordf32>) -> bool {
        match (bound, self.best) {
            (Some(bound), Some(best)) => best > bound,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    /// Bound the current node, telling the selector how much of the candidate order it can skip.
    ///
    /// Every candidate before `decided_before` has already been decided — included by an inclusion
    /// frame, or banned by an exclusion one — so a metric asking about undecided candidates never
    /// has to look at them. That is what keeps the cost of a node proportional to the answer rather
    /// than to the depth it was found at.
    fn bound_of_current(&mut self, decided_before: usize) -> Option<Ordf32> {
        self.selector.set_decided_before(decided_before);
        self.metric.bound(&self.selector)
    }

    fn bound_is_promising(&mut self, decided_before: usize) -> bool {
        let bound = self.bound_of_current(decided_before);
        self.is_promising(bound)
    }

    fn cursor(&self) -> usize {
        self.stack.last().map_or(0, |frame| frame.next_cursor)
    }

    /// The first undecided candidate at or after `start` in the candidate order, as
    /// `(index, cursor)`.
    fn next_candidate(&self, start: usize) -> Option<(usize, usize)> {
        for (cursor, (index, _)) in (start..).zip(self.selector.candidates_from(start)) {
            if !self.selector.is_selected(index) && !self.selector.banned().contains(index) {
                return Some((index, cursor));
            }
        }
        None
    }

    /// The candidates to ban when excluding `index`, and the cursor to resume from.
    ///
    /// For the exclusion branch, we keep banning candidates that are interchangeable with the one
    /// we exclude: same value and weight, and dragging in exactly the same unconfirmed ancestors.
    /// Two coins of equal value and weight are not interchangeable if one of them drags in an
    /// ancestor that needs bumping. Candidates are only compared until the first mismatch, since
    /// this exploits them being adjacent in the sorted order.
    fn exclusion_plan(&self, index: usize, cursor: usize) -> (Vec<usize>, usize) {
        let next = self.selector.candidate(index);
        let to_ban = (next.value, next.weight);
        let to_ban_drags_in = self.selector.problem().drags_in(index);
        let mut banned = alloc::vec![index];
        let mut next_cursor = cursor + 1;
        for (next_index, next) in self.selector.candidates_from(cursor + 1) {
            if self.selector.is_selected(next_index) || self.selector.banned().contains(next_index)
            {
                next_cursor += 1;
                continue;
            }
            if (next.value, next.weight) != to_ban
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

    fn apply_exclude(&mut self, banned: &[usize]) {
        for &index in banned {
            self.selector.ban(index);
        }
    }

    fn undo_exclude(&mut self, banned: &[usize]) {
        for &index in banned {
            self.selector.unban(index);
        }
    }

    fn push_include(&mut self, index: usize, cursor: usize, sibling_pending: bool) {
        self.selector.select(index);
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

    /// Step into the more promising child of the current node. Returns `false` if neither child
    /// can beat the incumbent (or there are no undecided candidates left).
    fn descend(&mut self) -> bool {
        let (index, cursor) = match self.next_candidate(self.cursor()) {
            Some(next) => next,
            None => return false,
        };

        self.selector.select(index);
        let inc_bound = self.bound_of_current(cursor + 1);
        let inc_ok = self.is_promising(inc_bound);
        self.selector.deselect(index);

        let (banned, exc_next_cursor) = self.exclusion_plan(index, cursor);
        self.apply_exclude(&banned);
        let exc_bound = self.bound_of_current(exc_next_cursor);
        let exc_ok = self.is_promising(exc_bound);
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
                // NOTE: We tiebreak equal bounds by preferring inclusion. We do this because we
                // want to try and get to evaluating complete selections as soon as possible.
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

    /// Unwind the path until a frame whose pending sibling can still beat the incumbent, and step
    /// into that sibling. Returns `false` once the whole tree is exhausted.
    ///
    /// The sibling's bound is recomputed here rather than reused from [`descend`](Self::descend):
    /// the incumbent may have improved since.
    fn backtrack_to_next_branch(&mut self) -> bool {
        while let Some(frame) = self.stack.pop() {
            // println!(
            //     "\t\t(BACK) undo {} [{}] sibling_pending={}",
            //     if frame.is_inclusion { "IN " } else { "EX " },
            //     frame.index,
            //     frame.sibling_pending,
            // );
            if frame.is_inclusion {
                self.selector.deselect(frame.index);
                if frame.sibling_pending {
                    let (banned, next_cursor) = self.exclusion_plan(frame.index, frame.cursor);
                    self.apply_exclude(&banned);
                    if self.bound_is_promising(next_cursor) {
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
                    self.selector.select(frame.index);
                    if self.bound_is_promising(frame.cursor + 1) {
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
                    self.selector.deselect(frame.index);
                }
            }
        }
        false
    }
}

/// A branch and bound metric where we minimize the [`Ordf32`] score.
///
/// This is to be used as input for [`CoinSelector::run_bnb`] or [`CoinSelector::bnb_solutions`].
///
/// Every selection passed to these methods carries its own [`Target`](crate::Target), reachable via
/// [`CoinSelector::target`]. [`bound`] is only a valid lower bound on the [`score`] of the
/// descendants of `cs` because the search clones `cs` to build them, so every node of a single
/// search is guaranteed to have the same target.
///
/// [`bound`]: BnbMetric::bound
/// [`score`]: BnbMetric::score
pub trait BnbMetric {
    /// Get the score of the selection `cs` against [`cs.target()`](CoinSelector::target).
    ///
    /// If this returns `None`, the selection is invalid.
    fn score(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf32>;

    /// Get the lower bound score, using a heuristic, against
    /// [`cs.target()`](CoinSelector::target).
    ///
    /// This represents the best possible score of all descendant branches (according to the
    /// heuristic).
    ///
    /// If this returns `None`, the current branch and all descendant branches will not have valid
    /// solutions.
    fn bound(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf32>;

    /// The change output (a.k.a. drain) this metric decides on for the selection `cs` and
    /// [`cs.target()`](CoinSelector::target), or [`Drain::NONE`] if it decides there should be no
    /// change.
    ///
    /// Call this on a branch-and-bound solution to get the change output the metric optimized against.
    fn drain(&mut self, cs: &CoinSelector<'_>) -> Drain;

    /// Returns whether the metric requies we order candidates by descending value per weight unit.
    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        false
    }
}
