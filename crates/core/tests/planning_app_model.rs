//! Regression tests for the *planning* MILP shape.
//!
//! The app's model has integer placement variables plus continuous auxiliaries
//! (`peak`/`min`/`range`/`dev`) whose definition rows carry large coefficients
//! (e.g. `720·x - min >= 0`). The simplex equilibrates rows, so its `1e-7`
//! tolerance can be ~1e-4 in **original** units (`check_solution` uses `1e-6`).
//! A rounded LP point therefore looked infeasible and every integer candidate
//! was rejected — the app saw `NoSolution`. The fix projects the continuous
//! variables onto their rows (`heuristic::polish`) before validating.
//!
//! `app_shaped_model()` is a faithful, minimal reproducer of the in-memory
//! model that triggered the bug (1 group, 8 boxers, matchesPerDay 2, 2 days,
//! 1 ring): 12 columns, 15 rows, same column/row order as the app.
use optio_core::heuristic::find_feasible;
use optio_core::mip::branch_and_bound;
use optio_core::solve::solve_with_start;
use optio_core::status::ModelStatus;
use optio_core::{check_solution, solve, Callbacks, Model, Options, StdClock, VarId, VarType, INF};
use std::cell::Cell;

/// The exact app-shaped model (placement + continuous auxiliaries). `total` is
/// the `tot` row RHS; `7` is feasible, `8` is genuinely infeasible (`day_*`
/// rows force `x1=4y`, `x2=3y`, so `x1+x2 <= 7`).
fn app_shaped_model(total: f64) -> Model {
    let mut m = Model::new();
    // Columns, in the app's order.
    let y = m.add_col(0.0, 1.0);
    let x1 = m.add_col(0.0, 4.0);
    let xr_qf = m.add_col(0.0, 4.0);
    let x2 = m.add_col(0.0, 3.0);
    let xr_final = m.add_col(0.0, 1.0);
    let xr_semi = m.add_col(0.0, 2.0);
    let peak1 = m.add_col(0.0, 1e9);
    let min1 = m.add_col(0.0, 1e9);
    let range1 = m.add_col(0.0, 1e9);
    let peak2 = m.add_col(0.0, 1e9);
    let min2 = m.add_col(0.0, 1e9);
    let range2 = m.add_col(0.0, 1e9);
    for c in [y, x1, xr_qf, x2, xr_final, xr_semi] {
        m.set_integrality(c, VarType::Integer);
    }
    // Objective: prefer day 1, then balance (peak/range).
    m.set_cost(y, -0.1);
    m.set_cost(peak1, 0.05);
    m.set_cost(range1, 1.0);
    m.set_cost(peak2, 0.05);
    m.set_cost(range2, 1.0);

    let c = |v: VarId| v.0;
    // Rows, in the app's order.
    m.add_row(1.0, 1.0, &[(c(y), 1.0)]); // yone
    m.add_row(0.0, 0.0, &[(c(x1), 1.0), (c(xr_qf), -1.0)]); // roundlink 1
    m.add_row(
        0.0,
        0.0,
        &[(c(x2), 1.0), (c(xr_final), -1.0), (c(xr_semi), -1.0)],
    ); // roundlink 2
    m.add_row(total, total, &[(c(x1), 1.0), (c(x2), 1.0)]); // tot
    m.add_row(0.0, 0.0, &[(c(x1), 1.0), (c(y), -4.0)]); // day 0
    m.add_row(0.0, 0.0, &[(c(xr_qf), 1.0), (c(y), -4.0)]); // roundday qf
    m.add_row(0.0, 0.0, &[(c(x2), 1.0), (c(y), -3.0)]); // day 1
    m.add_row(0.0, 0.0, &[(c(xr_final), 1.0), (c(y), -1.0)]); // roundday final
    m.add_row(0.0, 0.0, &[(c(xr_semi), 1.0), (c(y), -2.0)]); // roundday semi
    m.add_row(
        0.0,
        INF,
        &[(c(range1), 1.0), (c(peak1), -1.0), (c(min1), 1.0)],
    ); // rangerel 1
    m.add_row(-INF, 0.0, &[(c(x1), 720.0), (c(peak1), -1.0)]); // peakge 1
    m.add_row(0.0, INF, &[(c(x1), 720.0), (c(min1), -1.0)]); // minle 1
    m.add_row(
        0.0,
        INF,
        &[(c(range2), 1.0), (c(peak2), -1.0), (c(min2), 1.0)],
    ); // rangerel 2
    m.add_row(-INF, 0.0, &[(c(x2), 720.0), (c(peak2), -1.0)]); // peakge 2
    m.add_row(0.0, INF, &[(c(x2), 720.0), (c(min2), -1.0)]); // minle 2
    m
}

/// The heuristic's LP-rounding path must accept the valid placement. Before the
/// `polish` fix the rounded LP point carried a ~1e-4 `min` residual and
/// `find_feasible` fell back to min-conflicts, which stalls on this coupled
/// structure and returned `None` (the app's `NoSolution`).
#[test]
fn find_feasible_accepts_app_model_placement() {
    let m = app_shaped_model(7.0);
    let opts = Options {
        time_limit_secs: 5.0,
        ..Options::default()
    };
    let f = find_feasible(&m, &opts, &StdClock::new(), 200_000)
        .expect("find_feasible must return a feasible app-model plan");
    // Independent oracle, in original units.
    let chk = check_solution(&m, &f.x, 1e-6);
    assert!(chk.feasible, "check_solution rejected the plan: {chk:?}");
    // The placement is the unique valid one: y=1, x1=4, x2=3, finals day 1.
    assert!((f.x[0] - 1.0).abs() < 1e-9, "y={}", f.x[0]);
    assert!((f.x[1] - 4.0).abs() < 1e-9, "x1={}", f.x[1]);
    assert!((f.x[3] - 3.0).abs() < 1e-9, "x2={}", f.x[3]);
}

