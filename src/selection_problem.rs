use alloc::vec::Vec;

use crate::{Candidate, CoinSelector, Target};

/// Target and candidates for one coin-selection run.
///
/// Pass a reference to [`CoinSelector::new`]. The selector borrows it for its lifetime, so the
/// target and candidates stay fixed while it runs.
#[derive(Debug, Clone)]
pub struct SelectionProblem {
    target: Target,
    candidates: Vec<Candidate>,
}

impl SelectionProblem {
    /// A problem with no unconfirmed ancestors.
    ///
    /// `candidates` are taken as-is.
    pub fn new_no_ancestors(
        target: Target,
        candidates: impl IntoIterator<Item = Candidate>,
    ) -> Self {
        Self {
            target,
            candidates: candidates.into_iter().collect(),
        }
    }

    /// What this problem is funding.
    pub fn target(&self) -> Target {
        self.target
    }

    /// A copy of this problem — same candidates and ancestry — funding `target` instead.
    ///
    /// Pair it with [`CoinSelector::with_problem`] to re-measure an existing selection against
    /// another target.
    pub fn with_target(&self, target: Target) -> Self {
        Self {
            target,
            ..self.clone()
        }
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

    /// A [`CoinSelector`] over this problem.
    pub fn selector(&self) -> CoinSelector<'_> {
        CoinSelector::new(self)
    }
}
