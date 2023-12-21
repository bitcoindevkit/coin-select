//! This module contains a collection of change policies.
//!
//! A change policy determines whether a given coin selection (presented by [`CoinSelector`]) should
//! construct a transaction with a change output. A change policy is represented as a function of
//! type `Fn(&CoinSelector, Target) -> Drain`.

#[allow(unused)] // some bug in <= 1.48.0 sees this as unused when it isn't
use crate::float::FloatExt;
use crate::{DrainWeights, FeeRate};

/// Describes when a change output (although it could represent several) should be added that drains
/// the excess in the coin selection. It includes the `drain_weights` to account for the cost of
/// adding this outupt(s).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChangePolicy {
    /// The minimum amount of excesss there needs to be add a change output.
    pub min_value: u64,
    /// The weights of the drain that would be added according to the policy.
    pub drain_weights: DrainWeights,
}

impl ChangePolicy {
    /// Construct a change policy that creates change when the change value is greater than
    /// `min_value`.
    pub fn min_value(drain_weights: DrainWeights, min_value: u64) -> Self {
        Self {
            drain_weights,
            min_value,
        }
    }

    /// Construct a change policy that creates change when it would reduce the transaction waste
    /// given that `min_value` is respected.
    pub fn min_value_and_waste(
        drain_weights: DrainWeights,
        min_value: u64,
        target_feerate: FeeRate,
        long_term_feerate: FeeRate,
    ) -> Self {
        // The output waste of a changeless solution is the excess.
        let waste_with_change = drain_weights
            .waste(target_feerate, long_term_feerate)
            .ceil() as u64;

        Self {
            drain_weights,
            min_value: waste_with_change.max(min_value),
        }
    }
}