/// B&B must solve the app model to optimality (the root LP is integral), using
/// polished candidates as incumbents. Before the fix it returned `Unknown` with
/// no solution.
#[test]
fn branch_and_bound_solves_app_model() {
    let m = app_shaped_model(7.0);
    let opts = Options {
        time_limit_secs: 5.0,
        ..Options::default()
    };
    let sol = solve(&m, &opts, &StdClock::new());
    assert!(sol.has_solution(), "no solution: status {:?}", sol.status);
    let chk = check_solution(&m, &sol.col_value, 1e-6);
    assert!(
        chk.feasible,
        "check_solution rejected B&B solution: {chk:?}"
    );
    // The LP/objective optimum for this instance is 251.9 (x1=4,x2=3,range=0).
    assert!(
        (chk.objective - 251.9).abs() < 1e-2,
        "obj={}",
        chk.objective
    );
}

/// B&B's own root-node rounding + polishing must find an incumbent even with no
/// heuristic warm start. Before the fix the rounded root LP point was rejected
/// (the ~1e-4 `min` residual), so B&B returned `Unknown` with no solution.
#[test]
fn branch_and_bound_rounding_polishes_root_candidate() {
    let m = app_shaped_model(7.0);
    let opts = Options {
        time_limit_secs: 5.0,
        ..Options::default()
    };
    let mut cb = Callbacks::default();
    // `None` root_lp and `None` warm start: B&B must solve the root itself.
    let sol = branch_and_bound(&m, &opts, &StdClock::new(), &mut cb, None, None);
    assert!(
        sol.has_solution(),
        "B&B found no incumbent: {:?}",
        sol.status
    );
    assert!(
        check_solution(&m, &sol.col_value, 1e-6).feasible,
        "B&B incumbent failed the oracle"
    );
}

/// Polishing must not hide a genuine infeasibility: with `tot = 8` the day
/// rows make `x1 + x2 = 7`, so no integer point is feasible. `find_feasible`
/// must return `None` and the solver must not report a feasible incumbent.
#[test]
fn polish_does_not_hide_real_infeasibility() {
    let m = app_shaped_model(8.0);
    let opts = Options {
        time_limit_secs: 3.0,
        ..Options::default()
    };
    let f = find_feasible(&m, &opts, &StdClock::new(), 200_000);
    assert!(
        f.is_none(),
        "heuristic returned an incumbent on an infeasible model"
    );
    let sol = solve(&m, &opts, &StdClock::new());
    assert!(
        !sol.has_solution(),
        "solver reported a feasible incumbent on an infeasible model"
    );
}

#[test]
fn placement_start_is_reported_and_can_stop_before_search() {
    let m = app_shaped_model(7.0);
    let seed = vec![1.0, 4.0, 4.0, 3.0, 1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    assert!(!check_solution(&m, &seed, 1e-6).feasible);
    let mut candidates = Vec::new();
    let mut incumbent = |inc: &optio_core::solve::Incumbent| candidates.push(inc.col_value.clone());
    let stop = Cell::new(false);
    let mut progress = |p: &optio_core::solve::Progress| {
        assert!(p.has_incumbent);
        stop.set(true);
    };
    let interrupt = || stop.get();
    let mut callbacks = Callbacks {
        on_incumbent: Some(&mut incumbent),
        on_progress: Some(&mut progress),
        interrupt: Some(&interrupt),
    };
    let sol = solve_with_start(
        &m,
        &Options::default(),
        &StdClock::new(),
        &mut callbacks,
        Some(&seed),
    );
    drop(callbacks);
    assert_eq!(sol.status, ModelStatus::Interrupt);
    assert_eq!(sol.nodes, 0, "a caller-accepted start needs no B&B search");
    assert_eq!(sol.lp_iterations, 0);
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        &candidates[0][..6],
        &seed[..6],
        "polish changed integer placements"
    );
    assert!(check_solution(&m, &candidates[0], 1e-6).feasible);
}

#[test]
fn invalid_start_does_not_discard_the_heuristic_incumbent() {
    let m = app_shaped_model(7.0);
    let mut callbacks = Callbacks::default();
    let seed = vec![0.0; m.num_col];
    let sol = solve_with_start(
        &m,
        &Options::default(),
        &StdClock::new(),
        &mut callbacks,
        Some(&seed),
    );
    assert!(sol.has_solution());
    assert!(check_solution(&m, &sol.col_value, 1e-6).feasible);
}

#[test]
fn non_finite_candidates_are_never_feasible() {
    let m = app_shaped_model(7.0);
    let sol = solve(&m, &Options::default(), &StdClock::new());
    assert!(sol.has_solution());
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        for col in [0, 6] {
            let mut x = sol.col_value.clone();
            x[col] = invalid;
            assert!(!check_solution(&m, &x, 1e-6).feasible);
        }
    }
}
