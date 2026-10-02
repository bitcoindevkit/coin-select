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

/// A decision on the current path: either `candidate_index` was selected, or `banned`
/// (`candidate_index` and the candidates interchangeable with it) were banned.
#[derive(Debug)]
struct Frame {
    is_inclusion: bool,
    candidate_index: usize,
    /// Position of `candidate_index` in the candidate order.
    order_pos: usize,
    /// Position in the candidate order where the search for the next undecided candidate starts,
    /// below this frame.
    order_pos_scan_from: usize,
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
        //     println!("{} {:?}", &self.selector, self.metric.bound(&self.selector));
        //     for frame in self.stack.iter() {
        //         println!(
        //             "\t{} [{}] order_pos={} sibling_pending={}",
        //             if frame.is_inclusion { "IN " } else { "EX " },
        //             frame.candidate_index,
        //             frame.order_pos,
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

        if !iter.bound_is_promising() {
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

    fn bound_is_promising(&mut self) -> bool {
        let bound = self.metric.bound(&self.selector);
        self.is_promising(bound)
    }

    fn order_pos_scan_from(&self) -> usize {
        self.stack
            .last()
            .map_or(0, |frame| frame.order_pos_scan_from)
    }

    /// The first undecided candidate at or after `start` in the candidate order, as
    /// `(candidate_index, order_pos)`.
    fn next_candidate(&self, start: usize) -> Option<(usize, usize)> {
        for (order_pos, (candidate_index, _)) in
            (start..).zip(self.selector.candidates().skip(start))
        {
            if !self.selector.is_selected(candidate_index)
                && !self.selector.banned().contains(candidate_index)
            {
                return Some((candidate_index, order_pos));
            }
        }
        None
    }

    /// The candidates to ban when excluding `candidate_index`, and the position to scan from next.
    ///
    /// For the exclusion branch, we keep banning candidates identical to the one we exclude, since
    /// selecting any one of them is equivalent to selecting another. Candidates are only compared
    /// until the first mismatch, since this relies on the sort placing identical candidates next to
    /// each other.
    fn exclusion_plan(&self, candidate_index: usize, order_pos: usize) -> (Vec<usize>, usize) {
        let next = self.selector.candidate(candidate_index);
        let to_ban = next;
        let mut banned = alloc::vec![candidate_index];
        let mut order_pos_scan_from = order_pos + 1;
        for (next_index, next) in self.selector.candidates().skip(order_pos + 1) {
            if self.selector.is_selected(next_index) || self.selector.banned().contains(next_index)
            {
                order_pos_scan_from += 1;
                continue;
            }
            if next != to_ban {
                break;
            }
            // println!("banning: [{}] {:?}", next_index, next);
            banned.push(next_index);
            order_pos_scan_from += 1;
        }
        (banned, order_pos_scan_from)
    }

    fn apply_exclude(&mut self, banned: &[usize]) {
        for &candidate_index in banned {
            self.selector.ban(candidate_index);
        }
    }

    fn undo_exclude(&mut self, banned: &[usize]) {
        for &candidate_index in banned {
            self.selector.unban(candidate_index);
        }
    }

    fn push_include(&mut self, candidate_index: usize, order_pos: usize, sibling_pending: bool) {
        self.selector.select(candidate_index);
        self.stack.push(Frame {
            is_inclusion: true,
            candidate_index,
            order_pos,
            order_pos_scan_from: order_pos + 1,
            banned: Vec::new(),
            sibling_pending,
        });
    }

    fn push_exclude(
        &mut self,
        candidate_index: usize,
        order_pos: usize,
        banned: Vec<usize>,
        order_pos_scan_from: usize,
        sibling_pending: bool,
    ) {
        self.apply_exclude(&banned);
        self.stack.push(Frame {
            is_inclusion: false,
            candidate_index,
            order_pos,
            order_pos_scan_from,
            banned,
            sibling_pending,
        });
    }

    /// Step into the more promising child of the current node. Returns `false` if neither child
    /// can beat the incumbent (or there are no undecided candidates left).
    fn descend(&mut self) -> bool {
        let (candidate_index, order_pos) = match self.next_candidate(self.order_pos_scan_from()) {
            Some(next) => next,
            None => return false,
        };

        self.selector.select(candidate_index);
        let inc_bound = self.metric.bound(&self.selector);
        let inc_ok = self.is_promising(inc_bound);
        self.selector.deselect(candidate_index);

        let (banned, exc_order_pos_scan_from) = self.exclusion_plan(candidate_index, order_pos);
        self.apply_exclude(&banned);
        let exc_bound = self.metric.bound(&self.selector);
        let exc_ok = self.is_promising(exc_bound);
        self.undo_exclude(&banned);

        // println!(
        //     "\t\t(DESC) branch={} next=[{}] inc_lb={:?}{} exc_lb={:?}{}",
        //     self.selector,
        //     candidate_index,
        //     inc_bound,
        //     if inc_ok { "" } else { " (REJ)" },
        //     exc_bound,
        //     if exc_ok { "" } else { " (REJ)" },
        // );

        match (inc_ok, exc_ok) {
            (false, false) => false,
            (true, false) => {
                self.push_include(candidate_index, order_pos, false);
                true
            }
            (false, true) => {
                self.push_exclude(
                    candidate_index,
                    order_pos,
                    banned,
                    exc_order_pos_scan_from,
                    false,
                );
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
                    self.push_include(candidate_index, order_pos, true);
                } else {
                    self.push_exclude(
                        candidate_index,
                        order_pos,
                        banned,
                        exc_order_pos_scan_from,
                        true,
                    );
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
            //     frame.candidate_index,
            //     frame.sibling_pending,
            // );
            if frame.is_inclusion {
                self.selector.deselect(frame.candidate_index);
                if frame.sibling_pending {
                    let (banned, order_pos_scan_from) =
                        self.exclusion_plan(frame.candidate_index, frame.order_pos);
                    self.apply_exclude(&banned);
                    if self.bound_is_promising() {
                        self.stack.push(Frame {
                            is_inclusion: false,
                            candidate_index: frame.candidate_index,
                            order_pos: frame.order_pos,
                            order_pos_scan_from,
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
                    self.selector.select(frame.candidate_index);
                    if self.bound_is_promising() {
                        self.stack.push(Frame {
                            is_inclusion: true,
                            candidate_index: frame.candidate_index,
                            order_pos: frame.order_pos,
                            order_pos_scan_from: frame.order_pos + 1,
                            banned: Vec::new(),
                            sibling_pending: false,
                        });
                        return true;
                    }
                    self.selector.deselect(frame.candidate_index);
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
