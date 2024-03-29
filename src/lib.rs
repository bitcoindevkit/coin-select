#![doc = include_str!("../README.md")]
#![no_std]
#![warn(missing_docs)]
#![deny(unsafe_code)]

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

#[cfg(feature = "std")]
#[macro_use]
extern crate std;

mod coin_selector;
pub mod float;
pub use coin_selector::*;

mod bnb;
pub use bnb::*;

pub mod metrics;

mod feerate;
pub use feerate::*;
mod target;
pub use target::*;
mod drain;
pub use drain::*;

/// Txin "base" fields include `outpoint` (32+4) and `nSequence` (4) and 1 byte for the scriptSig
/// length.
pub const TXIN_BASE_WEIGHT: u32 = (32 + 4 + 4 + 1) * 4;

/// The weight of a TXOUT with a zero length `scriptPubKey`
#[allow(clippy::identity_op)]
pub const TXOUT_BASE_WEIGHT: u32 =
    // The value
    4 * core::mem::size_of::<u64>() as u32
    // The spk length
    + (4 * 1);

/// The weight of the `nVersion` and `nLockTime` transaction fields
pub const TX_FIXED_FIELD_WEIGHT: u32 = (4 /* nVersion */ + 4/* nLockTime */) * 4;

/// The weight of a taproot keyspend witness
pub const TR_KEYSPEND_SATISFACTION_WEIGHT: u32 = 1 /*witness_len*/ + 1 /*item len*/ +  64 /*signature*/;

/// The weight of a segwit `v1` (taproot) script pubkey in an output. This does not include the weight of
/// the `TxOut` itself or the script pubkey length field.
pub const TR_SPK_WEIGHT: u32 = (1 + 1 + 32) * 4; // version + push + key

/// The weight of a taproot TxIn with witness
pub const TR_KEYSPEND_TXIN_WEIGHT: u32 = TXIN_BASE_WEIGHT + TR_KEYSPEND_SATISFACTION_WEIGHT;

/// The minimum value a taproot output can have to be relayed with Bitcoin core's default dust relay
/// fee
pub const TR_DUST_RELAY_MIN_VALUE: u64 = 330;

/// Helper to calculate varint size. `v` is the value the varint represents.
const fn varint_size(v: usize) -> u32 {
    if v <= 0xfc {
        return 1;
    }
    if v <= 0xffff {
        return 3;
    }
    if v <= 0xffff_ffff {
        return 5;
    }
    9
}

#[allow(unused)]
fn txout_weight_from_spk_len(spk_len: usize) -> u32 {
    (TXOUT_BASE_WEIGHT + varint_size(spk_len) + (spk_len as u32)) * 4
}
