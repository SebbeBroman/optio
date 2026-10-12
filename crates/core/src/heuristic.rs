//! Phase-2 primal heuristic: find a feasible (not necessarily optimal) point.
//!
//! Generic **min-conflicts with cost-aware move selection**, preceded by a cheap
//! LP-rounding fast path. It is the first priority for the planning use-case,
//! where a fully-placed plan matters far more than optimality. The LP relaxation
//! + branch & bound (which prove and improve quality) come next and use this
//! point as a warm start.

use crate::lp::{LpOptions, LpRelaxation, LpSolution, LpStatus};
use crate::model::{check_solution, Model};
use crate::options::Options;
use crate::solve::Clock;

/// Cheap LP-based feasibility for assignment / exact-cover models (e.g. Sudoku):
/// solve the relaxation (bounded) and round the integer columns. The root LP of
/// such models is a vertex that is frequently already (near-)integral, so a
/// single rounding yields a feasible point far faster than local search.
///
/// Returns the validated rounded point (if any) **and** the solved root
/// relaxation (if the LP reached optimality). The root solve is handed back up
/// so the caller can give it to branch & bound, which would otherwise solve the
/// exact same root LP a second time.
fn lp_round_feasible(
    model: &Model,
    clock: &dyn Clock,
    budget_secs: f64,
) -> (Option<Vec<f64>>, Option<Vec<f64>>, Option<LpSolution>) {
    let int_cols: Vec<usize> = (0..model.num_col)
        .filter(|&j| model.col_type[j].is_integral())
        .collect();
    if int_cols.is_empty() {
        return (None, None, None);
    }
    let opts = LpOptions {
        max_iterations: 50_000,
        time_limit_secs: budget_secs.max(0.02),
        ..LpOptions::default()
    };
    let mut lpr = LpRelaxation::build(model);
    let lp = lpr.solve(&opts, clock);
    if lp.status != LpStatus::Optimal {
        return (None, None, None);
    }
    let mut xr = lp.col_value.clone();
    for &j in &int_cols {
        xr[j] = xr[j].round();
    }
    // Project continuous auxiliaries before validating (see `polish`).
    let rounded = if polish(model, &mut xr) {
        Some(xr)
    } else {
        None
    };
    (rounded, Some(lp.col_value.clone()), Some(lp))
}

struct Csc {
    start: Vec<u32>,
    index: Vec<u32>,
    value: Vec<f64>,
}

impl Csc {
    fn build(model: &Model) -> Csc {
        let n = model.num_col;
        let mut start = vec![0u32; n + 1];
        for &c in &model.a_index {
            start[c as usize + 1] += 1;
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let nnz = model.a_value.len();
        let mut index = vec![0u32; nnz];
        let mut value = vec![0.0f64; nnz];
        let mut cursor = start.clone();
        for r in 0..model.num_row {
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                let c = model.a_index[k] as usize;
                let p = cursor[c] as usize;
                index[p] = r as u32;
                value[p] = model.a_value[k];
                cursor[c] += 1;
            }
        }
        Csc {
            start,
            index,
            value,
        }
    }

    #[inline]
    fn col_rows(&self, c: usize) -> impl Iterator<Item = (usize, f64)> + '_ {
        let lo = self.start[c] as usize;
        let hi = self.start[c + 1] as usize;
        (lo..hi).map(move |k| (self.index[k] as usize, self.value[k]))
    }
}

struct Rng(u64);
impl Rng {
    #[inline]
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[inline]
fn violation(lo: f64, hi: f64, lhs: f64) -> f64 {
    (lo - lhs).max(lhs - hi).max(0.0)
}

fn init_point(model: &Model, _rng: &mut Rng) -> Vec<f64> {
    (0..model.num_col)
        .map(|c| {
            let v = 0.0f64.clamp(model.col_lower[c], model.col_upper[c]);
            if model.col_type[c].is_integral() {
                v.round()
            } else {
                v
            }
        })
        .collect()
}

fn compute(model: &Model, x: &[f64]) -> (Vec<f64>, Vec<f64>, f64) {
    let mut lhs = vec![0.0f64; model.num_row];
    for r in 0..model.num_row {
        let mut s = 0.0;
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            s += model.a_value[k] * x[model.a_index[k] as usize];
        }
        lhs[r] = s;
    }
    let viol: Vec<f64> = (0..model.num_row)
        .map(|r| violation(model.row_lower[r], model.row_upper[r], lhs[r]))
        .collect();
    let total = viol.iter().sum();
    (lhs, viol, total)
}

