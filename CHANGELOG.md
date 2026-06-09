# Unreleased

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

