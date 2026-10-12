//! Native entry point.
//!
//! Two modes:
//! - default: build a tiny demo model and validate it through the oracle;
//! - `--model-json <file> [--time <secs>] [--gap <rel>] [--lp]`: read a detached
//!   model (`highs-js` `getModel('csr')` shape) and solve it, printing one line
//!   of JSON. This is the contract the differential harness drives.

use optio_core::lp::{LpOptions, LpRelaxation, LpStatus};
use optio_core::model::{check_solution, Model, VarType, INF};
use optio_core::solve::{solve, solve_with, Callbacks};
use optio_core::{Clock, Options, StdClock};
use serde_json::Value;

fn demo_model() -> Model {
    // min -x - 10y  s.t. x + 7y <= 17.5, 0<=x<=3.5, y>=0 integer.
    let mut m = Model::new();
    let x = m.add_col(0.0, 3.5);
    let y = m.add_col(0.0, INF);
    m.set_cost(x, -1.0);
    m.set_cost(y, -10.0);
    m.set_integrality(y, VarType::Integer);
    m.add_row(-INF, 17.5, &[(x.0, 1.0), (y.0, 7.0)]);
    m
}

fn demo() {
    let model = demo_model();
    println!(
        "model: {} cols, {} rows, {} nnz",
        model.num_col,
        model.num_row,
        model.num_nz()
    );

    let check = check_solution(&model, &[3.5, 2.0], 1e-9);
    println!(
        "oracle on (3.5, 2.0): feasible={} objective={} max_row_viol={}",
        check.feasible, check.objective, check.max_row_violation
    );

    let options = Options::default();
    let clock = StdClock::new();
    let sol = solve(&model, &options, &clock);
    println!(
        "solve status={:?} objective={} has_solution={}",
        sol.status,
        sol.objective,
        sol.has_solution()
    );
}

fn json_num_vec(v: &Value) -> Vec<f64> {
    // Accept a plain array, or the numeric-keyed object form that Node produces
    // when `JSON.stringify`-ing a typed array (`{"0":..,"1":..,"length":..}`).
    let mut out: Vec<f64> = Vec::new();
    if let Some(a) = v.as_array() {
        out.extend(a.iter().map(|x| x.as_f64().unwrap_or(0.0)));
        return out;
    }
    if let Some(o) = v.as_object() {
        let mut i = 0usize;
        while let Some(x) = o.get(&i.to_string()) {
            out.push(x.as_f64().unwrap_or(0.0));
            i += 1;
        }
    }
    out
}

fn as_f64_array(v: &Value, name: &str) -> Vec<f64> {
    v.get(name).map(json_num_vec).unwrap_or_default()
}

fn as_u32_array(v: &Value, name: &str) -> Vec<u32> {
    v.get(name)
        .map(json_num_vec)
        .unwrap_or_default()
        .into_iter()
        .map(|x| x as u32)
        .collect()
}

/// Build a `Model` from a `highs-js` detached model (`getModel('csr')`).
/// `cost_sign` is `+1` for minimisation, `-1` for maximisation (the solver
/// always minimises; the reported objective is negated back by the caller).
fn model_from_json(v: &Value, cost_sign: f64) -> Model {
    let num_col = v.get("numCols").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let num_row = v.get("numRows").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    let cost = as_f64_array(v, "colCost");
    let lower = as_f64_array(v, "colLower");
    let upper = as_f64_array(v, "colUpper");
    let row_lo = as_f64_array(v, "rowLower");
    let row_hi = as_f64_array(v, "rowUpper");
    let integrality = as_u32_array(v, "integrality");

    let mut m = Model::new();
    for c in 0..num_col {
        m.add_col(
            lower.get(c).copied().unwrap_or(0.0),
            upper.get(c).copied().unwrap_or(INF),
        );
        m.set_cost(
            optio_core::VarId(c as u32),
            cost_sign * cost.get(c).copied().unwrap_or(0.0),
        );
        let ty = match integrality.get(c).copied().unwrap_or(0) {
            0 => VarType::Continuous,
            2 => VarType::Binary,
            _ => VarType::Integer,
        };
        m.set_integrality(optio_core::VarId(c as u32), ty);
    }

    let matrix = v.get("matrix").cloned().unwrap_or(Value::Null);
    let starts = as_u32_array(&matrix, "starts");
    let indices = as_u32_array(&matrix, "indices");
    let values = as_f64_array(&matrix, "values");
    for r in 0..num_row {
        let lo = starts.get(r).copied().unwrap_or(0) as usize;
        let hi = starts.get(r + 1).copied().unwrap_or(lo as u32) as usize;
        let entries: Vec<(u32, f64)> = (lo..hi)
            .map(|k| {
                (
                    indices.get(k).copied().unwrap_or(0),
                    values.get(k).copied().unwrap_or(0.0),
                )
            })
            .collect();
        m.add_row(
            row_lo.get(r).copied().unwrap_or(-INF),
            row_hi.get(r).copied().unwrap_or(INF),
            &entries,
        );
    }
    m
}

