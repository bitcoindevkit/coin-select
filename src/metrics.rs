//! Branch and bound metrics that can be passed to [`CoinSelector::bnb_solutions`] or
//! [`CoinSelector::run_bnb`].
//!
//! [`CoinSelector::bnb_solutions`]: crate::CoinSelector::bnb_solutions
//! [`CoinSelector::run_bnb`]: crate::CoinSelector::run_bnb
mod lowest_fee;
pub use lowest_fee::*;
mod changeless;
pub use changeless::*;
