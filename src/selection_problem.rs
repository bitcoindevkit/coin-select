use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::bitset::Bitset;
use crate::{Candidate, CoinSelector, FeeRate, Target};

/// An unconfirmed ancestor that may need bumping to the target feerate (CPFP).
///
/// `Txid` is whatever the caller keys transactions by. This crate has no `bitcoin` dependency.
#[derive(Debug, Clone)]
pub struct AncestorToBump<Txid> {
    /// Caller-chosen id for this transaction.
    pub txid: Txid,
    /// Weight of this transaction in weight units.
    pub weight: u64,
    /// Fee this transaction already pays, in satoshis.
    pub fee: u64,
    /// Direct parents only; transitive ancestors are derived when building a [`SelectionProblem`].
    pub parents: Vec<Txid>,
}

/// One or more UTXOs that must be spent together, described on their own terms.
///
/// Everything here is intrinsic to the coins. Hand these to [`SelectionProblem::new`], which pairs
/// each group with the ancestors it drags in.
pub type InputGroup<Txid> = Vec<Input<Txid>>;

/// A single UTXO, before it is folded into a [`Candidate`].
#[derive(Debug, Clone, Copy)]
pub struct Input<Txid> {
    /// Value of the UTXO in satoshis.
    pub value: u64,
    /// Input weight as for [`Candidate::weight`] (legacy inputs omit the empty-witness byte).
    pub weight: u64,
    /// Whether this input is segwit.
    pub is_segwit: bool,
    /// Transaction that created this UTXO (may be unconfirmed).
    pub residing_txid: Txid,
}

impl<Txid> From<Input<Txid>> for InputGroup<Txid> {
    fn from(input: Input<Txid>) -> Self {
        alloc::vec![input]
    }
}

/// Target, candidates, and (optional) ancestor-bump data for one coin-selection run.
///
/// Build with [`SelectionProblem::new_no_ancestors`] when nothing is unconfirmed, or
/// [`SelectionProblem::new`] when spending unconfirmed UTXOs. Pass a reference to
/// [`CoinSelector::new`].
///
/// Ancestor bump figures are stored here (not on [`Candidate`]) so candidates stay a plain
/// description of inputs. Every unconfirmed transaction that created an input, and all of its
/// transitive unconfirmed ancestors, must be supplied for accurate CPFP pricing. Any absent id,
/// including an [`Input::residing_txid`] or parent id, is treated as confirmed and ignored, which
/// can underestimate the required fee. Deficits are computed against the full supplied ancestor
/// union; unlike Bitcoin Core, this does not remove transactions that could already be mined at an
/// intermediate feerate, so it may also conservatively overestimate a bump.
///
/// What a selection actually owes is
/// [`SelectionView::ancestor_bump`](crate::SelectionView::ancestor_bump): the shortfall of the
/// ancestors its selected candidates drag in, each charged once, weight and fee netted over the
/// union.
#[derive(Debug, Clone)]
pub struct SelectionProblem {
    target: Target,
    candidates: Vec<Candidate>,
    /// Weight and fee of each ancestor, after txids are dropped.
    ancestors: Vec<(u64, u64)>,
    /// Per-candidate set of ancestor indices dragged in by selecting that candidate.
    ///
    /// Empty when the problem has no ancestors (see [`has_ancestors`](Self::has_ancestors)); use
    /// [`drags_in`](Self::drags_in) rather than indexing this directly.
    drags_in: Vec<Bitset>,
    /// Summed weight and fee of the ancestors *only* this candidate can drag in.
    ///
    /// No other candidate reaches them, so they arrive exactly when this candidate is selected.
    /// Summed rather than reduced to a bump because the target rate must be applied to the total
    /// weight of the whole selection once, and because an ancestor paying above the rate has to be
    /// able to subsidize one paying below it.
    private: Vec<(u64, u64)>,
    /// [`drags_in`](Self::drags_in) restricted to ancestors reachable via several candidates, which
    /// are the only ones that still need de-duplicating at selection time.
    shared_drags_in: Vec<Bitset>,
    /// Whether any ancestor is reachable via exactly one candidate.
    has_private_ancestors: bool,
    /// Whether any ancestor is reachable via more than one candidate.
    has_shared_ancestors: bool,
}

