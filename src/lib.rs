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

mod bitset;
pub use bitset::*;
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
mod selection_problem;
pub use selection_problem::*;

/// The weight of an unsatisfied txin: the non-discounted `prevout` (32+4), `nSequence` (4) and
/// empty `scriptSig` length (1), plus the discounted empty `scriptWitness` stack item count (1).
pub const TXIN_BASE_WEIGHT: u64 =
    (32 + 4 /* prevout */ + 4 /* nSequence */ + 1 /* scriptSig length */) * 4 + 1 /* stack item count */;

/// The weight of a TXOUT with a zero length `scriptPubKey`
#[allow(clippy::identity_op)]
pub const TXOUT_BASE_WEIGHT: u64 =
    // The value
    4 * core::mem::size_of::<u64>() as u64
    // The spk length
    + (4 * 1);

/// The weight of the non-discounted `nVersion`, `nLockTime` fields and the discounted segwit
/// `marker` and `flag`.
pub const TX_FIXED_FIELD_WEIGHT: u64 = (4 /* nVersion */ + 4/* nLockTime */) * 4 + 2;

/// The weight of a taproot keyspend `scriptWitness`, excluding the stack item count that
/// [`TXIN_BASE_WEIGHT`] covers.
///
/// This assumes a 64-byte `SIGHASH_DEFAULT` signature. miniscript's `max_weight_to_satisfy` assumes
/// the worst case, a 65-byte signature with an explicit sighash byte, and so returns 1 WU more.
pub const TR_KEYSPEND_SATISFACTION_WEIGHT: u64 = 1 /* stack item length */ + 64 /* signature */;

/// The weight of a segwit `v1` (taproot) script pubkey in an output. This does not include the weight of
/// the `TxOut` itself or the script pubkey length field.
pub const TR_SPK_WEIGHT: u64 = (1 + 1 + 32) * 4; // version + push + key

/// The weight of a taproot TxIn with witness
pub const TR_KEYSPEND_TXIN_WEIGHT: u64 = TXIN_BASE_WEIGHT + TR_KEYSPEND_SATISFACTION_WEIGHT;

/// The minimum value a taproot output can have to be relayed with Bitcoin core's default dust relay
/// fee
pub const TR_DUST_RELAY_MIN_VALUE: u64 = 330;

/// Helper to calculate varint size. `v` is the value the varint represents.
const fn varint_size(v: usize) -> u64 {
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
fn txout_weight_from_spk_len(spk_len: usize) -> u64 {
    (TXOUT_BASE_WEIGHT + varint_size(spk_len) + (spk_len as u64)) * 4
}
