use crate::{varint_size, DrainWeights, FeeRate};

/// A target value to select for along with feerate constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub struct Target {
    /// The fee constraints that must be satisfied by the selection
    pub fee: TargetFee,
    /// The aggregate properties of outputs you're trying to fund
    pub outputs: TargetOutputs,
}

impl Target {
    /// The value target that we are trying to fund
    pub fn value(&self) -> u64 {
        self.outputs.value_sum
    }
}

/// Information about the outputs we're trying to fund. Note the fields are total values since we
/// don't care about the weights or the values of individual outputs for the purposes of coin
/// selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
pub struct TargetOutputs {
    /// The sum of the values of the individual `TxOuts`s.
    pub value_sum: u64,
    /// The sum of the weights of the individual `TxOut`s.
    pub weight_sum: u32,
    /// The total number of outputs
    pub n_outputs: usize,
}

impl TargetOutputs {
    /// The output weight of the outptus we're trying to fund
    pub fn output_weight(&self) -> u32 {
        self.weight_sum + varint_size(self.n_outputs) * 4
    }

    /// The output weight of the target's outputs combined with the drain outputs defined by
    /// `drain_weight`.
    ///
    /// This is not a simple addition of the `drain_weight` and [`output_weight`] because of how
    /// adding the drain weights might add an extra vbyte for the length of the varint.
    ///
    /// [`output_weight`]: Self::output_weight
    pub fn output_weight_with_drain(&self, drain_weight: DrainWeights) -> u32 {
        let n_outputs = drain_weight.n_outputs + self.n_outputs;
        varint_size(n_outputs) * 4 + drain_weight.output_weight + self.weight_sum
    }

    /// Creates a `TargetOutputs` from a list of outputs represented as `(weight, value)` pairs.
    pub fn fund_outputs(outputs: impl IntoIterator<Item = (u32, u64)>) -> Self {
        let mut n_outputs = 0;
        let mut weight_sum = 0;
        let mut value_sum = 0;

        for (weight, value) in outputs {
            n_outputs += 1;
            weight_sum += weight;
            value_sum += value;
        }
        Self {
            n_outputs,
            weight_sum,
            value_sum,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
/// The fee constraints of a coin selection.
///
/// There are two orthogonal constraints:
///
/// - `rate`: The feerate of the transaction must at least be this high. You set this to control how
/// quickly your transaction is confirmed. Typically a coin selection will try and hit this target
/// exactly but it might go over if the `replace` constraint takes precedence or if the
/// [`ChangePolicy`] determines that the excess value should just be given to miners (rather than
/// create a change output).
/// - `replace`: The selection must have a high enough fee to satisfy [RBF rule 4]
///
/// [RBF rule 4]: https://github.com/bitcoin/bitcoin/blob/master/doc/policy/mempool-replacements.md#current-replace-by-fee-policy
/// [`ChangePolicy`]: crate::ChangePolicy
pub struct TargetFee {
    /// The feerate the transaction must have
    pub rate: FeeRate,
    /// The fee must enough enough to replace this
    pub replace: Option<Replace>,
}

impl Default for TargetFee {
    /// The default is feerate set is [`FeeRate::DEFAULT_MIN_RELAY`] and doesn't replace anything.
    fn default() -> Self {
        Self {
            rate: FeeRate::DEFAULT_MIN_RELAY,
            replace: None,
        }
    }
}

impl TargetFee {
    /// A target fee of 0 sats per vbyte (and no replacement)
    pub const ZERO: Self = TargetFee {
        rate: FeeRate::ZERO,
        replace: None,
    };

    /// Creates a target fee from a feerate. The target won't include a replacement.
    pub fn from_feerate(feerate: FeeRate) -> Self {
        Self {
            rate: feerate,
            replace: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd)]
/// The weight transaction(s) that this new transaction is replacing including the feerate.
pub struct Replace {
    /// The fee of the transaction being replaced paid
    pub fee: u64,
    /// The incrememental relay feerate (by default 1 sat per vbyte).
    pub incremental_relay_feerate: FeeRate,
}

impl Replace {
    /// Replace transaction(s) that paid `tx_fee` in fees assuming the default *incremental relay feerate*.
    pub fn new(tx_fee: u64) -> Self {
        Self {
            fee: tx_fee,
            incremental_relay_feerate: FeeRate::DEFUALT_RBF_INCREMENTAL_RELAY,
        }
    }

    /// The minimum fee for the transaction with weight `replacing_tx_weight` that wants to do the replacement.
    /// This is defined by [RBF rule 4].
    ///
    /// [RBF rule 4]: https://github.com/bitcoin/bitcoin/blob/master/doc/policy/mempool-replacements.md#current-replace-by-fee-policy
    pub fn min_fee_to_do_replacement(&self, replacing_tx_weight: u32) -> u64 {
        let min_fee_increment =
            (replacing_tx_weight as f32 * self.incremental_relay_feerate.spwu()).ceil() as u64;
        self.fee + min_fee_increment
    }
}
