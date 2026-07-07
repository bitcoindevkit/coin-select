# BDK Coin Selection

`bdk_coin_select` is a zero-dependency tool to help you select inputs for making Bitcoin (ticker: BTC) transactions.

> ⚠ This work is only ready to use by those who expect (potentially catastrophic) bugs and will have
> the time to investigate them and contribute back to this crate.

## Synopis

```rust
use std::str::FromStr;
use bdk_coin_select::{ CoinSelector, Candidate, TR_KEYSPEND_TXIN_WEIGHT, Drain, FeeRate, Target, ChangePolicy, TargetOutputs, TargetFee, DrainWeights};
use bitcoin::{ Amount, Address, Network, Transaction, TxIn, TxOut };

let recipient_addr: Address = "tb1pvjf9t34fznr53u5tqhejz4nr69luzkhlvsdsdfq9pglutrpve2xq7hps46"
    .parse::<Address<_>>()
    .unwrap()
    .assume_checked();

let outputs = vec![TxOut {
    value: Amount::from_sat(3_500_000),
    script_pubkey: recipient_addr.script_pubkey(),
}];

let target = Target {
    outputs: TargetOutputs::fund_outputs(outputs.iter().map(|output| (output.weight().to_wu(), output.value.to_sat()))),
    fee: TargetFee::from_feerate(FeeRate::from_sat_per_vb(42.0)),
    // An optional cap on the resulting transaction weight (e.g. for TRUC). `None` = unconstrained.
    max_weight: None,
};

let candidates = vec![
    Candidate {
        // How many inputs does this candidate represents. Needed so we can 
        // figure out the weight of the varint that encodes the number of inputs
        input_count: 1,
        // the value of the input
        value: 1_000_000,
        // the total weight of the input(s) including their witness/scriptSig
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

// You can now select coins!
let mut coin_selector = CoinSelector::new(&candidates);
coin_selector.select(0);

assert!(!coin_selector.is_funded(target), "we didn't select enough");
println!("we didn't select enough yet we're missing: {}", coin_selector.missing(target));
coin_selector.select(1);
assert!(coin_selector.is_funded(target), "we should have enough now");

// Now we need to know if we need a change output to drain the excess if we overshot too much
//
// We don't need to know exactly which change output we're going to use yet but we assume it's a taproot output
// that we'll use a keyspend to spend from.
let drain_weights = DrainWeights::TR_KEYSPEND; 
// Our policy is to only add a change output if the value is over 1_000 sats
let change_policy = ChangePolicy::min_value(drain_weights, 1_000);
let change = coin_selector.drain(target, change_policy);
if change.is_some() {
    println!("We need to add our change output to the transaction with {} value", change.value);
} else {
    println!("Yay we don't need to add a change output");
}
```

## Automatic selection with Branch and Bound

You can use methods such as [`CoinSelector::select`] to manually select coins, or methods such as
[`CoinSelector::select_until_target_met`] for a rudimentary automatic selection. Probably you want
to use [`CoinSelector::run_bnb`] to do this in a smart way.

Built-in metrics are provided in the [`metrics`] submodule. Currently, only the
[`LowestFee`](metrics::LowestFee) metric is considered stable. Note you *can* try and write your own
metric by implementing the [`BnbMetric`] yourself but we don't recommend this.

```rust
use std::str::FromStr;
use bdk_coin_select::{ BnbMetric, Candidate, CoinSelector, FeeRate, Target, TargetFee, TargetOutputs, TR_KEYSPEND_TXIN_WEIGHT};
use bdk_coin_select::metrics::LowestFee;
use bitcoin::{ Address, Amount, Network, Transaction, TxIn, TxOut };

let recipient_addr: Address = "tb1pvjf9t34fznr53u5tqhejz4nr69luzkhlvsdsdfq9pglutrpve2xq7hps46"
    .parse::<Address<_>>()
    .unwrap()
    .assume_checked();

let outputs = vec![TxOut {
    value: Amount::from_sat(210_000),
    script_pubkey: recipient_addr.script_pubkey(),
}];

let candidates = [
    Candidate {
        input_count: 1,
        value: 400_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true
    },
    Candidate {
        input_count: 1,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true
    },
    Candidate {
        input_count: 1,
        value: 11_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
        is_segwit: true
    }
];
let drain_weights = bdk_coin_select::DrainWeights::default();
// You could determine this by looking at the user's transaction history and taking an average of the feerate.
let long_term_feerate = FeeRate::from_sat_per_vb(10.0);

let mut coin_selector = CoinSelector::new(&candidates);

let target = Target {
    fee: TargetFee::from_feerate(FeeRate::from_sat_per_vb(15.0)),
    outputs: TargetOutputs::fund_outputs(outputs.iter().map(|output| (output.weight().to_wu(), output.value.to_sat()))),
    max_weight: None,
};

// The feerate used to work out whether a change output would be dust (and so shouldn't be added).
// The standard dust relay feerate is 3 sat/vb.
let dust_relay_feerate = FeeRate::from_sat_per_vb(3.0);

// The LowestFee metric tries to make selections that minimize your total fees paid over time. It
// decides for itself whether to add a change output: change is added whenever doing so reduces the
// long-term fee (factoring in the cost to spend the output later on) and the change wouldn't be dust.
let mut metric = LowestFee {
    long_term_feerate, // used to calculate the cost of spending the change output in the future
    dust_relay_feerate,
    drain_weights,
};

// We run the branch and bound algorithm with a max round limit of 100,000.
// On success it returns the score along with the change output the metric decided on.
let change = match coin_selector.run_bnb(target, metric, 100_000) {
    Err(err) => {
        println!("failed to find a solution: {}", err);
        // fall back to naive selection
        coin_selector.select_until_target_met(target).expect("a selection was impossible!");
        // the metric still decides the change output for whatever we end up selecting
        metric.drain(&coin_selector, target)
    }
    Ok((score, change)) => {
        println!("we found a solution with score {}", score);
        change
    }
};


let selection = coin_selector
   .apply_selection(&candidates)
   .collect::<Vec<_>>();

println!("we selected {} inputs", selection.len());
println!("We are including a change output of {} value (0 means not change)", change.value);


```

# Minimum Supported Rust Version (MSRV)

This library is compiles on rust v1.54 and above

