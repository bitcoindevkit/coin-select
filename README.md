# BDK Coin Selection

`bdk_coin_select` is a tool to help you select inputs for making Bitcoin (ticker: BTC) transactions.
It's got zero dependencies so you can paste it into your project without concern.

## Constructing the `CoinSelector`

The main structure is [`CoinSelector`](crate::CoinSelector). To construct it, we specify a list of
candidate UTXOs and a transaction `base_weight`. The `base_weight` includes the recipient outputs
and mandatory inputs (if any).

```rust
use std::str::FromStr;
use bdk_coin_select::{ CoinSelector, Candidate, TR_KEYSPEND_TXIN_WEIGHT};
use bitcoin::{ Address, Network, Transaction, TxIn, TxOut };

// The address where we want to send our coins.
let recipient_addr = 
    Address::from_str("tb1pvjf9t34fznr53u5tqhejz4nr69luzkhlvsdsdfq9pglutrpve2xq7hps46").unwrap();

let candidates = vec![
    Candidate {
        // How many inputs does this candidate represents. Needed so we can 
        // figure out the weight of the varint that encodes the number of inputs
        input_count: 1,
        // the value of the input
        value: 1_000_000,
        // the total weight of the input(s).
        // you may need to use miniscript to figure out the correct value here.
        weight: TR_KEYSPEND_TXIN_WEIGHT, 
        // wether it's a segwit input. Needed so we know whether to include the
        // segwit header in total weight calculations.
        is_segwit: true
    },
    Candidate {
        // A candidate can represent multiple inputs in the case where you 
        // always want some inputs to be spent together.
        input_count: 2,
        weight: 2*TR_KEYSPEND_TXIN_WEIGHT,
        value: 3_000_000,
        is_segwit: true
    }
];

let base_tx = Transaction {
    input: vec![],
    // include your recipient outputs here
    output: vec![TxOut {
        value: 900_000,
        script_pubkey: recipient_addr.payload.script_pubkey(),
    }],
    lock_time: bitcoin::absolute::LockTime::from_height(0).unwrap(),
    version: 0x02,
};
let base_weight = base_tx.weight().to_wu() as u32;
println!("base weight: {}", base_weight);

// You can now select coins!
let mut coin_selector = CoinSelector::new(&candidates, base_weight);
coin_selector.select(0);
```

## Change Policy

A change policy determines whether the drain output(s) should be in the final solution. The
determination is simple: if the excess value is above a threshold then the drain should be added. To
construct a change policy you always provide `DrainWeights` which tell the coin selector the weight
cost of adding the drain. `DrainWeights` includes two weights. One is the weight of the drain
output(s). The other is the weight of spending the drain output later on (the input weight).


```rust
use std::str::FromStr;
use bdk_coin_select::{CoinSelector, Candidate, DrainWeights, TXIN_BASE_WEIGHT, ChangePolicy, TR_KEYSPEND_TXIN_WEIGHT};
use bitcoin::{Address, Network, Transaction, TxIn, TxOut};
const TR_SATISFACTION_WEIGHT: u32 = 66;
let base_tx = Transaction {
    input: vec![],
    output: vec![/* include your recipient outputs here */],
    lock_time: bitcoin::absolute::LockTime::from_height(0).unwrap(),
    version: 0x02,
};
let base_weight = base_tx.weight().to_wu() as u32;

// The change output that may or may not be included in the final transaction.
let drain_addr =
    Address::from_str("tb1pvjf9t34fznr53u5tqhejz4nr69luzkhlvsdsdfq9pglutrpve2xq7hps46")
    .expect("address must be valid")
    .require_network(Network::Testnet)
    .expect("network must match");

// The drain output(s) may or may not be included in the final tx. We calculate
// the drain weight to include the output length varint weight changes from
// including the drain output(s).
let drain_output_weight = {
    let mut tx_with_drain = base_tx.clone();
    tx_with_drain.output.push(TxOut {
        script_pubkey: drain_addr.script_pubkey(),
        ..Default::default()
    });
    tx_with_drain.weight().to_wu() as u32 - base_weight
};
println!("drain output weight: {}", drain_output_weight);

let drain_weights = DrainWeights {
    output_weight: drain_output_weight,
    spend_weight: TR_KEYSPEND_TXIN_WEIGHT,
};

// This constructs a change policy that creates change when the change value is
// greater than or equal to the dust limit.
let change_policy = ChangePolicy::min_value(
    drain_weights,
    drain_addr.script_pubkey().dust_value().to_sat(),
);
```

## Branch and Bound

You can use methods such as [`CoinSelector::select`] to manually select coins, or methods such as
[`CoinSelector::select_until_target_met`] for a rudimentary automatic selection. However, if you 
wish to automatically select coins to optimize for a given metric, [`CoinSelector::run_bnb`] can be
used.