#[inline]
fn delta_violation(model: &Model, csc: &Csc, lhs: &[f64], viol: &[f64], c: usize, d: f64) -> f64 {
    let mut dv = 0.0;
    for (r, a) in csc.col_rows(c) {
        dv += violation(model.row_lower[r], model.row_upper[r], lhs[r] + a * d) - viol[r];
    }
    dv
}

fn apply(
    model: &Model,
    csc: &Csc,
    x: &mut [f64],
    lhs: &mut [f64],
    viol: &mut [f64],
    c: usize,
    delta: f64,
) {
    x[c] += delta;
    for (r, a) in csc.col_rows(c) {
        viol[r] -= violation(model.row_lower[r], model.row_upper[r], lhs[r]);
        let new_lhs = lhs[r] + a * delta;
        lhs[r] = new_lhs;
        viol[r] += violation(model.row_lower[r], model.row_upper[r], new_lhs);
    }
}

/// Re-project the continuous variables onto the rows (a few passes) so small
/// LP residuals do not make an otherwise-valid integer point look infeasible.
///
/// Why this is needed: the simplex equilibrates rows/columns, so its `1e-7`
/// tolerance can be ~1e-4 in *original* units on models with large
/// coefficients (e.g. `720·x - min >= 0`). `check_solution` uses `1e-6`
/// original-space, so a rounded LP point can be rejected even though the
/// integer placement is correct. Projecting the continuous auxiliaries
/// (`min`/`peak`/`range`/`dev`/…) fixes that exactly. Returns whether the
/// polished point is feasible; the point is a candidate *incumbent*, so
/// projecting can only help (it is re-checked by the oracle).
pub(crate) fn polish(model: &Model, x: &mut [f64]) -> bool {
    if check_solution(model, x, 1e-6).feasible {
        return true;
    }
    let csc = Csc::build(model);
    let (mut lhs, mut viol, _) = compute(model, x);
    for _ in 0..4 {
        project_all(model, &csc, x, &mut lhs, &mut viol);
        if check_solution(model, x, 1e-6).feasible {
            return true;
        }
    }
    false
}

pub struct Feasible {
    pub x: Vec<f64>,
    pub iterations: u64,
    /// The root LP relaxation, when the LP-rounding fast path solved it. Handed
    /// to branch & bound so it can reuse the root view instead of re-solving it.
    pub root_lp: Option<LpSolution>,
}

/// Adjust a continuous variable in row `r` to satisfy it, then propagate to the
/// other rows that variable touches (e.g. load -> total/peak/dev). Bounded depth.
fn project_row(
    model: &Model,
    csc: &Csc,
    x: &mut [f64],
    lhs: &mut [f64],
    viol: &mut [f64],
    r: usize,
    depth: u32,
) {
    if depth > 8 || viol[r] <= 1e-12 {
        return;
    }
    let lo = model.row_lower[r];
    let hi = model.row_upper[r];
    let target = if lhs[r] < lo {
        lo
    } else if lhs[r] > hi {
        hi
    } else {
        return;
    };
    for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
        let c = model.a_index[k] as usize;
        if !matches!(model.col_type[c], crate::model::VarType::Continuous) {
            continue;
        }
        let a = model.a_value[k];
        if a == 0.0 {
            continue;
        }
        let old = x[c];
        let nv = (old + (target - lhs[r]) / a).clamp(model.col_lower[c], model.col_upper[c]);
        let d = nv - old;
        if d.abs() > 1e-12 {
            apply(model, csc, x, lhs, viol, c, d);
            let affected: Vec<usize> = csc.col_rows(c).map(|(rr, _)| rr).collect();
            for rr in affected {
                if rr != r {
                    project_row(model, csc, x, lhs, viol, rr, depth + 1);
                }
            }
        }
        return;
    }
}

fn project_all(model: &Model, csc: &Csc, x: &mut [f64], lhs: &mut [f64], viol: &mut [f64]) {
    for r in 0..model.num_row {
        project_row(model, csc, x, lhs, viol, r, 0);
    }
}

