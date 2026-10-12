//! A relaxation over original choices, continuous auxiliaries and integer
//! group loads. It drops placement restrictions; only a checked original
//! witness can close its bound. No sampled patterns enter this certificate.
use super::{detect, gcd, row, Shape};
use crate::{
    model::{check_solution, Model, VarType, INF},
    options::Options,
    solve::{Callbacks, Clock},
    status::ModelStatus,
};
use std::collections::BTreeMap;

struct Demand {
    choices: Vec<(usize, f64)>,
    feeders: Vec<usize>,
    session: usize,
    duration: f64,
}

struct Relaxation {
    model: Model,
    kept: Vec<usize>,
    loads: Vec<usize>,
    units: Vec<f64>,
    demands: Vec<Demand>,
    shape: Shape,
}

pub(crate) struct Proof {
    pub bound: f64,
    pub point: Option<Vec<f64>>,
    pub nodes: u64,
    pub iterations: u64,
    pub lp_stats: crate::lp::LpStats,
    pub mip_stats: crate::solve::MipStats,
}

fn exact_integer(x: f64) -> Option<i64> {
    (x.is_finite() && x.fract() == 0. && x.abs() <= 1_000_000_000.).then_some(x as i64)
}

fn add_row(model: &mut Model, lo: f64, hi: f64, terms: BTreeMap<usize, f64>) {
    let terms: Vec<_> = terms
        .into_iter()
        .filter(|&(_, v)| v != 0.)
        .map(|(j, v)| (j as u32, v))
        .collect();
    model.add_row(lo, hi, &terms);
}

