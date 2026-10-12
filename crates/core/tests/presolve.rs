use optio_core::{
    solve, solve_with, Callbacks, Clock, Model, ModelStatus, Options, StdClock, VarType,
};

fn chain() -> Model {
    let mut m = Model::new();
    let a = m.add_col(1.0, 1.0);
    let b = m.add_col(0.0, 1.0);
    let c = m.add_col(0.0, 1.0);
    for j in [a, b, c] {
        m.set_integrality(j, VarType::Integer);
    }
    m.set_cost(c, -3.0);
    m.add_row(1.0, 1.0, &[(b.0, 1.0), (c.0, 1.0)]);
    m.add_row(1.0, 1.0, &[(a.0, 1.0), (b.0, 1.0)]);
    m
}
#[test]
fn fixed_solution_keeps_original_coordinates_and_emits_incumbent() {
    let m = chain();
    let mut seen = Vec::new();
    let mut cb = |i: &optio_core::Incumbent| seen.push(i.col_value.clone());
    let mut callbacks = Callbacks {
        on_incumbent: Some(&mut cb),
        ..Default::default()
    };
    let s = solve_with(&m, &Options::default(), &StdClock::new(), &mut callbacks);
    assert_eq!(s.status, ModelStatus::Optimal);
    assert_eq!(s.nodes, 0);
    assert_eq!(s.lp_iterations, 0);
    assert_eq!(s.col_value, vec![1.0, 0.0, 1.0]);
    assert_eq!(s.dual_bound, Some(-3.0));
    assert_eq!(seen, vec![s.col_value]);
}
#[test]
fn presolve_switch_does_not_change_optimum() {
    let m = chain();
    for threads in [1, 2] {
        let s = solve(
            &m,
            &Options {
                presolve: false,
                threads,
                ..Default::default()
            },
            &StdClock::new(),
        );
        assert_eq!(s.status, ModelStatus::Optimal);
        assert!((s.objective + 3.0).abs() < 1e-6);
        assert!(s.dual_bound.unwrap() <= s.objective + 1e-9);
    }
}
#[test]
fn true_cost_bound_does_not_exceed_known_integer_optimum() {
    let mut m = Model::new();
    let x = m.add_col(0.0, 1.0);
    let y = m.add_col(0.0, 1.0);
    m.set_integrality(x, VarType::Integer);
    m.set_integrality(y, VarType::Integer);
    m.set_cost(x, -2.0);
    m.set_cost(y, -1.0);
    m.add_row(-optio_core::INF, 1.5, &[(x.0, 1.0), (y.0, 1.0)]);
    for threads in [1, 2] {
        let s = solve(
            &m,
            &Options {
                threads,
                mip_rel_gap: 0.0,
                ..Default::default()
            },
            &StdClock::new(),
        );
        assert_eq!(s.status, ModelStatus::Optimal);
        assert_eq!(s.objective, -2.0);
        assert!(s.dual_bound.unwrap() <= -2.0 + 1e-9);
    }
}
struct Expired;
impl Clock for Expired {
    fn now_secs(&self) -> f64 {
        1000.0
    }
}
#[test]
fn interrupted_lp_cannot_be_reported_as_proven_optimal() {
    let m = chain();
    // Direct B&B exercises the parallel worker's unresolved-root path.
    let s = optio_core::mip::branch_and_bound(
        &m,
        &Options {
            threads: 2,
            time_limit_secs: -1.0,
            ..Default::default()
        },
        &Expired,
        &mut Callbacks::default(),
        None,
        None,
    );
    assert_ne!(s.status, ModelStatus::Optimal);
    assert_ne!(s.status, ModelStatus::Infeasible);
}
