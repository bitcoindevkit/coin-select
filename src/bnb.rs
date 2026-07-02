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

        self.insert_new_branches(&selector, &cache);
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
        iter.consider_adding_to_queue(&selector, &cache, false);

        iter
    }

    fn consider_adding_to_queue(
        &mut self,
        cs: &CoinSelector<'a>,
        cache: &SelectionCache,
        is_exclusion: bool,
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
                });
            }
        }
    }

    fn insert_new_branches(&mut self, cs: &CoinSelector<'a>, cache: &SelectionCache) {
        let (next_index, next) = match cs.unselected().next() {
            Some(c) => c,
            None => return, // exhausted
        };

        // Inclusion branch: selecting `next_index` requires updating the cache.
        let mut inclusion_cs = cs.clone();
        let mut inclusion_cache = cache.clone();
        inclusion_cs.select(next_index);
        inclusion_cache.add(next);
        self.consider_adding_to_queue(&inclusion_cs, &inclusion_cache, false);

        // Exclusion branch: only bans, no selection change → cache unchanged.
        let mut exclusion_cs = cs.clone();
        let to_ban = (next.value, next.weight);
        for (ban_index, ban_cand) in cs.unselected() {
            if (ban_cand.value, ban_cand.weight) != to_ban {
                break;
            }
            exclusion_cs.ban(ban_index);
        }
        self.consider_adding_to_queue(&exclusion_cs, cache, true);
    }
}

#[derive(Debug, Clone)]
struct Branch<'a> {
    lower_bound: Ordf32,
    selector: CoinSelector<'a>,
    cache: SelectionCache,
    is_exclusion: bool,
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