fn build(original: &Model, clock: &dyn Clock, deadline: f64) -> Option<Relaxation> {
    // Reject duplicate CSR entries rather than inferring a complete load
    // expression from a term count that could hide a missing feeder.
    let mut seen = vec![usize::MAX; original.num_col];
    for r in 0..original.num_row {
        for (j, _) in row(original, r) {
            if seen[j] == r {
                return None;
            }
            seen[j] = r;
        }
    }
    let shape = detect(original, clock, deadline, 0)?;
    if shape.groups.len() > 128 {
        return None;
    }
    let n = original.num_col;
    let mut feeders = vec![None; n];
    let mut duration = vec![0.; n];
    let mut units = Vec::new();
    for (g, group) in shape.groups.iter().enumerate() {
        let mut unit = 0;
        for &(j, d) in &group.feeders {
            let d_int = exact_integer(d)?;
            if d_int <= 0 || !original.col_type[j].is_integral() || original.col_lower[j] < 0. {
                return None;
            }
            unit = gcd(unit, d_int as usize);
            feeders[j] = Some(g);
            duration[j] = d;
        }
        units.push(unit as f64);
    }
    let mut choice = vec![false; n];
    for r in 0..original.num_row {
        if original.row_lower[r] == 1.
            && original.row_upper[r] == 1.
            && row(original, r).all(|(j, a)| {
                a == 1.
                    && original.col_type[j].is_integral()
                    && feeders[j].is_none()
                    && original.col_lower[j] >= 0.
                    && original.col_upper[j] <= 1.
            })
        {
            for (j, _) in row(original, r) {
                choice[j] = true;
            }
        }
    }
    let kept: Vec<_> = (0..n)
        .filter(|&j| choice[j] || !original.col_type[j].is_integral())
        .collect();
    if (0..n)
        .any(|j| !choice[j] && original.col_type[j].is_integral() && original.col_cost[j] != 0.)
    {
        return None;
    }
    let mut reduced = Model::new();
    let mut map = vec![None; n];
    for &j in &kept {
        let v = reduced.add_col(original.col_lower[j], original.col_upper[j]);
        reduced.set_cost(v, original.col_cost[j]);
        reduced.set_integrality(v, original.col_type[j]);
        map[j] = Some(v.0 as usize);
    }
    let mut loads = Vec::new();
    for (g, group) in shape.groups.iter().enumerate() {
        let upper = group
            .feeders
            .iter()
            .try_fold(0u128, |sum, &(j, d)| {
                let u = exact_integer(original.col_upper[j].ceil())?;
                if u < 0 {
                    return None;
                }
                sum.checked_add(u as u128 * d as u128)
            })
            .filter(|&sum| sum <= (1u128 << 53))
            .map_or(INF, |sum| (sum / units[g] as u128) as f64);
        let v = reduced.add_col(0., upper);
        reduced.set_integrality(v, VarType::Integer);
        loads.push(v.0 as usize);
    }
    // Retain a row only when each feeder expression is a complete, exactly
    // scaled group load. Integer cross-products verify the substitution.
    for r in 0..original.num_row {
        let mut terms = BTreeMap::new();
        let mut grouped = BTreeMap::<usize, Vec<(usize, f64)>>::new();
        let mut valid = true;
        for (j, a) in row(original, r) {
            if let Some(k) = map[j] {
                *terms.entry(k).or_insert(0.) += a;
            } else if let Some(g) = feeders[j] {
                grouped.entry(g).or_default().push((j, a));
            } else {
                valid = false;
                break;
            }
        }
        for (g, entries) in grouped {
            if entries.len() != shape.groups[g].feeders.len() {
                valid = false;
                break;
            }
            let (j, a) = entries[0];
            let Some(coefficient) = exact_integer(a) else {
                valid = false;
                break;
            };
            let d = duration[j] as i64;
            let numerator = coefficient as i128 * units[g] as i128;
            if numerator % d as i128 != 0 {
                valid = false;
                break;
            }
            if entries.iter().any(|&(k, b)| {
                exact_integer(b).is_none_or(|b| {
                    b as i128 * d as i128 != coefficient as i128 * duration[k] as i128
                })
            }) {
                valid = false;
                break;
            }
            *terms.entry(loads[g]).or_insert(0.) += (numerator / d as i128) as f64;
        }
        if valid {
            add_row(
                &mut reduced,
                original.row_lower[r],
                original.row_upper[r],
                terms,
            );
        }
    }
    let mut covered = vec![false; n];
    let mut demands = Vec::new();
    for r in 0..original.num_row {
        if original.row_lower[r] != 0. || original.row_upper[r] != 0. {
            continue;
        }
        let xs: Vec<_> = row(original, r)
            .filter_map(|(j, a)| feeders[j].filter(|_| a == 1.).map(|_| j))
            .collect();
        if xs.is_empty()
            || row(original, r).any(|(j, a)| {
                if feeders[j].is_some() {
                    a != 1.
                } else {
                    !choice[j] || a >= 0. || exact_integer(a).is_none()
                }
            })
        {
            continue;
        }
        let d = duration[xs[0]];
        let s = shape.groups[feeders[xs[0]]?].session;
        if xs.iter().any(|&j| {
            duration[j] != d || shape.groups[feeders[j].unwrap()].session != s || covered[j]
        }) {
            return None;
        }
        for &j in &xs {
            covered[j] = true;
        }
        let choices = row(original, r)
            .filter(|&(j, _)| choice[j])
            .map(|(j, a)| (j, -a))
            .collect();
        demands.push(Demand {
            choices,
            feeders: xs,
            session: s,
            duration: d,
        });
    }
    if (0..n).any(|j| feeders[j].is_some() && !covered[j]) {
        return None;
    }
    // Weighted sum of the original day equalities: no placement assumption.
    for (s, gs) in shape.sessions.iter().enumerate() {
        let mut terms: BTreeMap<_, _> = gs.iter().map(|&g| (loads[g], units[g])).collect();
        for demand in demands.iter().filter(|d| d.session == s) {
            for &(j, a) in &demand.choices {
                *terms.entry(map[j]?).or_insert(0.) -= a * demand.duration;
            }
        }
        if terms
            .values()
            .any(|v| !v.is_finite() || v.abs() > (1u64 << 53) as f64)
        {
            return None;
        }
        add_row(&mut reduced, 0., 0., terms);
    }
    if reduced.num_col >= original.num_col || clock.now_secs() >= deadline {
        return None;
    }
    Some(Relaxation {
        model: reduced,
        kept,
        loads,
        units,
        demands,
        shape,
    })
}

