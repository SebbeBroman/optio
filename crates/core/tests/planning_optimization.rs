use optio_core::lp::{LpOptions, LpRelaxation, LpStatus};
use optio_core::model::check_solution;
use optio_core::{Model, StdClock, VarType, INF};

#[test]
fn child_basis_reoptimization_matches_cold_for_changed_bounds() {
    let mut m = Model::new();
    let x = m.add_col(0.0, 3.0);
    let y = m.add_col(0.0, 3.0);
    m.set_cost(x, -1.0);
    m.set_cost(y, -2.0);
    m.add_row(-INF, 4.0, &[(x.0, 1.0), (y.0, 1.0)]);
    let mut lp = LpRelaxation::build(&m);
    let root = lp.solve(&LpOptions::default(), &StdClock::new());
    assert_eq!(root.status, LpStatus::Optimal);
    let basis = root.basis.as_deref().expect("clean optimal basis");
    let mut reused = 0;
    for (lo, hi) in [
        ([0.0, 0.0], [0.0, 3.0]),
        ([0.0, 0.0], [3.0, 1.0]),
        ([2.0, 2.0], [3.0, 3.0]),
        ([3.0, 3.0], [3.0, 3.0]),
        ([3.0, 0.0], [2.0, 3.0]),
    ] {
        let clock = StdClock::new();
        let warm = lp.solve_with_basis(&lo, &hi, &LpOptions::default(), &clock, Some(basis));
        let cold = lp.solve_with_bounds(&lo, &hi, &LpOptions::default(), &clock);
        assert_eq!(warm.status, cold.status);
        if warm.status == LpStatus::Optimal {
            assert!((warm.objective - cold.objective).abs() < 1e-6);
            let mut child = m.clone();
            child.col_lower = lo.to_vec();
            child.col_upper = hi.to_vec();
            assert!(check_solution(&child, &warm.col_value, 1e-6).feasible);
            assert!(warm.dual_bound.unwrap() <= warm.objective + 1e-8);
        }
        reused += warm.stats.warm_solves;
    }
    assert!(
        reused >= 2,
        "warm path must be exercised, not only its fallback"
    );
}

#[test]
fn optimized_mip_matches_exhaustive_binary_optima() {
    use optio_core::{solve, ModelStatus, Options};
    let mut seed = 17u64;
    let mut rnd = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 32) as u32
    };
    for _ in 0..30 {
        let mut m = Model::new();
        for _ in 0..6 {
            let x = m.add_col(0.0, 1.0);
            m.set_integrality(x, VarType::Integer);
            m.set_cost(x, -((rnd() % 10 + 1) as f64));
        }
        for _ in 0..3 {
            let row: Vec<_> = (0..6).map(|j| (j, (rnd() % 9 + 1) as f64)).collect();
            m.add_row(-INF, (rnd() % 15 + 5) as f64, &row);
        }
        let best = (0..64)
            .filter_map(|bits| {
                let x: Vec<_> = (0..6).map(|j| ((bits >> j) & 1) as f64).collect();
                let c = check_solution(&m, &x, 1e-8);
                c.feasible.then_some(c.objective)
            })
            .fold(f64::INFINITY, f64::min);
        for threads in [1, 2] {
            let s = solve(
                &m,
                &Options {
                    threads,
                    time_limit_secs: 3.0,
                    mip_rel_gap: 0.0,
                    ..Default::default()
                },
                &StdClock::new(),
            );
            assert_eq!(s.status, ModelStatus::Optimal);
            assert!(
                (s.objective - best).abs() < 1e-6,
                "wrong optimum {} vs {}",
                s.objective,
                best
            );
            assert!(s.dual_bound.unwrap() <= best + 1e-7);
            assert!(check_solution(&m, &s.col_value, 1e-6).feasible);
        }
    }
}

#[test]
fn narrow_box_does_not_cycle_on_zero_length_bound_flips() {
    let mut m = Model::new();
    let x = m.add_col(0.0, 1e-11);
    m.set_cost(x, -1.0);
    let s = LpRelaxation::build(&m).solve(&LpOptions::default(), &StdClock::new());
    assert_eq!(s.status, LpStatus::Optimal);
    assert!(s.iterations < 5);
    assert!((s.objective + 1e-11).abs() < 1e-15);
}

#[test]
fn tolerance_feasible_seed_does_not_make_objective_domains_empty() {
    use optio_core::{solve_with_start, Callbacks, ModelStatus, Options};
    let mut m = Model::new();
    let b = m.add_col(1.0, 1.0);
    m.set_integrality(b, VarType::Integer);
    let y = m.add_col(0.0, 2.0);
    m.set_cost(y, 1.0);
    m.add_row(0.0, INF, &[(y.0, 1.0), (b.0, -1.0)]);
    let s = solve_with_start(
        &m,
        &Options::default(),
        &StdClock::new(),
        &mut Callbacks::default(),
        Some(&[1.0, 1.0 - 5e-7]),
    );
    assert_ne!(s.status, ModelStatus::Infeasible);
    assert!(check_solution(&m, &s.col_value, 1e-6).feasible);
}
