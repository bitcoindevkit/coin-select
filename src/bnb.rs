//! Branch-and-bound search.
//!
//! BnB explores a binary tree where each [`Branch`] expands into two
//! children: an *inclusion* child that selects the candidate at the
//! branch's `cursor`, and an *exclusion* child that bans it along with
//! any same-(value, weight) duplicates that immediately follow.
//!
//! Cursors only advance as we descend — inclusion's child has cursor
//! `parent + 1`; exclusion's jumps past the banned duplicates. That
//! invariant lets `insert_new_branches` skip directly to the next
//! undecided candidate without re-scanning `selected` / `banned`. The
//! only scanning happens to step past pre-selected / pre-banned positions
//! (rare, lazy).

use core::cmp::Reverse;

use crate::{float::Ordf32, Drain, SelectionCache, SelectionView, Target};

use super::CoinSelector;
use alloc::collections::BinaryHeap;

/// An [`Iterator`] that iterates over rounds of branch and bound to minimize the score of the
/// provided [`BnbMetric`].
#[derive(Debug)]
pub(crate) struct BnbIter<'a, M: BnbMetric> {
    queue: BinaryHeap<Branch<'a>>,
    best: Option<Ordf32>,
    /// The target the metric scores selections against.
    pub(crate) target: Target,
    /// The `BnBMetric` that will score each selection
    pub(crate) metric: M,
}

impl<'a, M: BnbMetric> Iterator for BnbIter<'a, M> {
    type Item = Option<(CoinSelector<'a>, Ordf32)>;

    fn next(&mut self) -> Option<Self::Item> {
        // {
        //     println!("=========================== {:?}", self.best);
        //     for thing in self.queue.iter() {
        //         println!("{} {:?}", &thing.selector, thing.lower_bound);
        //     }
        //     let _ = std::io::stdin().read_line(&mut alloc::string::String::new());
        // }

        let branch = self.queue.pop()?;
        if let Some(best) = &self.best {
            // If the next thing in queue is not better than our best we're done.
            if *best < branch.lower_bound {
                // println!(
                //     "\t\t(SKIP) branch={} inclusion={} lb={:?}, score={:?}",
                //     branch.selector,
                //     !branch.is_exclusion,
                //     branch.lower_bound,
                //     self.metric.score(
                //         &SelectionView::with_cache(&branch.selector, &branch.cache),
                //         self.target,
                //     ),
                // );
                return None;
            }
        }
        // println!(
        //     "\t\t( POP) branch={} inclusion={} lb={:?}, score={:?}",
        //     branch.selector,
        //     !branch.is_exclusion,
        //     branch.lower_bound,
        //     self.metric.score(
        //         &SelectionView::with_cache(&branch.selector, &branch.cache),
        //         self.target,
        //     ),
        // );

        let Branch {
            selector,
            cache,
            is_exclusion,
            cursor,
            ..
        } = branch;

        let mut return_val = None;
        if !is_exclusion {
            let view = SelectionView::with_cache(&selector, &cache);
            if let Some(score) = self.metric.score(&view, self.target) {
                let better = match self.best {
                    Some(best_score) => score < best_score,
                    None => true,
                };
                if better {
                    self.best = Some(score);
                    return_val = Some(score);
                }
            };
        }

        self.insert_new_branches(&selector, &cache, cursor);
        Some(return_val.map(|score| (selector, score)))
    }
}

impl<'a, M: BnbMetric> BnbIter<'a, M> {
    pub(crate) fn new(mut selector: CoinSelector<'a>, target: Target, metric: M) -> Self {
        let mut iter = BnbIter {
            queue: BinaryHeap::default(),
            best: None,
            target,
            metric,
        };

        if iter.metric.requires_ordering_by_descending_value_pwu() {
            selector.sort_candidates_by_descending_value_pwu();
        }

        let cache = SelectionCache::from_selector(&selector);
        iter.consider_adding_to_queue(&selector, &cache, false, 0);

        iter
    }

    fn consider_adding_to_queue(
        &mut self,
        cs: &CoinSelector<'a>,
        cache: &SelectionCache,
        is_exclusion: bool,
        cursor: usize,
    ) {
        let bound = self
            .metric
            .bound(&SelectionView::with_cache(cs, cache), self.target);
        if let Some(bound) = bound {
            let is_good_enough = match self.best {
                Some(best) => best > bound,
                None => true,
            };
            if is_good_enough {
                self.queue.push(Branch {
                    lower_bound: bound,
                    selector: cs.clone(),
                    cache: cache.clone(),
                    is_exclusion,
                    cursor,
                });
            }
        }
    }