fn run_json(path: &str, time: f64, gap: f64, lp_only: bool, threads: usize, presolve: bool) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            println!("{{\"error\":\"read {}: {}\"}}", path, e);
            return;
        }
    };
    let v: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            println!("{{\"error\":\"parse {}: {}\"}}", path, e);
            return;
        }
    };
    let sense = v.get("sense").and_then(|x| x.as_i64()).unwrap_or(1);
    let sign = if sense < 0 { -1.0 } else { 1.0 };
    let offset = v.get("offset").and_then(|x| x.as_f64()).unwrap_or(0.0);
    let model = model_from_json(&v, sign);
    let clock = StdClock::new();

    if lp_only {
        let build_t0 = clock.now_secs();
        let mut lpr = LpRelaxation::build(&model);
        let build_ms = (clock.now_secs() - build_t0) * 1000.0;
        let opts = LpOptions {
            max_iterations: 500_000,
            time_limit_secs: time,
            ..LpOptions::default()
        };
        let solve_t0 = clock.now_secs();
        let sol = lpr.solve(&opts, &clock);
        let solve_ms = (clock.now_secs() - solve_t0) * 1000.0;
        let feasible = sol.status == LpStatus::Optimal;
        let out = serde_json::json!({
            "statusCode": match sol.status {
                LpStatus::Optimal => 7,
                LpStatus::Infeasible => 8,
                LpStatus::Unbounded => 10,
                LpStatus::TimeLimit => 13,
                LpStatus::IterationLimit => 14,
                LpStatus::Numerical => 15,
            },
            "objective": if feasible { Some(sign * sol.objective + offset) } else { None },
            "dualBound": sol.dual_bound.map(|b| sign * b + offset),
            "colValue": sol.col_value,
            "nodes": 0,
            "iterations": sol.iterations,
            "ms": solve_ms,
            "buildMs": build_ms,
            "maxRowViolation": Value::Null,
        });
        println!("{}", out);
        return;
    }

    let options = Options {
        time_limit_secs: time,
        mip_rel_gap: gap,
        output_flag: false,
        threads,
        presolve,
        lp_warm_start: !std::env::args().any(|s| s == "--cold-nodes"),
        mip_cuts: !std::env::args().any(|s| s == "--no-cuts"),
        mip_pseudocost: !std::env::args().any(|s| s == "--no-pseudocost"),
        mip_node_presolve: !std::env::args().any(|s| s == "--no-node-presolve"),
        mip_column_compression: std::env::args().any(|s| s == "--column-compression")
            && !std::env::args().any(|s| s == "--no-column-compression"),
        mip_objective_counts: !std::env::args().any(|s| s == "--no-objective-counts"),
        mip_objective_lattice: !std::env::args().any(|s| s == "--no-objective-lattice"),
        mip_strong_branch: !std::env::args().any(|s| s == "--no-strong-branch"),
        adaptive_search: std::env::args().any(|s| s == "--adaptive"),
        ..Options::default()
    };
    let solve_t0 = clock.now_secs();
    let mut incumbents = Vec::new();
    let mut on_incumbent = |inc: &optio_core::solve::Incumbent| {
        incumbents.push(serde_json::json!({"ms": (clock.now_secs()-solve_t0)*1000.0, "objective": sign*inc.objective+offset}));
    };
    let sol = solve_with(
        &model,
        &options,
        &clock,
        &mut Callbacks {
            on_incumbent: Some(&mut on_incumbent),
            ..Default::default()
        },
    );
    let solve_ms = (clock.now_secs() - solve_t0) * 1000.0;
    let check = if sol.has_solution() {
        Some(check_solution(&model, &sol.col_value, 1e-6))
    } else {
        None
    };
    let out = serde_json::json!({
        "statusCode": sol.status.as_i32(),
        "objective": if sol.has_solution() { Some(sign * sol.objective + offset) } else { None },
        "dualBound": sol.dual_bound.map(|b| sign * b + offset),
        "threads": threads,
        "gap": sol.dual_bound.filter(|_| sol.has_solution()).map(|b| ((sol.objective - b) / sol.objective.abs().max(1e-10)).max(0.0)),
        "colValue": sol.col_value,
        "nodes": sol.nodes,
        "iterations": sol.lp_iterations,
        "ms": solve_ms,
        "timing": { "presolve": sol.presolve_secs, "heuristic": sol.heuristic_secs, "localSearch": sol.local_search_secs, "cuts": sol.cut_secs,
            "boundCertificate": sol.bound_secs, "branchAndBound": sol.bb_secs, "nodeLpWorkerSeconds": sol.lp_stats.seconds },
        "lpStats": { "solves": sol.lp_stats.solves, "phase1Iterations": sol.lp_stats.phase1_iterations, "phase2Iterations": sol.lp_stats.phase2_iterations, "warmSolves": sol.lp_stats.warm_solves, "warmFallbacks": sol.lp_stats.warm_fallbacks, "warmInvalidBasis": sol.lp_stats.warm_invalid_basis, "warmSingularBasis": sol.lp_stats.warm_singular_basis, "warmResidualFailures": sol.lp_stats.warm_residual_failures, "warmDualFailures": sol.lp_stats.warm_dual_failures, "warmNoPivot": sol.lp_stats.warm_no_pivot, "warmPivotFailures": sol.lp_stats.warm_pivot_failures, "warmIterationLimits": sol.lp_stats.warm_iteration_limits, "warmBoundFailures": sol.lp_stats.warm_bound_failures, "warmBoundFlips": sol.lp_stats.warm_bound_flips },
        "mipStats": { "compressedColumns": sol.mip_stats.compressed_columns, "objectiveCountColumns": sol.mip_stats.objective_count_columns, "propagationPrunes": sol.mip_stats.propagation_prunes, "propagationSeconds": sol.mip_stats.propagation_seconds, "strongBranchProbes": sol.mip_stats.strong_branch_probes, "numericalRetries": sol.mip_stats.numerical_retries },
        "incumbents": incumbents,
        "rootCuts": sol.root_cuts,
        "inferredIntegerCols": sol.inferred_integer_cols,
        "maxRowViolation": check.map(|c| c.max_row_violation),
        "maxBoundViolation": check.map(|c| c.max_bound_violation),
        "maxIntegralityViolation": check.map(|c| c.max_integrality_violation),
    });
    println!("{}", out);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut path: Option<String> = None;
    let mut time = 10.0f64;
    let mut gap = 1e-4f64;
    let mut lp_only = false;
    let mut threads = 1usize;
    let mut presolve = true;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--model-json" => {
                i += 1;
                path = args.get(i).cloned();
            }
            "--time" => {
                i += 1;
                time = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(time);
            }
            "--gap" => {
                i += 1;
                gap = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(gap);
            }
            "--threads" => {
                i += 1;
                threads = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(threads);
            }
            "--no-presolve" => presolve = false,
            "--lp" => lp_only = true,
            _ => {}
        }
        i += 1;
    }

    match path {
        Some(p) => run_json(&p, time, gap, lp_only, threads, presolve),
        None => demo(),
    }
}
