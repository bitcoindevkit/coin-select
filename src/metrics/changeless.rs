use crate::{bnb::BnbMetric, float::Ordf32, CoinSelector, Drain, Target};

/// Constrains an `inner` metric to only changeless solutions.
///
/// A selection is scored by `inner` only if the inner metric decides it should *not* have a change
/// output (see [`BnbMetric::drain`]); otherwise it is treated as invalid. This lets you find, for
/// example, the lowest-fee changeless solution via `Changeless<LowestFee>`.
///
/// `target` must match the target `inner` is optimizing for. It's used only by the branch-pruning
/// heuristic (to tell which candidates reduce the excess); the change decision and the scoring are
/// delegated entirely to `inner`.
#[derive(Clone, Copy, Debug)]
pub struct Changeless<M> {
    /// The target of the resultant selection. Must match the target of `inner`.
    pub target: Target,
    /// The inner metric that scores changeless solutions and owns the change decision.
    pub inner: M,
}

impl<M: BnbMetric> Changeless<M> {
    /// Whether every selection reachable down this branch (the current one and any superset of it)
    /// would have a change output according to the inner metric — so no changeless solution exists
    /// here and the branch can be pruned.
    ///
    /// The inner metric only adds change once the excess is large enough (we assume its change
    /// decision is monotone in the excess). So the reachable selection least likely to have change
    /// is the one with the smallest excess — the current selection plus every remaining
    /// negative-effective-value candidate, since each of those lowers the excess. If even that
    /// selection still has change, then so does every reachable selection.
    ///
    /// NOTE: this relies on candidates being sorted so that all negative effective value candidates
    /// are next to each other, which [`requires_ordering_by_descending_value_pwu`] guarantees.
    ///
    /// [`requires_ordering_by_descending_value_pwu`]: BnbMetric::requires_ordering_by_descending_value_pwu
    fn change_unavoidable(&mut self, cs: &CoinSelector<'_>) -> bool {
        if self.inner.drain(cs).is_none() {
            return false;
        }

        let mut least_excess = cs.clone();
        cs.unselected()
            .rev()
            .take_while(|(_, wv)| wv.effective_value(self.target.fee.rate) < 0.0)
            .for_each(|(index, _)| {
                least_excess.select(index);
            });

        self.inner.drain(&least_excess).is_some()
    }
}

impl<M: BnbMetric> BnbMetric for Changeless<M> {
    fn drain(&mut self, _cs: &CoinSelector<'_>) -> Drain {
        // by definition a changeless selection never has a change output
        Drain::NONE
    }

    fn score(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf32> {
        // Reject selections that have change. We don't need an explicit target-met check: `inner`
        // returns `None` for invalid (e.g. not-target-met) selections.
        //
        // NOTE: for metrics whose `score` recomputes the drain (e.g. `LowestFee`), this evaluates
        // the drain decision twice per node. Sharing it would mean threading the drain into
        // `score`, which we avoid to keep metrics composable.
        if self.inner.drain(cs).is_some() {
            return None;
        }
        self.inner.score(cs)
    }

    fn bound(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf32> {
        if self.change_unavoidable(cs) {
            // every descendant has change, so no changeless solution is reachable
            None
        } else {
            // the changeless-constrained optimum is no better than the inner metric's unconstrained
            // optimum, so the inner bound is a valid lower bound
            self.inner.bound(cs)
        }
    }

    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        true
    }
}
