use crate::{bnb::BnbMetric, float::Ordf32, CoinSelector, Drain, Target};

/// Constrains an `inner` metric to only changeless solutions.
///
/// A selection is scored by `inner` only if the inner metric decides it should *not* have a change
/// output (see [`BnbMetric::drain`]); otherwise it is treated as invalid. This lets you find, for
/// example, the lowest-fee changeless solution via `Changeless<LowestFee>`.
#[derive(Clone, Copy, Debug)]
pub struct Changeless<M>(
    /// The inner metric that scores changeless solutions and owns the change decision.
    pub M,
);

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
    fn change_unavoidable(&mut self, cs: &CoinSelector<'_>, target: Target) -> bool {
        if self.0.drain(cs, target).is_none() {
            return false;
        }

        let mut least_excess = cs.clone();
        cs.unselected()
            .rev()
            .take_while(|(_, wv)| wv.effective_value(target.fee.rate) < 0.0)
            .for_each(|(index, _)| {
                least_excess.select(index);
            });

        self.0.drain(&least_excess, target).is_some()
    }
}

impl<M: BnbMetric> BnbMetric for Changeless<M> {
    fn drain(&mut self, _cs: &CoinSelector<'_>, _target: Target) -> Drain {
        // by definition a changeless selection never has a change output
        Drain::NONE
    }

    fn score(&mut self, cs: &CoinSelector<'_>, target: Target) -> Option<Ordf32> {
        // Reject selections that have change. We don't need an explicit target-met check: `inner`
        // returns `None` for invalid (e.g. not-target-met) selections.
        //
        // NOTE: for metrics whose `score` recomputes the drain (e.g. `LowestFee`), this evaluates
        // the drain decision twice per node. Sharing it would mean threading the drain into
        // `score`, which we avoid to keep metrics composable.
        if self.0.drain(cs, target).is_some() {
            return None;
        }
        self.0.score(cs, target)
    }

    fn bound(&mut self, cs: &CoinSelector<'_>, target: Target) -> Option<Ordf32> {
        if self.change_unavoidable(cs, target) {
            // every descendant has change, so no changeless solution is reachable
            None
        } else {
            // the changeless-constrained optimum is no better than the inner metric's unconstrained
            // optimum, so the inner bound is a valid lower bound
            self.0.bound(cs, target)
        }
    }

    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        true
    }
}
