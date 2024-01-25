# Unreleased

- Remove `is_target_met_with_change_policy`: it was redundant. If the target is met without a change policy it will always be met with it.
- Remove `min_fee` in favour of `replace` which allows you to replace a transaction
- Remove `Drain` argument from `CoinSelector::select_until_target_met` because adding a drain won't
  change when the target is met.
- No more `base_weight` in `CoinSelector`. Weight of the outputs is tracked in `target`.
- You now account for the number of outputs in both drain and target and their weight.
- Removed waste metric because it was pretty broken and took a lot to maintain