/// The fee still owed so the ancestors in `set` meet `rate`, over the whole set at once.
///
/// Weights and fees are netted across the set, so an overpaying ancestor subsidizes an underpaying
/// one and the result saturates at 0 (the child is never credited).
fn bump_of(ancestors: &[(u64, u64)], rate: FeeRate, set: &Bitset) -> u64 {
    let (weight, fee) = set.iter().fold((0_u64, 0_u64), |(w, f), anc_i| {
        let (anc_w, anc_f) = ancestors[anc_i];
        (w + anc_w, f + anc_f)
    });
    rate.implied_fee_wu(weight).saturating_sub(fee)
}

impl SelectionProblem {
    /// A problem with no unconfirmed ancestors.
    ///
    /// `candidates` are taken as-is.
    pub fn new_no_ancestors(
        target: Target,
        candidates: impl IntoIterator<Item = Candidate>,
    ) -> Self {
        let candidates: Vec<Candidate> = candidates.into_iter().collect();
        let n = candidates.len();
        Self {
            target,
            candidates,
            ancestors: Vec::new(),
            drags_in: (0..n).map(|_| Bitset::with_capacity(0)).collect(),
            private: alloc::vec![(0, 0); n],
            shared_drags_in: (0..n).map(|_| Bitset::with_capacity(0)).collect(),
            has_private_ancestors: false,
            has_shared_ancestors: false,
        }
    }

    /// Build candidates from input groups and the unconfirmed ancestors they may drag in.
    ///
    /// Each input group must be non-empty, and every `AncestorToBump::txid` must be unique. Supply
    /// every unconfirmed residing transaction and transitive unconfirmed ancestor needed for
    /// accurate pricing; absent ids are assumed confirmed.
    ///
    /// For each input group, the residing txids and their transitive parents (restricted to
    /// `ancestors_to_bump`) form that candidate's `drags_in` set. Ancestors only one candidate can
    /// reach are folded into [`private_ancestors`](Self::private_ancestors); the rest stay in
    /// [`shared_drags_in`](Self::shared_drags_in) to be de-duplicated per selection.
    pub fn new<Txid, G, A>(target: Target, input_groups: G, ancestors_to_bump: A) -> Self
    where
        Txid: Copy + Ord + Eq,
        G: IntoIterator,
        G::Item: Into<InputGroup<Txid>>,
        A: IntoIterator,
        A::Item: Into<AncestorToBump<Txid>>,
    {
        let ancestors: Vec<AncestorToBump<Txid>> =
            ancestors_to_bump.into_iter().map(Into::into).collect();

        let txid_to_anc: BTreeMap<Txid, usize> = ancestors
            .iter()
            .enumerate()
            .map(|(i, a)| (a.txid, i))
            .collect();

        let n_anc = ancestors.len();
        let anc_weight_fee: Vec<(u64, u64)> = ancestors.iter().map(|a| (a.weight, a.fee)).collect();
        let mut candidates = Vec::new();
        let mut drags_in = Vec::new();

        for input_group in input_groups {
            let mut cand = Candidate {
                value: 0,
                weight: 0,
                segwit_count: 0,
                legacy_count: 0,
            };
            let mut dragged = Bitset::with_capacity(n_anc);

            for input in input_group.into() {
                cand.value += input.value;
                cand.weight += input.weight;
                match input.is_segwit {
                    true => cand.segwit_count += 1,
                    false => cand.legacy_count += 1,
                }

                let mut txid_stack = alloc::vec![input.residing_txid];
                while let Some(txid) = txid_stack.pop() {
                    if let Some(&anc_i) = txid_to_anc.get(&txid) {
                        if dragged.insert(anc_i) {
                            txid_stack.extend(ancestors[anc_i].parents.iter().copied());
                        }
                    }
                }
            }

            candidates.push(cand);
            drags_in.push(dragged);
        }

        // An ancestor no other candidate can reach arrives exactly when this one is selected, so its
        // weight and fee can be folded into the candidate now. The rest still have to be
        // de-duplicated at selection time.
        let mut reachable_by = alloc::vec![0_u32; n_anc];
        for dragged in &drags_in {
            for anc_i in dragged.iter() {
                reachable_by[anc_i] += 1;
            }
        }
        let mut private = Vec::with_capacity(drags_in.len());
        let mut shared_drags_in = Vec::with_capacity(drags_in.len());
        let mut has_private_ancestors = false;
        let mut has_shared_ancestors = false;
        for dragged in &drags_in {
            let mut private_weight_fee = (0_u64, 0_u64);
            let mut shared = Bitset::with_capacity(n_anc);
            for anc_i in dragged.iter() {
                if reachable_by[anc_i] == 1 {
                    let (weight, fee) = anc_weight_fee[anc_i];
                    private_weight_fee.0 += weight;
                    private_weight_fee.1 += fee;
                    has_private_ancestors = true;
                } else {
                    shared.insert(anc_i);
                    has_shared_ancestors = true;
                }
            }
            private.push(private_weight_fee);
            shared_drags_in.push(shared);
        }

        Self {
            target,
            candidates,
            ancestors: anc_weight_fee,
            drags_in,
            private,
            shared_drags_in,
            has_private_ancestors,
            has_shared_ancestors,
        }
    }