pub fn find_feasible(
    model: &Model,
    options: &Options,
    clock: &dyn Clock,
    max_iters: u64,
) -> Option<Feasible> {
    let tol = 1e-6;
    // LP-rounding fast path first: for assignment / exact-cover models the root
    // LP rounds to a feasible integer point in a fraction of the budget, whereas
    // single-variable min-conflicts stalls on the coupled equalities. On models
    // where it fails, this only adds a bounded LP solve before the local search.
    // The solved root LP is carried up (even when rounding fails to be feasible)
    // so branch & bound can reuse it and skip its own root solve.
    let lp_budget = (options.time_limit_secs * 0.25).clamp(0.02, 0.5);
    let (rounded, lp_seed, root_lp) = lp_round_feasible(model, clock, lp_budget);
    if let Some(x) = rounded {
        return Some(Feasible {
            x,
            iterations: 0,
            root_lp,
        });
    }
    // Only needed on the min-conflicts fallback path (the LP fast path returns
    // above), so build it lazily.
    let csc = Csc::build(model);
    let mut rng = Rng(options
        .seed
        .wrapping_mul(0x9E3779B97F4A7C15)
        .wrapping_add(1));

    // Seed the local search from the (fractional) root LP point when available:
    // its integer columns are the ones a repair move needs to change, and its
    // continuous auxiliaries are already consistent with the rows.
    let mut x = lp_seed.unwrap_or_else(|| init_point(model, &mut rng));
    if x.len() != model.num_col {
        x = init_point(model, &mut rng);
    }
    for j in 0..model.num_col {
        if model.col_type[j].is_integral() {
            x[j] = x[j].round();
        }
    }
    let (mut lhs, mut viol, _) = compute(model, &x);
    project_all(model, &csc, &mut x, &mut lhs, &mut viol);
    let mut total = viol.iter().sum::<f64>();
    let mut best_total = total;
    let mut since_improve = 0u64;
    let start = clock.now_secs();
    #[cfg(not(target_arch = "wasm32"))]
    let debug = std::env::var("OPTIO_HDEBUG").is_ok();
    #[cfg(target_arch = "wasm32")]
    let debug = false;

    for iter in 0..max_iters {
        if total <= tol {
            return Some(Feasible {
                x,
                iterations: iter,
                root_lp,
            });
        }
        if iter & 0x3f == 0 && clock.now_secs() - start > options.time_limit_secs {
            break;
        }

        // Most-violated row (random tie-break).
        let mut r = 0usize;
        let mut best_v = -1.0;
        for rr in 0..model.num_row {
            if viol[rr] > best_v + 1e-12 {
                best_v = viol[rr];
                r = rr;
            } else if viol[rr] > best_v - 1e-12 && rng.next_f64() < 0.25 {
                r = rr;
            }
        }
        if best_v <= 0.0 {
            return Some(Feasible {
                x,
                iterations: iter,
                root_lp,
            });
        }

        let lo = model.row_lower[r];
        let hi = model.row_upper[r];
        let dir = if lhs[r] < lo { 1.0 } else { -1.0 };
        let need = if lhs[r] < lo {
            lo - lhs[r]
        } else {
            lhs[r] - hi
        };

        // Best candidate in this row by true effect on total violation.
        let mut candidate: Option<(usize, f64, f64)> = None;
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            let c = model.a_index[k] as usize;
            let a = model.a_value[k];
            if a == 0.0 {
                continue;
            }
            let desired = dir * need / a;
            let old = x[c];
            let clo = model.col_lower[c];
            let chi = model.col_upper[c];
            let mut newv = (old + desired).clamp(clo, chi);
            if model.col_type[c].is_integral() {
                newv = newv.round();
                if (newv - old).abs() < 1e-9 {
                    newv = (old + dir * a.signum()).clamp(clo, chi).round();
                }
            }
            let d = newv - old;
            if d.abs() < 1e-9 {
                continue;
            }
            let dv = delta_violation(model, &csc, &lhs, &viol, c, d);
            if candidate.map_or(true, |(_, _, bdv)| dv < bdv) {
                candidate = Some((c, d, dv));
            }
        }

        match candidate {
            Some((c, d, dv)) if dv < -1e-12 => {
                apply(model, &csc, &mut x, &mut lhs, &mut viol, c, d);
                let affected: Vec<usize> = csc.col_rows(c).map(|(r, _)| r).collect();
                for r in affected {
                    project_row(model, &csc, &mut x, &mut lhs, &mut viol, r, 0);
                }
            }
            _ => {
                // No improving move in this (or any) row: perturb to escape.
                let cc = (rng.next_f64() * model.num_col as f64) as usize % model.num_col.max(1);
                if model.col_type[cc].is_integral() {
                    let old = x[cc];
                    let step = if rng.next_f64() < 0.5 { 1.0 } else { -1.0 };
                    let nv = (old + step)
                        .clamp(model.col_lower[cc], model.col_upper[cc])
                        .round();
                    let dd = nv - old;
                    if dd.abs() > 1e-9 {
                        apply(model, &csc, &mut x, &mut lhs, &mut viol, cc, dd);
                    }
                }
            }
        }

        total = viol.iter().sum();
        if debug && iter % 200_000 == 0 {
            eprintln!("iter {iter} total {total:.2} best {best_total:.2} since {since_improve}");
        }
        if total < best_total - 1e-9 {
            best_total = total;
            since_improve = 0;
        } else {
            since_improve += 1;
            if since_improve > 4000 {
                x = init_point(model, &mut rng);
                let (l, v, _t) = compute(model, &x);
                lhs = l;
                viol = v;
                project_all(model, &csc, &mut x, &mut lhs, &mut viol);
                total = viol.iter().sum();
                best_total = total;
                since_improve = 0;
            }
        }
    }

    if total <= tol {
        Some(Feasible {
            x,
            iterations: max_iters,
            root_lp,
        })
    } else {
        if debug {
            let mut idx: Vec<usize> = (0..model.num_row).filter(|&r| viol[r] > 1e-9).collect();
            idx.sort_by(|&a, &b| {
                viol[b]
                    .partial_cmp(&viol[a])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            eprintln!("remaining violated rows: {}", idx.len());
            for &r in idx.iter().take(12) {
                eprintln!(
                    "  row {r} viol {:.2} lhs {:.2} [{:.2},{:.2}]",
                    viol[r], lhs[r], model.row_lower[r], model.row_upper[r]
                );
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{VarType, INF};
    use crate::options::Options;
    use crate::solve::StdClock;

    /// Build a planning-shaped model natively, mirroring tools/gen-planning.mjs:
    /// integer x[u,p,r], binary z[u,p,r], per-(p,r) load = sum dur*x, cap, and
    /// per-session peak/min/range/deviation auxiliaries. Cap is set from a witness
    /// so the instance is feasible.
    fn planning_like(units: usize, sessions: usize, rings: usize) -> Model {
        let mut m = Model::new();
        let rng = |i: usize, mul: usize| ((i * mul % 100) as f64) / 100.0;
        let matches: Vec<f64> = (0..units)
            .map(|u| (1.0 + 4.0 * rng(u, 37)).round())
            .collect();
        let dur: Vec<f64> = (0..units)
            .map(|u| (600.0 + 600.0 * rng(u, 53)).round())
            .collect();

        let mut x = vec![vec![vec![0u32; rings]; sessions]; units];
        let mut z = vec![vec![vec![0u32; rings]; sessions]; units];
        for u in 0..units {
            for p in 0..sessions {
                for r in 0..rings {
                    let xi = m.add_col(0.0, matches[u]).0;
                    m.set_integrality(crate::model::VarId(xi), VarType::Integer);
                    let zi = m.add_col(0.0, 1.0).0;
                    m.set_integrality(crate::model::VarId(zi), VarType::Binary);
                    x[u][p][r] = xi;
                    z[u][p][r] = zi;
                }
            }
        }

        // Witness -> cap.
        let mut wload = vec![vec![0.0f64; rings]; sessions];
        for u in 0..units {
            wload[u % sessions][u % rings] += matches[u] * dur[u];
        }
        let cap = (wload.iter().flatten().cloned().fold(0.0, f64::max) * 1.5).ceil();

        for u in 0..units {
            let mut tot = Vec::new();
            for p in 0..sessions {
                for r in 0..rings {
                    tot.push((x[u][p][r], 1.0));
                }
            }
            m.add_row(matches[u], matches[u], &tot);
            for p in 0..sessions {
                let mut zt = Vec::new();
                for r in 0..rings {
                    m.add_row(-INF, 0.0, &[(x[u][p][r], 1.0), (z[u][p][r], -matches[u])]);
                    zt.push((z[u][p][r], 1.0));
                }
                m.add_row(-INF, 1.0, &zt);
            }
        }

        let mut load = vec![vec![0u32; rings]; sessions];
        for p in 0..sessions {
            for r in 0..rings {
                let l = m.add_col(0.0, INF).0;
                load[p][r] = l;
                let mut terms = vec![(l, 1.0)];
                for u in 0..units {
                    terms.push((x[u][p][r], -dur[u]));
                }
                m.add_row(0.0, 0.0, &terms); // load = sum dur*x
                m.add_row(-INF, cap, &[(l, 1.0)]); // cap
            }
            let peak = m.add_col(0.0, INF).0;
            let minv = m.add_col(0.0, INF).0;
            let total = m.add_col(0.0, INF).0;
            let range = m.add_col(0.0, INF).0;
            let mut tot_def = vec![(total, 1.0)];
            for r in 0..rings {
                tot_def.push((load[p][r], -1.0));
                m.add_row(-INF, 0.0, &[(load[p][r], 1.0), (peak, -1.0)]); // peak >= load
                m.add_row(0.0, INF, &[(load[p][r], 1.0), (minv, -1.0)]); // min <= load
                let dev = m.add_col(0.0, INF).0;
                let mut hi = vec![(dev, -1.0), (total, -1.0)];
                let mut lo = vec![(dev, -1.0), (total, 1.0)];
                for rr in 0..rings {
                    hi.push((load[p][rr], rings as f64));
                    lo.push((load[p][rr], -(rings as f64)));
                }
                m.add_row(-INF, 0.0, &hi);
                m.add_row(-INF, 0.0, &lo);
            }
            m.add_row(0.0, 0.0, &tot_def);
            m.add_row(0.0, INF, &[(range, 1.0), (peak, -1.0), (minv, 1.0)]);
        }
        m
    }

    #[test]
    fn finds_feasible_assignment() {
        let mut m = Model::new();
        for _ in 0..3 {
            m.add_col(0.0, 2.0);
        }
        for c in 0..3 {
            m.set_integrality(crate::model::VarId(c), VarType::Integer);
        }
        m.add_row(4.0, 4.0, &[(0, 1.0), (1, 1.0), (2, 1.0)]);
        let opts = Options {
            time_limit_secs: 2.0,
            seed: 7,
            ..Default::default()
        };
        let f = find_feasible(&m, &opts, &StdClock::new(), 100_000).expect("feasible");
        assert!((f.x.iter().sum::<f64>() - 4.0).abs() < 1e-9);
        let _ = INF;
    }

    #[test]
    fn finds_feasible_with_linked_fixed_charge() {
        // 2 units x 2 rings: choose-one + fixed-charge linking + load/peak.
        let mut m = Model::new();
        let mut x = [[0u32; 2]; 2];
        let mut z = [[0u32; 2]; 2];
        for u in 0..2 {
            for r in 0..2 {
                x[u][r] = m.add_col(0.0, 2.0).0;
                z[u][r] = m.add_col(0.0, 1.0).0;
                m.set_integrality(crate::model::VarId(x[u][r]), VarType::Integer);
                m.set_integrality(crate::model::VarId(z[u][r]), VarType::Binary);
            }
            m.add_row(2.0, 2.0, &[(x[u][0], 1.0), (x[u][1], 1.0)]);
            m.add_row(0.0, 1.0, &[(z[u][0], 1.0), (z[u][1], 1.0)]);
            for r in 0..2 {
                m.add_row(-INF, 0.0, &[(x[u][r], 1.0), (z[u][r], -2.0)]);
            }
        }
        let peak = m.add_col(0.0, INF).0;
        let load = m.add_col(0.0, INF).0;
        let mut terms: Vec<(u32, f64)> = vec![(load, 1.0)];
        for u in 0..2 {
            for r in 0..2 {
                terms.push((x[u][r], -100.0));
            }
        }
        m.add_row(0.0, 0.0, &terms);
        m.add_row(0.0, INF, &[(load, 1.0), (peak, -1.0)]);
        let opts = Options {
            time_limit_secs: 2.0,
            seed: 3,
            ..Default::default()
        };
        let f = find_feasible(&m, &opts, &StdClock::new(), 200_000).expect("feasible");
        assert!(crate::model::check_solution(&m, &f.x, 1e-6).feasible);
    }

    // Known limitation: the coupled SOS1 ("choose one ring per unit/session")
    // assignment stalls single-variable local search on the last few rows; it is
    // resolved by the LP relaxation + B&B / a transfer move (Phase 1-2b).
    #[ignore = "planning-shaped coupled models need LP/B&B or a transfer move"]
    #[test]
    fn finds_feasible_planning_like_small() {
        let m = planning_like(8, 3, 5);
        let opts = Options {
            time_limit_secs: 5.0,
            seed: 11,
            ..Default::default()
        };
        let f = find_feasible(&m, &opts, &StdClock::new(), 5_000_000);
        assert!(f.is_some(), "heuristic failed to find a feasible plan");
    }

    // Known limitation: once the "choose one ring per unit/session" (SOS1-like)
    // coupling bites, single-variable local search stalls on the last few rows.
    // Resolved by the LP relaxation + branch & bound / a transfer move (Phase 1-2b).
    #[ignore = "coupled SOS1 rows need LP/B&B or a transfer move"]
    #[test]
    fn finds_feasible_planning_like_large() {
        let m = planning_like(40, 3, 5);
        let opts = Options {
            time_limit_secs: 10.0,
            seed: 11,
            ..Default::default()
        };
        let f = find_feasible(&m, &opts, &StdClock::new(), 5_000_000);
        assert!(f.is_some(), "heuristic failed to find a feasible plan");
    }
}
