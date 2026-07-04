#![allow(dead_code)]

use bdk_coin_select::{
    float::Ordf32, metrics::LowestFee, BnbMetric, Candidate, CoinSelector, Drain, DrainWeights,
    FeeRate, NoBnbSolution, Replace, Target, TargetFee, TargetOutputs,
};
use proptest::{
    prelude::*,
    prop_assert, prop_assert_eq,
    test_runner::{RngAlgorithm, TestRng},
};
use rand::seq::IteratorRandom;
use std::any::type_name;

pub fn replace(fee_strategy: impl Strategy<Value = u64>) -> impl Strategy<Value = Replace> {
    fee_strategy.prop_map(|fee| Replace {
        fee,
        incremental_relay_feerate: FeeRate::DEFUALT_RBF_INCREMENTAL_RELAY,
    })
}

pub fn maybe_replace(
    fee_strategy: impl Strategy<Value = u64>,
) -> impl Strategy<Value = Option<Replace>> {
    proptest::option::of(replace(fee_strategy))
}

/// Strategy for an optional [`Target::max_weight`] cap (`None` = unconstrained).
pub fn maybe_max_weight(
    weight_strategy: impl Strategy<Value = u64>,
) -> impl Strategy<Value = Option<u64>> {
    proptest::option::of(weight_strategy)
}

/// Used for constructing a proptest that compares an exhaustive search result with a bnb result
/// with the given metric.
///
/// We don't restrict bnb rounds, so we expect that the bnb result to be equal to the exhaustive
/// search result.
pub fn can_eventually_find_best_solution<M>(
    params: StrategyParams,
    candidates: Vec<Candidate>,
    mut metric: M,
) -> Result<(), proptest::test_runner::TestCaseError>
where
    M: BnbMetric + Clone,
{
    println!("== TEST ==");
    println!("{}", type_name::<M>());
    println!("{:?}", params);

    let target = params.target();

    let mut selection = CoinSelector::new(&candidates);
    let mut exp_selection = selection.clone();

    if metric.requires_ordering_by_descending_value_pwu() {
        exp_selection.sort_candidates_by_descending_value_pwu();
    }
    print_candidates(&params, &exp_selection);

    println!("\texhaustive search:");
    let now = std::time::Instant::now();
    let exp_result = exhaustive_search(&mut exp_selection, target, &mut metric);
    let exp_change = metric.drain(&exp_selection, target);
    let exp_result_str = result_string(&exp_result.ok_or("no possible solution"), exp_change);
    println!(
        "\t\telapsed={:8}s result={}",
        now.elapsed().as_secs_f64(),
        exp_result_str
    );
    // bonus check: ensure replacement fee is respected
    if exp_result.is_some() {
        let selected_value = exp_selection.selected_value();
        let drain = metric.drain(&exp_selection, target);
        let target_value = target.value();
        let replace_fee = params
            .replace
            .map(|replace| {
                replace
                    .min_fee_to_do_replacement(exp_selection.weight(target.outputs, drain.weights))
            })
            .unwrap_or(0);
        assert!(selected_value - target_value - drain.value >= replace_fee);
    }

    println!("\tbranch and bound:");
    let now = std::time::Instant::now();
    let mut bnb_metric = metric.clone();
    let result = bnb_search(&mut selection, target, metric, usize::MAX);
    let change = bnb_metric.drain(&selection, target);
    let result_str = result_string(&result, change);
    println!(
        "\t\telapsed={:8}s result={}",
        now.elapsed().as_secs_f64(),
        result_str
    );

    match exp_result {
        Some((score_to_match, _max_rounds)) => {
            let (score, _rounds) = result.expect("must find solution");
            // [todo] how do we check that `_rounds` is less than `_max_rounds` MOST of the time?
            prop_assert_eq!(
                score,
                score_to_match,
                "score:
                    got={}
                    exp={}",
                result_str,
                exp_result_str
            );

            // bonus check: ensure replacement fee is respected
            let selected_value = selection.selected_value();
            let drain = bnb_metric.drain(&selection, target);
            let target_value = target.value();
            let replace_fee = params
                .replace
                .map(|replace| {
                    replace
                        .min_fee_to_do_replacement(selection.weight(target.outputs, drain.weights))
                })
                .unwrap_or(0);
            assert!(selected_value - target_value - drain.value >= replace_fee);
        }
        _ => prop_assert!(result.is_err(), "should not find solution"),
    }

    Ok(())
}

