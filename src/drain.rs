#[allow(unused)] // some bug in <= 1.48.0 sees this as unused when it isn't
use crate::float::FloatExt;
use crate::{varint_size, FeeRate, TR_KEYSPEND_TXIN_WEIGHT, TR_SPK_WEIGHT, TXOUT_BASE_WEIGHT};

/// Represents the weight costs of a drain (a.k.a. change) output.
///
/// May also represent multiple outputs.
#[derive(Default, Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct DrainWeights {
    /// The weight of including this drain output.
    ///
    /// This must take into account the weight change from varint output count.
    pub output_weight: u32,
    /// The weight of spending this drain output (in the future).
    pub spend_weight: u32,
    /// The total number of outputs that the drain will use
    pub n_outputs: usize,
}

impl DrainWeights {
    /// `DrainWeights` for an output that will be spent with a taproot keyspend
    pub const TR_KEYSPEND: Self = Self {
        output_weight: TXOUT_BASE_WEIGHT + TR_SPK_WEIGHT,
        spend_weight: TR_KEYSPEND_TXIN_WEIGHT,
        n_outputs: 1,
    };

    /// `DrainWeights` for no drain at all
    pub const NONE: Self = Self {
        output_weight: 0,
        spend_weight: 0,
        n_outputs: 0,
    };

    /// The waste of adding this drain to a transaction according to the [waste metric].
    ///
    /// To get the precise answer you need to pass in the number of non-drain outputs (`n_target_outputs`) that you're
    /// adding to the transaction so we can include the cost of increasing the varint size of the output length.
    ///
    /// [waste metric]: https://bitcoin.stackexchange.com/questions/113622/what-does-waste-metric-mean-in-the-context-of-coin-selection
    pub fn waste(
        &self,
        feerate: FeeRate,
        long_term_feerate: FeeRate,
        n_target_outputs: usize,
    ) -> f32 {
        let extra_varint_weight =
            (varint_size(n_target_outputs + self.n_outputs) - varint_size(n_target_outputs)) * 4;
        let extra_output_weight = self.output_weight + extra_varint_weight;
        extra_output_weight as f32 * feerate.spwu()
            + self.spend_weight as f32 * long_term_feerate.spwu()
    }

    /// The fee you will pay to spend these change output(s) in the future.
    pub fn spend_fee(&self, long_term_feerate: FeeRate) -> u64 {
        (self.spend_weight as f32 * long_term_feerate.spwu()).ceil() as u64
    }
}

/// A drain (A.K.A. change) output.
/// Technically it could represent multiple outputs.
///
/// This is returned from [`CoinSelector::drain`]. Note if `drain` returns a drain where `is_none()`
/// returns true then **no change should be added** to the transaction.
///
/// [`CoinSelector::drain`]: crate::CoinSelector::drain
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Drain {
    /// Weight of adding drain output and spending the drain output.
    pub weights: DrainWeights,
    /// The value that should be assigned to the drain.
    pub value: u64,
}

impl Drain {
    /// The drain which represents no drain at all. We could but don't use `Option` because this
    /// causes friction internally, instead we just use a `Drain` with all 0 values.
    pub const NONE: Self = Drain {
        weights: DrainWeights::NONE,
        value: 0,
    };

    /// is the "none" drain
    pub fn is_none(&self) -> bool {
        self == &Drain::NONE
    }

    /// Is not the "none" drain
    pub fn is_some(&self) -> bool {
        !self.is_none()
    }
}

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
            .waste(
                target_feerate,
                long_term_feerate,
                0, /* ignore varint cost for now */
            )
            .ceil() as u64;

        Self {
            drain_weights,
            min_value: waste_with_change.max(min_value),
        }
    }
}