Built-in metrics are provided in the [`metrics`] submodule. Currently, only the 
[`LowestFee`](metrics::LowestFee) metric is considered stable.

```rust
use bdk_coin_select::{ Candidate, CoinSelector, FeeRate, Target, ChangePolicy };
use bdk_coin_select::metrics::LowestFee;
let candidates = [];
let base_weight = 0;
let drain_weights = bdk_coin_select::DrainWeights::default();
let dust_limit = 0;
let long_term_feerate = FeeRate::default_min_relay_fee();

let mut coin_selector = CoinSelector::new(&candidates, base_weight);

let target = Target {
    feerate: FeeRate::default_min_relay_fee(),
    min_fee: 0,
    value: 210_000,
};

// We use a change policy that introduces a change output if doing so reduces
// the "waste" and that the change output's value is at least that of the 
// `dust_limit`.
let change_policy = ChangePolicy::min_value_and_waste(
    drain_weights,
    dust_limit,
    target.feerate,
    long_term_feerate,
);

// This metric minimizes transaction fees paid over time. The 
// `long_term_feerate` is used to calculate the additional fee from spending 
// the change output in the future.
let metric = LowestFee {
    target,
    long_term_feerate,
    change_policy
};

// We run the branch and bound algorithm with a max round limit of 100,000.
match coin_selector.run_bnb(metric, 100_000) {
    Err(err) => println!("failed to find a solution: {}", err),
    Ok(score) => {
        println!("we found a solution with score {}", score);

        let selection = coin_selector
            .apply_selection(&candidates)
            .collect::<Vec<_>>();
        let change = coin_selector.drain(target, change_policy);

        println!("we selected {} inputs", selection.len());
        println!("We are including a change output of {} value (0 means not change)", change.value);
    }
};
```

## Finalizing a Selection

- [`is_target_met`] checks whether the current state of [`CoinSelector`] meets the [`Target`].
- [`apply_selection`] applies the selection to the original list of candidate `TxOut`s.

[`is_target_met`]: crate::CoinSelector::is_target_met
[`apply_selection`]: crate::CoinSelector::apply_selection
[`CoinSelector`]: crate::CoinSelector
[`Target`]: crate::Target

```rust
use bdk_coin_select::{CoinSelector, Candidate, DrainWeights, Target, ChangePolicy, TR_KEYSPEND_TXIN_WEIGHT, Drain};
use bitcoin::{Amount, TxOut, Address};
let base_weight = 0_u32;
let drain_weights = DrainWeights::new_tr_keyspend();
use core::str::FromStr;

// A random target, as an example.
let target = Target {
    value: 21_000,
    ..Default::default()
};
// Am arbitary drain policy, for the example.
let change_policy = ChangePolicy::min_value(drain_weights, 1337);

// This is a list of candidate txouts for coin selection. If a txout is picked,
// our transaction's input will spend it.
let candidate_txouts = vec![
    TxOut {
        value: 100_000,
        script_pubkey: Address::from_str("bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr").unwrap().payload.script_pubkey(),
    },
    TxOut {
        value: 150_000,
        script_pubkey: Address::from_str("bc1p4qhjn9zdvkux4e44uhx8tc55attvtyu358kutcqkudyccelu0was9fqzwh").unwrap().payload.script_pubkey(),
    },
    TxOut {
        value: 200_000,
        script_pubkey: Address::from_str("bc1p0d0rhyynq0awa9m8cqrcr8f5nxqx3aw29w4ru5u9my3h0sfygnzs9khxz8").unwrap().payload.script_pubkey()
    }
];
// We transform the candidate txouts into something `CoinSelector` can 
// understand.
let candidates = candidate_txouts
    .iter()
    .map(|txout| Candidate {
        input_count: 1,
        value: txout.value,
        weight: TR_KEYSPEND_TXIN_WEIGHT, // you need to figure out the weight of the txin somehow
        is_segwit: txout.script_pubkey.is_witness_program(),
    })
    .collect::<Vec<_>>();

let mut selector = CoinSelector::new(&candidates, base_weight);
let _result = selector
    .select_until_target_met(target,  Drain::none());

// Determine what the drain output will be, based on our selection.
let drain = selector.drain(target, change_policy);

// In theory the target must always still be met at this point
assert!(selector.is_target_met(target, drain));

// Get a list of coins that are selected.
let selected_coins = selector
    .apply_selection(&candidate_txouts)
    .collect::<Vec<_>>();
assert_eq!(selected_coins.len(), 1);
```

# Minimum Supported Rust Version (MSRV)

This library is tested to compile on 1.54

To build with the MSRV, you will need to pin the following dependencies:

```shell
# tempfile 3.7.0 has MSRV 1.63.0+
cargo update -p tempfile --precise "3.6.0"
```