/// Used for constructing a proptest that compares the bound score at every branch with the actual
/// scores of all descendant branches.
///
/// If this fails, it means the metric's bound function is too tight.
pub fn ensure_bound_is_not_too_tight<M>(
    params: StrategyParams,
    candidates: Vec<Candidate>,
    mut metric: M,
) -> Result<(), proptest::test_runner::TestCaseError>
where
    M: BnbMetric,
{
    println!("== TEST ==");
    println!("{}", type_name::<M>());
    println!("{:?}", params);

    let target = params.target();

    let init_cs = {
        let mut cs = CoinSelector::new(&candidates);
        if metric.requires_ordering_by_descending_value_pwu() {
            cs.sort_candidates_by_descending_value_pwu();
        }
        cs
    };
    print_candidates(&params, &init_cs);

    for (cs, _) in ExhaustiveIter::new(&init_cs).into_iter().flatten() {
        if let Some(lb_score) = metric.bound(&cs, target) {
            // This is the branch's lower bound. In other words, this is the BEST selection
            // possible (can overshoot) traversing down this branch. Let's check that!

            if let Some(score) = metric.score(&cs, target) {
                let has_change = metric.drain(&cs, target).is_some();
                prop_assert!(
                    score >= lb_score,
                    "checking branch: selection={} score={} change={} lb={}",
                    cs,
                    score,
                    has_change,
                    lb_score
                );
            }

            for (descendant_cs, _) in ExhaustiveIter::new(&cs)
                .into_iter()
                .flatten()
                .filter(|(_, inc)| *inc)
            {
                if let Some(descendant_score) = metric.score(&descendant_cs, target) {
                    let parent_has_change = metric.drain(&cs, target).is_some();
                    let descendant_has_change = metric.drain(&descendant_cs, target).is_some();
                    prop_assert!(
                        descendant_score >= lb_score,
                        "
                            parent={:8} change={} lb={} target_met={}
                        descendant={:8} change={} score={}
                        ",
                        cs,
                        parent_has_change,
                        lb_score,
                        cs.is_funded(target),
                        descendant_cs,
                        descendant_has_change,
                        descendant_score,
                    );
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct StrategyParams {
    pub n_candidates: usize,
    pub target_value: u64,
    pub n_target_outputs: usize,
    pub target_weight: u32,
    pub replace: Option<Replace>,
    pub feerate: f32,
    pub feerate_lt_diff: f32,
    pub drain_weight: u32,
    pub drain_spend_weight: u32,
    pub drain_dust: u64,
    pub n_drain_outputs: usize,
    pub max_weight: Option<u64>,
}

impl StrategyParams {
    pub fn target(&self) -> Target {
        Target {
            fee: TargetFee {
                rate: FeeRate::from_sat_per_vb(self.feerate),
                replace: self.replace,
                ..TargetFee::ZERO
            },
            outputs: TargetOutputs {
                value_sum: self.target_value,
                weight_sum: self.target_weight as u64,
                n_outputs: self.n_target_outputs,
            },
            max_weight: self.max_weight,
        }
    }

    pub fn feerate(&self) -> FeeRate {
        FeeRate::from_sat_per_vb(self.feerate)
    }

    pub fn long_term_feerate(&self) -> FeeRate {
        FeeRate::from_sat_per_vb((self.feerate + self.feerate_lt_diff).max(1.0))
    }

    pub fn drain_weights(&self) -> DrainWeights {
        DrainWeights {
            output_weight: self.drain_weight as u64,
            spend_weight: self.drain_spend_weight as u64,
            n_outputs: self.n_drain_outputs,
        }
    }

    pub fn dust_relay_feerate(&self) -> FeeRate {
        FeeRate::from_sat_per_vb(3.0)
    }

    pub fn lowest_fee_metric(&self) -> LowestFee {
        LowestFee {
            long_term_feerate: self.long_term_feerate(),
            dust_relay_feerate: self.dust_relay_feerate(),
            drain_weights: self.drain_weights(),
        }
    }
}

pub fn gen_candidates(n: usize) -> Vec<Candidate> {
    let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
    core::iter::repeat_with(move || {
        let value = rng.random_range(1..500_001);
        let weight = rng.random_range(1..2001);
        let input_count = rng.random_range(1..3);
        let is_segwit = rng.random_bool(0.01);

        Candidate {
            value,
            weight,
            input_count,
            is_segwit,
        }
    })
    .take(n)
    .collect()
}

pub fn print_candidates(params: &StrategyParams, cs: &CoinSelector<'_>) {
    println!("\tcandidates:");
    for (i, candidate) in cs.candidates() {
        println!(
            "\t\t{:3} | ev:{:10.2} | vpw:{:10.2} | waste:{:10.2} | {:?}",
            i,
            candidate.effective_value(params.feerate()),
            candidate.value_pwu(),
            candidate.weight as f32 * (params.feerate().spwu() - params.long_term_feerate().spwu()),
            candidate,
        );
    }
}

pub struct ExhaustiveIter<'a> {
    stack: Vec<(CoinSelector<'a>, bool)>, // for branches: (cs, this_index, include?)
}

impl<'a> ExhaustiveIter<'a> {
    pub fn new(cs: &CoinSelector<'a>) -> Option<Self> {
        let mut iter = Self { stack: Vec::new() };
        iter.push_branches(cs);
        Some(iter)
    }

    fn push_branches(&mut self, cs: &CoinSelector<'a>) {
        let next_index = match cs.unselected_indices().next() {
            Some(next_index) => next_index,
            None => return,
        };

        let inclusion_cs = {
            let mut cs = cs.clone();
            assert!(cs.select(next_index));
            cs
        };
        self.stack.push((inclusion_cs, true));

        let exclusion_cs = {
            let mut cs = cs.clone();
            cs.ban(next_index);
            cs
        };
        self.stack.push((exclusion_cs, false));
    }
}

impl<'a> Iterator for ExhaustiveIter<'a> {
    type Item = (CoinSelector<'a>, bool);

    fn next(&mut self) -> Option<Self::Item> {
        let (cs, inclusion) = self.stack.pop()?;
        self.push_branches(&cs);
        Some((cs, inclusion))
    }
}

pub fn exhaustive_search<M>(
    cs: &mut CoinSelector,
    target: Target,
    metric: &mut M,
) -> Option<(Ordf32, usize)>
where
    M: BnbMetric,
{
    if metric.requires_ordering_by_descending_value_pwu() {
        cs.sort_candidates_by_descending_value_pwu();
    }

    let mut best = Option::<(CoinSelector, Ordf32)>::None;
    let mut rounds = 0;

    let iter = ExhaustiveIter::new(cs)?
        .enumerate()
        .inspect(|(i, _)| rounds = *i)
        .filter(|(_, (_, inclusion))| *inclusion)
        .filter_map(|(_, (cs, _))| metric.score(&cs, target).map(|score| (cs, score)));

    for (child_cs, score) in iter {
        match &mut best {
            Some((best_cs, best_score)) => {
                if score < *best_score {
                    *best_cs = child_cs;
                    *best_score = score;
                }
            }
            best => *best = Some((child_cs, score)),
        }
    }

    if let Some((best_cs, score)) = &best {
        println!("\t\tsolution={}, score={}", best_cs, score);
        *cs = best_cs.clone();
    }

    best.map(|(_, score)| (score, rounds))
}

/// Exact feasibility oracle: does *any* subset of the currently-unbanned candidates (added to the
/// current selection) meet `target`, i.e. cover the value **and** stay within `max_weight`?
///
/// Enumerates every subset via [`ExhaustiveIter`] and reuses the real
/// [`CoinSelector::is_funded`] + [`CoinSelector::is_within_max_weight`], so it inherits the
/// exact weight model and is independent of the BnB weight prune it audits. Exponential — small `n`
/// only.
pub fn exact_selection_possible(cs: &CoinSelector, target: Target) -> bool {
    let feasible =
        |s: &CoinSelector| s.is_funded(target) && s.is_within_max_weight(target, Drain::NONE);
    // the current selection itself (no additions) is a valid subset and isn't yielded by the iter
    feasible(cs)
        || ExhaustiveIter::new(cs)
            .map(|mut iter| iter.any(|(subset, _)| feasible(&subset)))
            .unwrap_or(false)
}

pub fn bnb_search<M>(
    cs: &mut CoinSelector,
    target: Target,
    metric: M,
    max_rounds: usize,
) -> Result<(Ordf32, usize), NoBnbSolution>
where
    M: BnbMetric,
{
    let mut rounds = 0_usize;
    let (selection, score) = cs
        .bnb_solutions(target, metric)
        .inspect(|_| rounds += 1)
        .take(max_rounds)
        .flatten()
        .last()
        .ok_or(NoBnbSolution { max_rounds, rounds })?;
    println!("\t\tsolution={}, score={}", selection, score);
    *cs = selection;

    Ok((score, rounds))
}

pub fn result_string<E>(res: &Result<(Ordf32, usize), E>, change: Drain) -> String
where
    E: std::fmt::Debug,
{
    match res {
        Ok((score, rounds)) => {
            let drain = if change.is_some() {
                format!("{:?}", change)
            } else {
                "None".to_string()
            };
            format!("Ok(score={} rounds={} drain={})", score, rounds, drain)
        }
        err => format!("{:?}", err),
    }
}

pub fn compare_against_benchmarks<M: BnbMetric + Clone>(
    params: StrategyParams,
    candidates: Vec<Candidate>,
    mut metric: M,
) -> Result<(), TestCaseError> {
    println!("=======================================");
    let start = std::time::Instant::now();
    let mut rng = TestRng::deterministic_rng(RngAlgorithm::ChaCha);
    let target = params.target();
    let cs = CoinSelector::new(&candidates);
    let solutions = cs.bnb_solutions(target, metric.clone());

    let best = solutions
        .enumerate()
        .filter_map(|(i, sol)| Some((i, sol?)))
        .last();

    match best {
        Some((_i, (sol, _score))) => {
            let mut cmp_benchmarks = vec![
                {
                    let mut naive_select = cs.clone();
                    naive_select.sort_candidates_by_key(|(_, wv)| {
                        core::cmp::Reverse(Ordf32(wv.effective_value(target.fee.rate)))
                    });
                    // we filter out failing onces below
                    let _ = naive_select.select_until_target_met(target);
                    naive_select
                },
                {
                    let mut all_selected = cs.clone();
                    all_selected.select_all();
                    all_selected
                },
                {
                    let mut all_effective_selected = cs.clone();
                    all_effective_selected.select_all_effective(target.fee.rate);
                    all_effective_selected
                },
                {
                    // Lightest value-meeting greedy selection. Under a binding `max_weight` the
                    // bulk benchmarks above are all over-cap (score `None`) and get filtered out;
                    // this one is the relevant baseline that stays feasible when a light solution
                    // exists, so the comparison below isn't vacuous.
                    let mut greedy = cs.clone();
                    greedy.sort_candidates_by_descending_value_pwu();
                    let _ = greedy.select_until_target_met(target);
                    greedy
                },
            ];

            // add some random selections -- technically it's possible that one of these is better but it's very unlikely if our algorithm is working correctly.
            cmp_benchmarks.extend(
                (0..10).map(|_| randomly_satisfy_target(&cs, target, &mut rng, metric.clone())),
            );

            // Only compare against benchmarks that are themselves *valid* solutions. A benchmark
            // can meet the target value yet bust `max_weight` (e.g. `select_all` on a tight cap),
            // in which case its score is `None` and it isn't a real solution to compare against.
            let cmp_benchmarks = cmp_benchmarks
                .into_iter()
                .filter_map(|cs| {
                    let score = metric.clone().score(&cs, target)?;
                    Some((cs, score))
                })
                .collect::<Vec<_>>();
            let sol_score = metric.score(&sol, target);

            for (_bench_id, (mut bench, bench_score)) in cmp_benchmarks.into_iter().enumerate() {
                prop_assert!(
                    sol_score.is_some(),
                    "bnb must be able to find solution if benchmark can"
                );
                let sol_score = sol_score.expect("must be some");

                if sol_score > bench_score {
                    dbg!(_bench_id);
                    println!("bnb solution: {}", sol);
                    bench.sort_candidates_by_descending_value_pwu();
                    println!("found better: {}", bench);
                }
                prop_assert!(sol_score <= bench_score);
            }
        }
        None => {
            // Full feasibility (value *and* max_weight) is needed here; `is_fundable`
            // only covers value, so use the exact exhaustive oracle to assert impossibility.
            prop_assert!(!exact_selection_possible(&cs, target));
        }
    }

    dbg!(start.elapsed());
    Ok(())
}

#[allow(unused)]
fn randomly_satisfy_target<'a, R: rand::Rng>(
    cs: &CoinSelector<'a>,
    target: Target,
    rng: &mut R,
    mut metric: impl BnbMetric,
) -> CoinSelector<'a> {
    let mut cs = cs.clone();

    let mut last_score: Option<Ordf32> = None;
    while let Some(next) = cs.unselected_indices().choose(rng) {
        cs.select(next);
        if cs.is_funded(target) {
            let curr_score = metric.score(&cs, target);
            if let Some(last_score) = last_score {
                if curr_score.is_none() || curr_score.unwrap() > last_score {
                    break;
                }
            }
            last_score = curr_score;
        }
    }
    cs
}
