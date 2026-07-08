//! Branch and bound metrics that can be passed to [`CoinSelector::bnb_solutions`] or
//! [`CoinSelector::run_bnb`].
//!
//! [`CoinSelector::bnb_solutions`]: crate::CoinSelector::bnb_solutions
//! [`CoinSelector::run_bnb`]: crate::CoinSelector::run_bnb
mod lowest_fee;
pub use lowest_fee::*;
mod changeless;
pub use changeless::*;
mod changeless_waste;
pub use changeless_waste::*;

use crate::{Candidate, CoinSelector, Drain, DrainWeights, Target};

/// Outcome of the "resize trick" (see [`resize_bound`]).
enum ResizeBound<'a> {
    /// The crossing selection hit the target with exactly zero excess, so it is itself the
    /// minimum-cost target-meeting descendant. Holds that selection.
    Exact(CoinSelector<'a>),
    /// The crossing input must be fractionally resized. Holds the selection with the crossing input
    /// deselected, the crossing candidate, and the `scale ∈ [0, 1]` of it that satisfies every fee
    /// constraint with exactly zero excess.
    Resize(CoinSelector<'a>, Candidate, f32),
}

/// The "resize trick" behind [`LowestFee`]'s fee lower bound, for the case where `cs` does not
/// yet meet the target.
///
/// Walk the `value_pwu`-sorted unselected list until the target is first crossed, then represent
/// the crossing candidate as a fractional `scale ∈ [0, 1]` that satisfies each fee constraint
/// (rate, replacement, absolute) with exactly zero excess. Among all subsets of unselected that
/// reach the target, the highest-`value_pwu` candidates are the most efficient, so the
/// resize-scaled prefix approximates the cheapest target-meeting descendant.
///
/// Returns `None` if no target-meeting descendant exists (a fee constraint cannot be satisfied by
/// any available candidate).
///
/// CAUTION: the crossing point and `scale` are computed from the greedy prefix's *actual*
/// (`input_weight()`-corrected, vbyte-rounded) fee and weight. A descendant that avoids the
/// prefix's segwit/varint corrections can undercut quantities derived from them by a few sats or
/// weight units, so this walk must NOT be used as a lower bound on a descendant's `input_weight`
/// — [`ChangelessWaste`]'s bound was migrated to raw-weight-linear arithmetic for exactly that
/// reason (see the derivation in `ChangelessWaste::bound`). The same caveat applies in fee space
/// to the remaining use here.
fn resize_bound<'a>(cs: &CoinSelector<'a>, target: Target) -> Option<ResizeBound<'a>> {
    // Step 1: select everything up until the input that first hits the target.
    let (mut cs, resize_index, to_resize) = cs
        .clone()
        .select_iter()
        .find(|(cs, _, _)| cs.is_funded(target))?;

    // If this selection is already perfect, it is the minimum-cost target-meeting descendant.
    if cs.excess(target, Drain::NONE) == 0 {
        return Some(ResizeBound::Exact(cs));
    }
    cs.deselect(resize_index);

    // Find the smallest `scale` of `to_resize` that satisfies every fee constraint. We imagine a
    // perfect input that hits the target with zero excess: for a feerate constraint,
    //
    //     scale = remaining_value_to_reach_feerate / effective_value_of_resized_input
    //
    // In this perfect scenario no extra fee is needed for weight-unit-to-vbyte rounding, so all
    // computations are on weight units directly.
    let mut scale = 0.0_f32;

    let rate_excess = cs.rate_excess_wu(target, Drain::NONE) as f32;
    if rate_excess < 0.0 {
        let remaining = rate_excess.abs();
        let ev_resized = to_resize.effective_value(target.fee.rate);
        if ev_resized > 0.0 {
            scale = scale.max(remaining / ev_resized);
        } else {
            return None; // we can never satisfy the constraint
        }
    }
    // Replacement uses the same approach with the incremental relay feerate.
    if let Some(replace) = target.fee.replace {
        let replace_excess = cs.replacement_excess_wu(target, Drain::NONE) as f32;
        if replace_excess < 0.0 {
            let remaining = replace_excess.abs();
            let ev_resized = to_resize.effective_value(replace.incremental_relay_feerate);
            if ev_resized > 0.0 {
                scale = scale.max(remaining / ev_resized);
            } else {
                return None; // we can never satisfy the constraint
            }
        }
    }
    // The absolute fee is a fixed amount (not weight-proportional), so we just need enough raw
    // value to cover the gap.
    let absolute_excess = cs.absolute_excess(target, Drain::NONE) as f32;
    if absolute_excess < 0.0 {
        let remaining = absolute_excess.abs();
        if to_resize.value > 0 {
            scale = scale.max(remaining / to_resize.value as f32);
        } else {
            return None; // we can never satisfy the constraint
        }
    }

    // max_weight-aware: reaching the feerate needs a perfect input weighing
    // `scale * to_resize.weight`. `to_resize` is the best value-per-weight input available,
    // so if the current weight plus even that (fractional) minimum already busts the cap,
    // no within-cap selection down this branch reaches the target -> prune. This is the
    // fractional relaxation, so it never prunes a branch with an (integer) within-cap
    // solution.
    if let Some(max_weight) = target.max_weight {
        if cs.weight(target.outputs, DrainWeights::NONE) as f32 + scale * to_resize.weight as f32
            > max_weight as f32
        {
            return None;
        }
    }

    Some(ResizeBound::Resize(cs, to_resize, scale))
}
