# Unreleased

- **Breaking:** Move the aggregate queries off `CoinSelector` and onto `SelectionView`, which already answered them from its cache while `CoinSelector` recomputed them by iterating. Obtain a view with `CoinSelector::compute_view` and call them there. Removed: `absolute_excess`, `ancestor_bump`, `ancestor_bump_lower_bound`, `drain`, `drain_value`, `effective_value`, `excess`, `fee`, `implied_fee`, `implied_feerate`, `input_weight`, `is_funded`, `is_funded_with_drain`, `is_within_max_weight`, `missing`, `rate_excess`, `rate_excess_wu`, `replacement_excess`, `replacement_excess_wu`, `selected_value`, `waste` and `weight`. `CoinSelector::select_until` now hands its predicate a `&SelectionView` instead of a `&CoinSelector`, and maintains that view's aggregates incrementally, so a predicate such as `|view| view.is_funded()` costs the same at every step rather than rescanning the growing selection.
- **Breaking:** Replace `Candidate`'s `input_count` and `is_segwit` fields with `segwit_count` and `legacy_count`, fixing `SelectionView::input_weight` undercounting candidates that group multiple inputs: in a segwit transaction every legacy input still serializes an empty witness (1 WU), which was previously paid once per candidate instead of once per legacy input, so a group of N legacy inputs came out N-1 WU short. Splitting the count by script type also means a single candidate may now mix legacy and segwit inputs and still be priced exactly. Replaces `Candidate::new` with `Candidate::new_segwit` and `Candidate::new_legacy`.
- **Breaking:** Add `SelectionProblem`, which owns the target, candidates, and optional unconfirmed-ancestor data for a selection run. `CoinSelector::new` now takes `&SelectionProblem`, and the selector borrows it for its lifetime.
- Charge selections for the fee needed to bring the union of their unconfirmed ancestors up to the target feerate. Build ancestor-aware problems from `Input`/`InputGroup` and `AncestorToBump` with `SelectionProblem::new`; use `SelectionProblem::new_no_ancestors` for prebuilt candidates that need no CPFP bump.
- Add `SelectionView`, a cached view obtained with `CoinSelector::compute_view`. `BnbMetric::{score, bound, drain}` now consume `&SelectionView`; branch and bound maintains its aggregates incrementally while the underlying selector remains unchanged.
- Add a per-branch cursor to avoid repeatedly scanning already-decided candidates during branch-and-bound search.
- **Breaking:** On a problem with unconfirmed ancestors, `CoinSelector::bnb_solutions` and `run_bnb` now dive and then deepen on the bound instead of only diving, changing which nodes they visit and in what order. The traversal stays depth-first and linear in memory, but it runs in passes under a rising ceiling on the bound, recovering the node ordering a priority queue would give. This is what stops a dive from stalling on an incumbent its candidate order led it to: measured over 42 benchmark fixtures against Bitcoin Core's wallet coin selection, total package fee falls 6.51% with six fixtures improved and none regressed, and the worst case improves 48%. Problems with no unconfirmed ancestors are unaffected — the dive has no ordering pathology to escape there and re-expanding from the root would cost a truncated search more than the better node order wins it. `CoinSelector::bnb_solutions_dive_only` always dives, and `bnb_solutions_hybrid` takes both tuning knobs explicitly; their defaults are `CoinSelector::DEFAULT_DEEPENING_EPS` and `DEFAULT_DIVE_FLOOR_PER_CANDIDATE`. Callers that assert on exact round counts should expect them to change.
- **Breaking:** `BnbMetric` metrics now decide the change output themselves. The trait gains a `drain(&mut self, cs) -> Drain` method; call it on a branch-and-bound solution (or the `LowestFee` metric directly) to get the change output the metric optimized against, instead of computing a separate `ChangePolicy`.
- **Breaking:** `CoinSelector::run_bnb` now returns `(Ordf32, Drain)` instead of just `Ordf32`, handing back the change output the metric decided on for the winning selection.
- **Breaking:** `LowestFee` no longer takes a `change_policy`. It now takes `dust_relay_feerate: FeeRate` and `drain_weights: DrainWeights`, and adds change only when doing so lowers the long-term fee, the value is at least the dust threshold, and the transaction with change fits `Target::max_weight`.
- Add `DrainWeights::dust_threshold(dust_relay_feerate)`, the minimum value a change output with these weights must have to not be dust.
- Add `CoinSelector::select_srd`, a Single Random Draw selector (port of Bitcoin Core's `SelectCoinsSRD`) that adds candidates in random order until the change reaches `change_lower`, producing a healthy-sized (privacy-friendly) change output instead of minimizing fees. Adds the `CHANGE_LOWER` constant for Core's value.
- Search branch and bound depth-first (best-child first, in-place backtracking) instead of best-first over a heap of cloned branches. Under a round cap this finds complete solutions on large pools where the old frontier often exhausted the budget without a selection.
- Hard-prune branch-and-bound nodes whose remaining candidates cannot meet the target feerate, using a running total the selection cache maintains (a port of Bitcoin Core's `SelectCoinsBnB` lookahead). The relaxation credits still-reachable ancestor surplus, so it holds with unconfirmed ancestors too.
- Let the ancestor-aware `LowestFee` bound prune a subtree when the best input still available cannot close a fee deficit at any weight. Previously this path was never allowed to claim infeasibility at all, because funding is not monotone with unconfirmed ancestors; that argument does not cover this case, which the relaxation can prove outright.
- Seed branch and bound with the greedy selection, so a search that runs out of rounds returns the best selection it has instead of `NoBnbSolution::RoundLimit`. `RoundLimit` now means the metric rejected the greedy selection too.
- **Breaking:** Remove the changeless metrics: `Changeless`, `LowestFeeChangeless`, and the `BnbMetric` tuple implementations (`impl BnbMetric for ((A, f32), ...)`). Generic metric composition is no longer supported. `LowestFee` decides for itself whether a selection should carry change, so a separate changeless objective is no longer maintained.
- **Breaking:** `CoinSelector::selected_indices` and `CoinSelector::banned` now return `&Bitset` instead of `&BTreeSet<usize>`. `Bitset` exposes `contains`/`len`/`is_empty`/`iter` (#46)
- Replace the internal `Cow<BTreeSet>`/`Cow<[usize]>` selection state with a `Bitset` and an `Arc`-shared candidate order, making the per-branch clones in branch-and-bound substantially cheaper (#46)
- Fix compilation error when building with `--no-default-features` (#36)

# 0.4.0

- Use `u64` for weights instead of u32
- Fix feerate not being rounded up to vbytes #29
- Fix `new_tr_keyspend` weight

# 0.3.0

- Remove `is_target_met_with_change_policy`: it was redundant. If the target is met without a change policy it will always be met with it.
- Remove `min_fee` in favour of `replace` which allows you to replace a transaction
- Remove `Drain` argument from `CoinSelector::select_until_target_met` because adding a drain won't
  change when the target is met.
- No more `base_weight` in `CoinSelector`. Weight of the outputs is tracked in `target`.
- You now account for the number of outputs in both drain and target and their weight.
- Removed waste metric because it was pretty broken and took a lot to maintain