    /// What this problem is funding.
    pub fn target(&self) -> Target {
        self.target
    }

    /// All candidates, in construction order.
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// Candidate at `index`.
    pub fn candidate(&self, index: usize) -> Candidate {
        self.candidates[index]
    }

    /// Number of candidates.
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// Whether there are no candidates.
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// Ancestors as `(weight, fee)` pairs, in the order supplied to [`SelectionProblem::new`].
    ///
    /// Supplied ancestors that no candidate reaches remain in this slice but are not charged.
    pub fn ancestors(&self) -> &[(u64, u64)] {
        &self.ancestors
    }

    /// Whether any candidate drags in an unconfirmed ancestor.
    ///
    /// `false` means every fee calculation reduces to the plain (child-only) case, allowing branch
    /// and bound to use its tighter no-ancestor bounds.
    pub fn has_ancestors(&self) -> bool {
        self.has_private_ancestors || self.has_shared_ancestors
    }

    /// Ancestor indices dragged in by selecting candidate `index`.
    pub fn drags_in(&self, index: usize) -> &Bitset {
        &self.drags_in[index]
    }

    /// Summed `(weight, fee)` of the ancestors only candidate `index` can drag in.
    ///
    /// Deliberately not reduced to a bump: the target rate applies to the total ancestor weight of
    /// the whole selection at once, and an ancestor paying above the rate must be able to subsidize
    /// one paying below it. See [`SelectionView::ancestor_bump`](crate::SelectionView::ancestor_bump).
    pub fn private_ancestors(&self, index: usize) -> (u64, u64) {
        self.private[index]
    }

    /// [`drags_in`](Self::drags_in) restricted to the ancestors that several candidates can reach.
    ///
    /// Those are the only ones that can be dragged in twice over, so they are the only ones a
    /// selection has to de-duplicate; the rest are folded into
    /// [`private_ancestors`](Self::private_ancestors).
    pub fn shared_drags_in(&self, index: usize) -> &Bitset {
        &self.shared_drags_in[index]
    }

    /// Whether any ancestor is reachable via exactly one candidate.
    ///
    /// When `false`, every ancestor is shared and [`private_ancestors`](Self::private_ancestors) is
    /// `(0, 0)` throughout, so summing it can be skipped.
    pub fn has_private_ancestors(&self) -> bool {
        self.has_private_ancestors
    }

    /// Whether any ancestor is reachable via more than one candidate.
    ///
    /// When `false`, what a selection owes is a plain sum over its selected candidates — nothing has
    /// to be de-duplicated.
    pub fn has_shared_ancestors(&self) -> bool {
        self.has_shared_ancestors
    }

    pub(crate) fn ancestor_fee_precision_slack(&self) -> u64 {
        let rate = self.target.fee.rate.spwu() as f64;
        if rate == 0.0 {
            return 0;
        }
        let total_weight = self
            .ancestors
            .iter()
            .fold(0_u64, |sum, (weight, _)| sum.saturating_add(*weight));
        let max_fee = total_weight as f64 * rate;
        if max_fee <= (1_u64 << f32::MANTISSA_DIGITS) as f64 {
            return 0;
        }
        // Conversion and multiplication each round in `implied_fee_wu`. Two f32 epsilons plus one
        // satoshi conservatively cover their combined error for every ancestor subset.
        ((max_fee * 2.0 * f32::EPSILON as f64).min(u64::MAX as f64) as u64).saturating_add(1)
    }

