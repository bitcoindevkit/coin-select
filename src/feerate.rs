use crate::float::Ordf32;
use core::ops::{Add, Sub};

/// Fee rate
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
// Internally stored as satoshi/weight unit
pub struct FeeRate(Ordf32);

impl FeeRate {
    /// A feerate of zero
    pub const ZERO: Self = Self(Ordf32(0.0));
    /// The default minimum relay fee that bitcoin core uses (1 sat per vbyte). The feerate your transaction has must
    /// be at least this to be forwarded by most nodes on the network.
    pub const DEFAULT_MIN_RELAY: Self = Self(Ordf32(0.25));
    /// The defualt incremental relay fee that bitcoin core uses (1 sat per vbyte). You must pay
    /// this fee over the fee of the transaction(s) you are replacing by through the replace-by-fee
    /// mechanism. This feerate is applied to the transaction that is replacing the old
    /// transactions.
    pub const DEFUALT_RBF_INCREMENTAL_RELAY: Self = Self(Ordf32(0.25));
    /// Create a new instance checking the value provided
    ///
    /// ## Panics
    ///
    /// Panics if the value is not [normal](https://doc.rust-lang.org/std/primitive.f32.html#method.is_normal) (except if it's a positive zero) or negative.
    fn new_checked(value: f32) -> Self {
        assert!(value.is_normal() || value == 0.0);
        assert!(value.is_sign_positive());

        Self(Ordf32(value))
    }

    /// Create a new instance of [`FeeRate`] given a float fee rate in btc/kvbytes
    ///
    /// ## Panics
    ///
    /// Panics if the value is not [normal](https://doc.rust-lang.org/std/primitive.f32.html#method.is_normal) (except if it's a positive zero) or negative.
    pub fn from_btc_per_kvb(btc_per_kvb: f32) -> Self {
        Self::new_checked(btc_per_kvb * 1e5 / 4.0)
    }

    /// Create a new instance of [`FeeRate`] given a float fee rate in satoshi/vbyte
    ///
    /// ## Panics
    ///
    /// Panics if the value is not [normal](https://doc.rust-lang.org/std/primitive.f32.html#method.is_normal) (except if it's a positive zero) or negative.
    pub fn from_sat_per_vb(sat_per_vb: f32) -> Self {
        Self::new_checked(sat_per_vb / 4.0)
    }

    /// Create a new [`FeeRate`] with the default min relay fee value
    #[deprecated(note = "use the DEFAULT_MIN_RELAY constant instead")]
    pub const fn default_min_relay_fee() -> Self {
        Self(Ordf32(0.25))
    }

    /// Calculate fee rate from `fee` and weight units (`wu`).
    pub fn from_wu(fee: u64, wu: usize) -> Self {
        Self::from_sat_per_wu(fee as f32 / wu as f32)
    }

    /// Calculate feerate from `satoshi/wu`.
    pub fn from_sat_per_wu(sats_per_wu: f32) -> Self {
        Self::new_checked(sats_per_wu)
    }

    /// Calculate fee rate from `fee` and `vbytes`.
    pub fn from_vb(fee: u64, vbytes: usize) -> Self {
        let rate = fee as f32 / vbytes as f32;
        Self::from_sat_per_vb(rate)
    }

    /// Return the value as satoshi/vbyte.
    pub fn as_sat_vb(&self) -> f32 {
        self.0 .0 * 4.0
    }

    /// Return the value as satoshi/wu.
    pub fn spwu(&self) -> f32 {
        self.0 .0
    }

    /// The fee that the transaction with weight `tx_weight` should pay in order to satisfy the fee rate given by `self`,
    /// where the fee rate is applied to the rounded-up vbytes obtained from `tx_weight`.
    pub fn implied_fee(&self, tx_weight: u64) -> u64 {
        ((tx_weight as f32 / 4.0).ceil() * self.as_sat_vb()).ceil() as u64
    }

    /// Same as [implied_fee](Self::implied_fee) except the fee rate given by `self` is applied to `tx_weight` directly.
    pub fn implied_fee_wu(&self, tx_weight: u64) -> u64 {
        (tx_weight as f32 * self.spwu()).ceil() as u64
    }
}

impl Add<FeeRate> for FeeRate {
    type Output = Self;

    fn add(self, rhs: FeeRate) -> Self::Output {
        Self(Ordf32(self.0 .0 + rhs.0 .0))
    }
}

impl Sub<FeeRate> for FeeRate {
    type Output = Self;

    fn sub(self, rhs: FeeRate) -> Self::Output {
        Self(Ordf32(self.0 .0 - rhs.0 .0))
    }
}