fn lift(
    original: &Model,
    relaxation: &Relaxation,
    optimum: &[f64],
    options: &Options,
    clock: &dyn Clock,
    deadline: f64,
    interrupt: Option<&dyn Fn() -> bool>,
) -> Option<(
    Vec<f64>,
    u64,
    u64,
    crate::lp::LpStats,
    crate::solve::MipStats,
)> {
    let mut point = vec![0.; original.num_col];
    for (&j, &v) in relaxation.kept.iter().zip(optimum) {
        point[j] = if original.col_type[j].is_integral() {
            v.round()
        } else {
            v
        };
    }
    // Batch interchangeable demands by their full eligible-group/weight
    // vectors. Counts eliminate assignment symmetries; disaggregation is
    // only accepted after the full original model is checked.
    type Item = (f64, Vec<usize>);
    let mut batches = BTreeMap::<Vec<(usize, i64)>, Vec<Item>>::new();
    for demand in &relaxation.demands {
        let amount: f64 = demand.choices.iter().map(|&(j, a)| a * point[j]).sum();
        if amount == 0. {
            continue;
        }
        if amount < 0. || amount.fract() != 0. {
            return None;
        }
        let mut xs: Vec<_> = demand
            .feeders
            .iter()
            .copied()
            .filter(|&j| original.col_lower[j] <= amount && original.col_upper[j] >= amount)
            .collect();
        xs.sort_by_key(|&j| relaxation.shape.column_groups[j]);
        let key = xs
            .iter()
            .map(|&j| {
                Some((
                    relaxation.shape.column_groups[j]?,
                    exact_integer(amount * demand.duration)?,
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        if key.is_empty() {
            return None;
        }
        batches.entry(key).or_default().push((amount, xs));
    }
    let mut assignment = Model::new();
    let mut load_rows = vec![Vec::new(); relaxation.loads.len()];
    let mut placements = Vec::new();
    for (key, items) in batches {
        let mut vars = Vec::new();
        for (g, weight) in key {
            let v = assignment.add_col(0., items.len() as f64);
            assignment.set_integrality(v, VarType::Integer);
            load_rows[g].push((v.0, weight as f64 / relaxation.units[g]));
            vars.push((v.0, g));
        }
        assignment.add_row(
            items.len() as f64,
            items.len() as f64,
            &vars.iter().map(|&(j, _)| (j, 1.)).collect::<Vec<_>>(),
        );
        placements.push((items, vars));
    }
    for (g, terms) in load_rows.iter().enumerate() {
        let target = optimum[relaxation.loads[g]].round();
        assignment.add_row(target, target, terms);
    }
    let remaining = deadline - clock.now_secs();
    if remaining <= 0. {
        return None;
    }
    let opts = Options {
        time_limit_secs: remaining.min(2.),
        threads: 1,
        mip_rel_gap: 0.,
        mip_abs_gap: 0.,
        ..options.clone()
    };
    let mut callbacks = Callbacks {
        interrupt,
        ..Default::default()
    };
    let assigned = crate::solve::solve_with(&assignment, &opts, clock, &mut callbacks);
    if assigned.col_value.len() != assignment.num_col
        || !check_solution(&assignment, &assigned.col_value, 1e-9).feasible
    {
        return None;
    }
    for (items, vars) in placements {
        let mut rings = Vec::new();
        for (j, g) in vars {
            let count = assigned.col_value[j as usize].round();
            if count < 0. || count > items.len() as f64 {
                return None;
            }
            rings.extend(std::iter::repeat_n(g, count as usize));
        }
        if rings.len() != items.len() {
            return None;
        }
        for ((amount, xs), g) in items.into_iter().zip(rings) {
            let j = xs
                .into_iter()
                .find(|&j| relaxation.shape.column_groups[j] == Some(g))?;
            point[j] = amount;
        }
    }
    // Recover each original round allocation from the active original choice.
    for block in &relaxation.shape.blocks {
        for &r in &block.rows {
            if original.row_lower[r] != 0. || original.row_upper[r] != 0. {
                continue;
            }
            let xs: Vec<_> = row(original, r)
                .filter_map(|(j, a)| (a == 1. && relaxation.shape.xr_x[j].is_some()).then_some(j))
                .collect();
            if xs.is_empty()
                || row(original, r).any(|(j, a)| a > 0. && relaxation.shape.xr_x[j].is_none())
            {
                continue;
            }
            let amount = -row(original, r)
                .filter(|&(j, a)| a < 0. && relaxation.shape.xr_x[j].is_none())
                .map(|(j, a)| a * point[j])
                .sum::<f64>();
            if amount == 0. {
                continue;
            }
            let active: Vec<_> = xs
                .into_iter()
                .filter(|&j| point[relaxation.shape.xr_x[j].unwrap()] > 0.)
                .collect();
            if active.len() != 1 {
                return None;
            }
            point[active[0]] = amount;
        }
    }
    for group in &relaxation.shape.groups {
        for &(j, _) in &group.feeders {
            point[relaxation.shape.x_z[j]?] = if point[j] > 0. { 1. } else { 0. };
        }
    }
    check_solution(original, &point, 1e-9).feasible.then_some((
        point,
        assigned.nodes,
        assigned.lp_iterations,
        assigned.lp_stats,
        assigned.mip_stats,
    ))
}

pub(crate) fn prove(
    original: &Model,
    start: Option<&[f64]>,
    options: &Options,
    clock: &dyn Clock,
    deadline: f64,
    interrupt: Option<&dyn Fn() -> bool>,
) -> Option<Proof> {
    let relaxation = build(original, clock, deadline)?;
    let projected = start
        .filter(|x| check_solution(original, x, 1e-9).feasible)
        .map(|x| {
            let mut p: Vec<_> = relaxation.kept.iter().map(|&j| x[j]).collect();
            for (g, group) in relaxation.shape.groups.iter().enumerate() {
                p.push(
                    group.feeders.iter().map(|&(j, d)| x[j] * d).sum::<f64>() / relaxation.units[g],
                );
            }
            p
        });
    let warm = projected
        .as_deref()
        .filter(|x| check_solution(&relaxation.model, x, 1e-9).feasible);
    let remaining = deadline - clock.now_secs();
    if remaining <= 0. {
        return None;
    }
    let opts = Options {
        time_limit_secs: remaining * 0.6,
        threads: 1,
        mip_rel_gap: 0.,
        mip_abs_gap: 0.,
        ..options.clone()
    };
    let mut callbacks = Callbacks {
        interrupt,
        ..Default::default()
    };
    let solved =
        crate::mip::branch_and_bound(&relaxation.model, &opts, clock, &mut callbacks, warm, None);
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var("OPTIO_BLOCKDEBUG").is_ok() {
        eprintln!(
            "aggregate proof {} cols {} rows: {:?}, {:?}, obj {}, nodes {}",
            relaxation.model.num_col,
            relaxation.model.num_row,
            solved.status,
            solved.dual_bound,
            solved.objective,
            solved.nodes
        );
    }
    let bound = solved.dual_bound.filter(|v| v.is_finite())?;
    if start.is_some_and(|x| {
        check_solution(original, x, 1e-9).feasible && bound > original.objective(x)
    }) {
        return None;
    }
    let mut proof = Proof {
        bound,
        point: None,
        nodes: solved.nodes,
        iterations: solved.lp_iterations,
        lp_stats: solved.lp_stats,
        mip_stats: solved.mip_stats,
    };
    if solved.status == ModelStatus::Optimal
        && solved.col_value.len() == relaxation.model.num_col
        && check_solution(&relaxation.model, &solved.col_value, 1e-9).feasible
    {
        if let Some((point, nodes, iterations, lp_stats, mip_stats)) = lift(
            original,
            &relaxation,
            &solved.col_value,
            options,
            clock,
            deadline,
            interrupt,
        ) {
            proof.point = Some(point);
            proof.nodes += nodes;
            proof.iterations += iterations;
            proof.lp_stats += lp_stats;
            proof.mip_stats += mip_stats;
        }
    }
    Some(proof)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StdClock;

    #[test]
    fn aggregate_projection_and_bound_are_valid_for_exhaustive_mixed_plans() {
        let m = super::super::tests::mixed_model(3);
        let clock = StdClock::new();
        let relaxed = build(&m, &clock, clock.now_secs() + 5.).unwrap();
        let shape = detect(&m, &clock, clock.now_secs() + 5., 512).unwrap();
        let mut optimum = f64::INFINITY;
        let mut best = None;
        let mut count = 0;
        for a in 0..shape.blocks[0].patterns.len() {
            for b in 0..shape.blocks[1].patterns.len() {
                for c in 0..shape.blocks[2].patterns.len() {
                    let mut loads = vec![0.; shape.groups.len()];
                    for (block, i) in shape.blocks.iter().zip([a, b, c]) {
                        for &(g, l) in &block.patterns[i].loads {
                            loads[g] += l;
                        }
                    }
                    if let Some(point) = super::super::pattern_point(&m, &shape, &[a, b, c], &loads)
                    {
                        let mut projected: Vec<_> =
                            relaxed.kept.iter().map(|&j| point[j]).collect();
                        for (g, group) in relaxed.shape.groups.iter().enumerate() {
                            projected.push(
                                group
                                    .feeders
                                    .iter()
                                    .map(|&(j, d)| point[j] * d)
                                    .sum::<f64>()
                                    / relaxed.units[g],
                            );
                        }
                        assert!(check_solution(&relaxed.model, &projected, 1e-9).feasible);
                        assert!(
                            (m.objective(&point) - relaxed.model.objective(&projected)).abs()
                                < 1e-9
                        );
                        count += 1;
                        if m.objective(&point) < optimum {
                            optimum = m.objective(&point);
                            best = Some(point);
                        }
                    }
                }
            }
        }
        assert!(count > 1);
        let proof = prove(
            &m,
            best.as_deref(),
            &Options::default(),
            &clock,
            clock.now_secs() + 2.,
            None,
        )
        .unwrap();
        assert!(proof.bound <= optimum + 1e-9);
        if let Some(point) = proof.point {
            assert!(check_solution(&m, &point, 1e-9).feasible);
        }
    }

    #[test]
    fn proof_rejects_duplicate_rows_fractional_duration_and_dropped_costs() {
        let m = super::super::tests::mixed_model(2);
        let clock = StdClock::new();
        let mut duplicate = m.clone();
        duplicate.add_row(0., 1., &[(0, 0.5), (0, 0.5)]);
        assert!(build(&duplicate, &clock, clock.now_secs() + 1.).is_none());
        let mut fractional = m.clone();
        let shape = detect(&m, &clock, clock.now_secs() + 1., 0).unwrap();
        let feeder = shape.groups[0].feeders[0].0;
        for r in 0..fractional.num_row {
            for k in fractional.a_start[r] as usize..fractional.a_start[r + 1] as usize {
                if fractional.a_index[k] as usize == feeder && fractional.a_value[k] > 100. {
                    fractional.a_value[k] += 0.5;
                }
            }
        }
        assert!(build(&fractional, &clock, clock.now_secs() + 1.).is_none());
        let mut nonzero = m.clone();
        nonzero.col_cost[feeder] = 1.;
        assert!(build(&nonzero, &clock, clock.now_secs() + 1.).is_none());
    }

    #[test]
    fn an_unliftable_relaxation_cannot_create_an_original_witness() {
        let mut m = super::super::tests::mixed_model(2);
        let clock = StdClock::new();
        let shape = detect(&m, &clock, clock.now_secs() + 1., 0).unwrap();
        // Each block needs a middle-session match. Forbid every occupancy
        // indicator: the aggregate relaxation still has loads, but no original
        // schedule can pass the checker.
        for group in &shape.groups {
            for &(j, _) in &group.feeders {
                m.col_upper[shape.x_z[j].unwrap()] = 0.;
            }
        }
        let proof = prove(
            &m,
            None,
            &Options::default(),
            &clock,
            clock.now_secs() + 2.,
            None,
        )
        .unwrap();
        assert!(proof.point.is_none());
    }
}