    /// The fee still owed so the ancestors only this candidate would drag in meet
    /// [`Target::fee`](crate::TargetFee)'s rate, as if it were the only selected candidate.
    ///
    /// Informational: must never be summed over a selection (shared ancestors would be charged
    /// twice). What a selection owes is [`SelectionView::ancestor_bump`](crate::SelectionView::ancestor_bump).
    pub fn local_bump(&self, index: usize) -> u64 {
        bump_of(&self.ancestors, self.target.fee.rate, self.drags_in(index))
    }

    /// A [`CoinSelector`] over this problem.
    pub fn selector(&self) -> CoinSelector<'_> {
        CoinSelector::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FeeRate, TargetFee, TargetOutputs};

    fn target(feerate_sat_vb: f32) -> Target {
        Target {
            fee: TargetFee {
                rate: FeeRate::from_sat_per_vb(feerate_sat_vb),
                absolute: 0,
                replace: None,
            },
            outputs: TargetOutputs {
                value_sum: 0,
                weight_sum: 0,
                n_outputs: 0,
            },
            max_weight: None,
        }
    }

    #[test]
    fn no_ancestors_round_trip() {
        let cands = [
            Candidate::new_segwit(100_000, 100),
            Candidate::new_legacy(50_000, 200),
        ];
        let p = SelectionProblem::new_no_ancestors(target(10.0), cands);
        assert_eq!(p.len(), 2);
        assert_eq!(p.candidate(0).value, 100_000);
        assert_eq!(p.candidate(1).legacy_count, 1);
        assert!(p.ancestors().is_empty());
        assert_eq!(p.local_bump(0), 0);
        assert_eq!(p.local_bump(1), 0);
        assert!(p.drags_in(0).is_empty());
    }

    #[test]
    fn transitive_parents() {
        // UTXO on C; C parents B; B parents A. All unconfirmed.
        let ancestors = [
            AncestorToBump {
                txid: "A",
                weight: 400,
                fee: 0,
                parents: vec![],
            },
            AncestorToBump {
                txid: "B",
                weight: 400,
                fee: 0,
                parents: vec!["A"],
            },
            AncestorToBump {
                txid: "C",
                weight: 400,
                fee: 0,
                parents: vec!["B"],
            },
        ];
        let inputs = [Input {
            value: 10_000,
            weight: 272,
            is_segwit: true,
            residing_txid: "C",
        }];
        let p = SelectionProblem::new(target(10.0), inputs, ancestors);
        assert_eq!(p.len(), 1);
        let dragged: Vec<_> = p.drags_in(0).iter().collect();
        assert_eq!(dragged, vec![0, 1, 2]); // A, B, C
    }

    #[test]
    fn shared_ancestor_in_both_drags_in() {
        let ancestors = [AncestorToBump {
            txid: "P",
            weight: 1_000,
            fee: 0,
            parents: vec![],
        }];
        let inputs = [
            Input {
                value: 10_000,
                weight: 272,
                is_segwit: true,
                residing_txid: "P",
            },
            Input {
                value: 20_000,
                weight: 272,
                is_segwit: true,
                residing_txid: "P",
            },
        ];
        let p = SelectionProblem::new(target(10.0), inputs, ancestors);
        assert!(p.drags_in(0).contains(0));
        assert!(p.drags_in(1).contains(0));
        assert_eq!(p.local_bump(0), p.local_bump(1));
        assert!(p.local_bump(0) > 0);
    }

    #[test]
    fn overpaying_ancestor_zero_bump() {
        // weight 400 wu at 1 sat/vb => ~100 sats implied; fee already 10_000
        let ancestors = [AncestorToBump {
            txid: "P",
            weight: 400,
            fee: 10_000,
            parents: vec![],
        }];
        let inputs = [Input {
            value: 10_000,
            weight: 272,
            is_segwit: true,
            residing_txid: "P",
        }];
        let p = SelectionProblem::new(target(1.0), inputs, ancestors);
        assert_eq!(p.local_bump(0), 0);
    }

    #[test]
    fn unknown_parent_ignored() {
        let ancestors = [AncestorToBump {
            txid: "child",
            weight: 400,
            fee: 0,
            parents: vec!["confirmed_parent"],
        }];
        let inputs = [Input {
            value: 10_000,
            weight: 272,
            is_segwit: true,
            residing_txid: "child",
        }];
        let p = SelectionProblem::new(target(10.0), inputs, ancestors);
        let dragged: Vec<_> = p.drags_in(0).iter().collect();
        assert_eq!(dragged, vec![0]); // only child
    }
}
