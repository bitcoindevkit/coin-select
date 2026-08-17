# BDK Coin Selection

`bdk_coin_select` is a zero-dependency tool to help you select inputs for making Bitcoin (ticker: BTC) transactions.

> ⚠ This work is only ready to use by those who expect (potentially catastrophic) bugs and will have
> the time to investigate them and contribute back to this crate.

## Synopsis

```rust
use std::str::FromStr;
use bdk_coin_select::{ CoinSelector, Candidate, SelectionProblem, TR_KEYSPEND_TXIN_WEIGHT, Drain, FeeRate, Target, ChangePolicy, TargetOutputs, TargetFee, DrainWeights};
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
        // and whether segwit transaction fields need to be counted in.
        segwit_count: 1,
        legacy_count: 0,
        // the value of the input
        value: 1_000_000,
        // the total weight of the input(s) including their witness/scriptSig
        // you may need to use miniscript to figure out the correct value here.
        weight: TR_KEYSPEND_TXIN_WEIGHT,
    },
    Candidate {
        // A candidate can represent multiple inputs in the case where you 
        // always want some inputs to be spent together.
        segwit_count: 2,
        legacy_count: 0,
        weight: 2*TR_KEYSPEND_TXIN_WEIGHT,
        value: 3_000_000,
    }
];

// You can now select coins!
let problem = SelectionProblem::new_no_ancestors(target, candidates);
let mut coin_selector = CoinSelector::new(&problem);
coin_selector.select(0);

// Aggregate queries live on a cached view of the current selection.
let view = coin_selector.compute_view();
assert!(!view.is_funded(), "we didn't select enough");
println!("we didn't select enough yet we're missing: {}", view.missing());
coin_selector.select(1);
assert!(coin_selector.compute_view().is_funded(), "we should have enough now");

// Now we need to know if we need a change output to drain the excess if we overshot too much
//
// We don't need to know exactly which change output we're going to use yet but we assume it's a taproot output
// that we'll use a keyspend to spend from.
let drain_weights = DrainWeights::TR_KEYSPEND; 
// Our policy is to only add a change output if the value is over 1_000 sats
let change_policy = ChangePolicy::min_value(drain_weights, 1_000);
let change = coin_selector.compute_view().drain(change_policy);
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
use bdk_coin_select::{ BnbMetric, Candidate, CoinSelector, FeeRate, SelectionProblem, Target, TargetFee, TargetOutputs, TR_KEYSPEND_TXIN_WEIGHT};
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
        segwit_count: 1,
        legacy_count: 0,
        value: 400_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
    },
    Candidate {
        segwit_count: 1,
        legacy_count: 0,
        value: 200_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
    },
    Candidate {
        segwit_count: 1,
        legacy_count: 0,
        value: 11_000,
        weight: TR_KEYSPEND_TXIN_WEIGHT,
    }
];
let drain_weights = bdk_coin_select::DrainWeights::TR_KEYSPEND;
// A wallet-policy or fee-estimator assumption for the future spend of change.
let long_term_feerate = FeeRate::from_sat_per_vb(10.0);

let target = Target {
    fee: TargetFee::from_feerate(FeeRate::from_sat_per_vb(15.0)),
    outputs: TargetOutputs::fund_outputs(outputs.iter().map(|output| (output.weight().to_wu(), output.value.to_sat()))),
    max_weight: None,
};

let problem = SelectionProblem::new_no_ancestors(target, candidates);
let mut coin_selector = CoinSelector::new(&problem);

// For repeated read-only calculations, compute a cached view of the current selection.
let empty_view = coin_selector.compute_view();
assert_eq!(empty_view.selected_value(), 0);

// The feerate used to work out whether a change output would be dust (and so shouldn't be added).
// The standard dust relay feerate is 3 sat/vb.
let dust_relay_feerate = FeeRate::from_sat_per_vb(3.0);

// The LowestFee metric tries to make selections that minimize your total fees paid over time. It
// decides for itself whether to add a change output: change is added whenever doing so reduces the
// long-term fee (factoring in the cost to spend the output later on), the value is at least the
// dust threshold, and the transaction with change fits its weight cap.
let mut metric = LowestFee {
    long_term_feerate, // used to calculate the cost of spending the change output in the future
    dust_relay_feerate,
    drain_weights,
};

// We run the branch and bound algorithm with a max round limit of 100,000.
// On success it returns the score along with the change output the metric decided on.
let change = match coin_selector.run_bnb(metric, 100_000) {
    Err(err) => {
        println!("failed to find a solution: {}", err);
        // fall back to naive selection
        coin_selector.select_until_target_met().expect("a selection was impossible!");
        // the metric still decides the change output for whatever we end up selecting
        metric.drain(&coin_selector.compute_view())
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
println!("We are including a change output of {} value (0 means no change)", change.value);


```

## Unconfirmed ancestors

Use `SelectionProblem::new` when spending unconfirmed UTXOs. Supply every unconfirmed transaction
that created an input and all of its transitive unconfirmed ancestors; missing transaction ids are
treated as confirmed and can make the required CPFP fee too low. Parent lists contain direct parents
only. Ancestors shared by several selected inputs are charged once over their union.

```rust
use bdk_coin_select::{
    AncestorToBump, FeeRate, Input, SelectionProblem, Target, TargetFee, TargetOutputs,
};

let target = Target {
    fee: TargetFee::from_feerate(FeeRate::from_sat_per_vb(5.0)),
    outputs: TargetOutputs::fund_outputs([(136, 50_000)]),
    max_weight: None,
};
let inputs = [Input {
    value: 100_000,
    weight: 272,
    is_segwit: true,
    residing_txid: "child",
}];
let ancestors = [
    AncestorToBump {
        txid: "parent",
        weight: 400,
        fee: 100,
        parents: vec![],
    },
    AncestorToBump {
        txid: "child",
        weight: 600,
        fee: 200,
        parents: vec!["parent"],
    },
];
let problem = SelectionProblem::new(target, inputs, ancestors);
let mut coin_selector = problem.selector();
```

Adding an input may drag in more fee debt than value, so funding is not necessarily monotone for
ancestor-aware problems. `run_bnb` accounts for this and de-duplicates shared ancestors.

## Minimum Supported Rust Version (MSRV)

This library compiles on Rust 1.54 and above.
