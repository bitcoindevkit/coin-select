/// Context for package-aware coin selection (CPFP scenarios).
///
/// When a transaction has unconfirmed parents, miners evaluate the *package feerate* rather than
/// the child's feerate alone. This struct captures the aggregate fee and weight of all parent
/// transactions so that coin selection can target a feerate that makes the entire package
/// attractive to miners.
///
/// The package feerate is calculated as:
/// ```text
/// package_feerate = (parent_fee + child_fee) / (parent_weight + child_weight)
/// ```
///
/// Use [`CoinSelector::with_package`] to create a package-aware coin selector.
///
/// [`CoinSelector::with_package`]: crate::CoinSelector::with_package
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Package {
    /// Total fees already paid by all parent transactions (in satoshis).
    pub parent_fee: u64,
    /// Total weight of all parent transactions (in weight units).
    pub parent_weight: u64,
}