    fn insert_new_branches(&mut self, cs: &CoinSelector<'a>, cache: &SelectionCache, start: usize) {
        // Find the position to expand on: at or after `start`, whichever is
        // the first candidate that's neither selected nor banned. Usually
        // this *is* `start` — the only reason to advance is to skip past
        // pre-selected/pre-banned candidates (see module-level docs).
        let mut iter = cs.candidates().skip(start);
        let mut cursor = start;
        let (here_idx, here_cand) = loop {
            match iter.next() {
                None => return, // no more candidates — this branch is a leaf
                Some((idx, cand)) => {
                    if !cs.is_selected(idx) && !cs.banned().contains(idx) {
                        break (idx, cand);
                    }
                    cursor += 1;
                }
            }
        };
        // Past here, `iter` is positioned at `cursor + 1`.

        // Inclusion: descendants explore "this candidate is selected".
        let mut inclusion_cs = cs.clone();
        let mut inclusion_cache = cache.clone();
        inclusion_cs.select(here_idx);
        inclusion_cache.add(here_cand);
        self.consider_adding_to_queue(&inclusion_cs, &inclusion_cache, false, cursor + 1);

        // Exclusion: descendants explore "this candidate is *not* selected".
        // Bans this and every consecutive same-(value, weight) candidate —
        // they're equivalent choices, so we deduplicate by handling the
        // entire equivalence class in one branch. The cursor jumps past
        // all of them.
        let mut exclusion_cs = cs.clone();
        exclusion_cs.ban(here_idx);
        let equiv = (here_cand.value, here_cand.weight);
        let mut exclusion_cursor = cursor + 1;
        for (idx, cand) in iter {
            // Already-decided candidates (pre-selected or banned) must not be banned and must not
            // end the equivalence run: the pre-cursor version scanned `unselected()`, which skips
            // them entirely. The cursor may still advance past them — they're decided.
            if cs.is_selected(idx) || cs.banned().contains(idx) {
                exclusion_cursor += 1;
                continue;
            }
            if (cand.value, cand.weight) != equiv {
                break;
            }
            exclusion_cs.ban(idx);
            exclusion_cursor += 1;
        }
        self.consider_adding_to_queue(&exclusion_cs, cache, true, exclusion_cursor);
    }
}

#[derive(Debug, Clone)]
struct Branch<'a> {
    lower_bound: Ordf32,
    selector: CoinSelector<'a>,
    cache: SelectionCache,
    is_exclusion: bool,
    /// Position in `candidate_order` of the candidate whose include /
    /// exclude decision creates this branch's two children. See the
    /// module-level "Cursor invariant" section.
    cursor: usize,
}

impl Ord for Branch<'_> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // NOTE: Reverse comparision `lower_bound` because we want a min-heap (by default BinaryHeap
        // is a max-heap).
        // NOTE: We tiebreak equal scores based on whether it's exlusion or not (preferring
        // inclusion). We do this because we want to try and get to evaluating complete selection
        // returning actual scores as soon as possible.
        core::cmp::Ord::cmp(
            &(Reverse(&self.lower_bound), !self.is_exclusion),
            &(Reverse(&other.lower_bound), !other.is_exclusion),
        )
    }
}

impl PartialOrd for Branch<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Branch<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.lower_bound == other.lower_bound
    }
}

impl Eq for Branch<'_> {}

/// A branch and bound metric where we minimize the [`Ordf32`] score.
///
/// This is to be used as input for [`CoinSelector::run_bnb`] or [`CoinSelector::bnb_solutions`].
///
/// Both [`score`](Self::score) and [`bound`](Self::bound) receive a
/// [`SelectionView`]: a read-only handle over the [`CoinSelector`] whose
/// `&self` methods (`view.selected_value`, `view.input_weight`, `view.excess`,
/// `view.is_funded`, `view.drain`, ...) are O(1) because the underlying
/// running aggregates are maintained incrementally as BnB explores branches.
/// Use these methods rather than recomputing aggregates yourself.
pub trait BnbMetric {
    /// Get the score of a given selection for `target`.
    ///
    /// If this returns `None`, the selection is invalid.
    fn score(&mut self, view: &SelectionView<'_>, target: Target) -> Option<Ordf32>;

    /// Get the lower bound score using a heuristic for `target`.
    ///
    /// This represents the best possible score of all descendant branches (according to the
    /// heuristic).
    ///
    /// If this returns `None`, the current branch and all descendant branches will not have valid
    /// solutions.
    fn bound(&mut self, view: &SelectionView<'_>, target: Target) -> Option<Ordf32>;

    /// The change output (a.k.a. drain) this metric decides on for the given selection and `target`,
    /// or [`Drain::NONE`] if it decides there should be no change.
    ///
    /// Call this on a branch-and-bound solution to get the change output the metric optimized against.
    fn drain(&mut self, view: &SelectionView<'_>, target: Target) -> Drain;

    /// Returns whether the metric requies we order candidates by descending value per weight unit.
    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        false
    }
}
