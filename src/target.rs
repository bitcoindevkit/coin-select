use crate::FeeRate;

/// A target value to select for along with feerate constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Default)]
pub struct Target {
    /// The fee constraints that must be satisfied by the selection
    pub fee: TargetFee,
    /// The minmum value that should be left for the output
    pub value: u64,
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
