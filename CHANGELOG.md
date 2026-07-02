# Unreleased

- **Breaking:** `BnbMetric` metrics now decide the change output themselves. The trait gains a `drain(&mut self, cs) -> Drain` method; call it on a branch-and-bound solution (or the `LowestFee` metric directly) to get the change output the metric optimized against, instead of computing a separate `ChangePolicy`.
- **Breaking:** `CoinSelector::run_bnb` now returns `(Ordf32, Drain)` instead of just `Ordf32`, handing back the change output the metric decided on for the winning selection.
- **Breaking:** `LowestFee` no longer takes a `change_policy`. It now takes `dust_relay_feerate: FeeRate` and `drain_weights: DrainWeights`, and adds change only when doing so lowers the long-term fee and the change would not be dust.
- Add `DrainWeights::dust_threshold(dust_relay_feerate)`, the minimum value a change output with these weights must have to not be dust.
- **Breaking:** `Changeless` is now `Changeless<M>`, wrapping an inner metric it constrains to changeless solutions (e.g. `Changeless<LowestFee>`), replacing the previous tuple-composition approach.
- **Breaking:** Removed the `BnbMetric` tuple implementations (`impl BnbMetric for ((A, f32), ...)`). Weighted composition of independent metrics is no longer supported; the only composition still provided is the changeless constraint, now expressed as `Changeless<M>`. If you relied on tuples to blend multiple objectives, there is no drop-in replacement.
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

