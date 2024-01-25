use super::change_lower_bound;
use crate::{bnb::BnbMetric, change_policy::ChangePolicy, float::Ordf32, CoinSelector, Target};

#[derive(Clone, Debug)]
/// Metric for finding changeless solutions only.
pub struct Changeless {
    /// The target parameters for the resultant selection.
    pub target: Target,
    /// Policy to determine whether a selection requires a change output.
    pub change_policy: ChangePolicy,
}

impl BnbMetric for Changeless {
    fn score(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf32> {
        if cs.is_target_met(self.target)
            && cs.drain_value(self.target, self.change_policy).is_none()
        {
            Some(Ordf32(0.0))
        } else {
            None
        }
    }

    fn bound(&mut self, cs: &CoinSelector<'_>) -> Option<Ordf32> {
        if change_lower_bound(cs, self.target, self.change_policy).is_some() {
            None
        } else {
            Some(Ordf32(0.0))
        }
    }

    fn requires_ordering_by_descending_value_pwu(&self) -> bool {
        true
    }
}
