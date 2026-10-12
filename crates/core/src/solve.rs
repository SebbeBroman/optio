//! Solve entry points and the platform clock abstraction.
//!
//! The full pipeline (presolve -> LP relaxation -> branch & bound with primal
//! heuristics) lands in Phases 1-2. This module defines the shapes the rest of
//! the workspace depends on now: `Clock`, `Solution`, and the anytime callbacks.

use crate::lp::LpSolution;
use crate::model::Model;
use crate::options::Options;
use crate::status::ModelStatus;

/// A monotonic time source. `std` has no clock on `wasm32-unknown-unknown`, so
/// the wasm crate supplies its own (`performance.now()` based).
pub trait Clock: Send + Sync {
    fn now_secs(&self) -> f64;
}

/// Native wall clock.
pub struct StdClock {
    start: std::time::Instant,
}

impl StdClock {
    pub fn new() -> Self {
        StdClock {
            start: std::time::Instant::now(),
        }
    }
}

impl Default for StdClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for StdClock {
    #[inline]
    fn now_secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
}

/// A candidate solution found during search.
#[derive(Clone, Debug)]
pub struct Incumbent {
    pub objective: f64,
    pub col_value: Vec<f64>,
}

/// Progress snapshot pushed to the callback (mirrors `MipProgress`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Progress {
    pub running_time: f64,
    pub nodes: u64,
    pub lp_iterations: u64,
    pub gap: f64,
    pub primal_bound: f64,
    pub dual_bound: f64,
    pub has_incumbent: bool,
}

/// Anytime hooks. Both are optional and called from the solve loop.
#[derive(Default)]
pub struct Callbacks<'a> {
    pub on_progress: Option<&'a mut dyn FnMut(&Progress)>,
    pub on_incumbent: Option<&'a mut dyn FnMut(&Incumbent)>,
    /// Return `true` to request an early stop (time/interrupt).
    pub interrupt: Option<&'a dyn Fn() -> bool>,
}

/// Final solve result.
#[derive(Clone, Copy, Debug, Default)]
pub struct MipStats {
    pub compressed_columns: u64,
    pub objective_count_columns: u64,
    pub propagation_prunes: u64,
    pub propagation_seconds: f64,
    pub strong_branch_probes: u64,
    pub numerical_retries: u64,
}
impl std::ops::AddAssign for MipStats {
    fn add_assign(&mut self, rhs: Self) {
        self.compressed_columns += rhs.compressed_columns;
        self.objective_count_columns += rhs.objective_count_columns;
        self.propagation_prunes += rhs.propagation_prunes;
        self.propagation_seconds += rhs.propagation_seconds;
        self.strong_branch_probes += rhs.strong_branch_probes;
        self.numerical_retries += rhs.numerical_retries;
    }
}

#[derive(Clone, Debug, Default)]
pub struct Solution {
    pub status: ModelStatus,
    pub objective: f64,
    /// Conservative global lower bound; None when no bound was established.
    pub dual_bound: Option<f64>,
    pub col_value: Vec<f64>,
    pub nodes: u64,
    pub lp_iterations: u64,
    /// Wall-clock phase timings (seconds): the primal heuristics, the planning
    /// objective local search, and branch & bound. `0.0` when not run.
    pub heuristic_secs: f64,
    pub local_search_secs: f64,
    pub bb_secs: f64,
    pub presolve_secs: f64,
    pub lp_stats: crate::lp::LpStats,
    pub mip_stats: MipStats,
    pub cut_secs: f64,
    pub bound_secs: f64,
    pub root_cuts: usize,
    pub inferred_integer_cols: usize,
}

impl Solution {
    pub fn has_solution(&self) -> bool {
        self.status.has_feasible_solution() && !self.col_value.is_empty()
    }
}

/// Run the planning objective local search, optionally as a native parallel
/// multi-start portfolio. Each replica searches from the same feasible start
/// with a different RNG seed and the best validated result wins, so the outcome
/// is deterministic for a fixed replica count. `threads`: 1 = serial, 0 = all
/// cores, N >= 2 = N replicas. Compiled out (serial) when `parallel` is off,
/// which is always the case on wasm32.
#[allow(unused_variables)]
fn plan_objective_search(
    model: &Model,
    x0: &[f64],
    clock: &dyn Clock,
    budget: f64,
    seed: u64,
    threads: usize,
    adaptive: bool,
    report: &mut dyn FnMut(&[f64]),
) -> Option<(f64, Vec<f64>)> {
    if let Some(x) =
        crate::block_planner::improve_with_report(model, Some(x0), clock, budget, Some(report))
    {
        return Some((model.objective(&x), x));
    }
    #[cfg(feature = "parallel")]
    {
        if threads != 1 {
            if let Ok(pool) = build_pool(threads) {
                use rayon::prelude::*;
                let reps = pool.current_num_threads();
                let results: Vec<Option<(f64, Vec<f64>)>> = pool.install(|| {
                    (0..reps)
                        .into_par_iter()
                        .map(|k| {
                            crate::local_search::improve_planning_adaptive(
                                model,
                                x0,
                                clock,
                                budget,
                                seed.wrapping_add(k as u64),
                                adaptive,
                            )
                        })
                        .collect()
                });
                let mut best: Option<(f64, Vec<f64>)> = None;
                for r in results.into_iter().flatten() {
                    if best.as_ref().map_or(true, |(o, _)| r.0 < *o - 1e-12) {
                        best = Some(r);
                    }
                }
                return best;
            }
        }
    }
    crate::local_search::improve_planning_adaptive(model, x0, clock, budget, seed, adaptive)
}

#[cfg(feature = "parallel")]
fn build_pool(threads: usize) -> Result<rayon::ThreadPool, rayon::ThreadPoolBuildError> {
    let b = rayon::ThreadPoolBuilder::new();
    if threads >= 2 {
        b.num_threads(threads).build()
    } else {
        b.build()
    }
}

/// Bounds-clamped zero point; the starting guess for primal heuristics.
pub fn initial_point(model: &Model) -> Vec<f64> {
    (0..model.num_col)
        .map(|c| 0.0f64.clamp(model.col_lower[c], model.col_upper[c]))
        .collect()
}


/// Add violated valid inequalities at the root relaxation, repeating until no
/// generated cut is violated or the round budget is exhausted. `model` must
/// carry the true model integrality (cuts are only valid then); it is mutated in
/// place. Every cut is additionally checked against a known feasible incumbent
/// when one is available, as a guard.
fn add_root_cuts(
    model: &mut Model,
    warm: Option<&[f64]>,
    clock: &dyn Clock,
    deadline: f64,
) -> (usize, Option<LpSolution>) {
    use crate::lp::{LpOptions, LpRelaxation, LpStatus};
    let mut added = 0usize;
    let mut reusable = None;
    for _round in 0..2 {
        if clock.now_secs() >= deadline {
            break;
        }
        let mut lpr = LpRelaxation::build(model);
        let opts = LpOptions {
            time_limit_secs: (deadline - clock.now_secs()).max(0.01),
            generate_cuts: true,
            ..LpOptions::default()
        };
        let sol = lpr.solve(&opts, clock);
        if sol.status != LpStatus::Optimal {
            break;
        }
        let mut round_added = 0usize;
        for cut in &sol.cuts {
            let mut lhs = 0.0;
            for (k, &j) in cut.indices.iter().enumerate() {
                lhs += cut.values[k] * sol.col_value.get(j as usize).copied().unwrap_or(0.0);
            }
            if cut.lower - lhs <= 1e-7 * (1.0 + cut.lower.abs()) {
                continue;
            }
            if let Some(w) = warm {
                let mut wlhs = 0.0;
                for (k, &j) in cut.indices.iter().enumerate() {
                    wlhs += cut.values[k] * w.get(j as usize).copied().unwrap_or(0.0);
                }
                if cut.lower - wlhs > 1e-6 * (1.0 + cut.lower.abs()) {
                    continue;
                }
            }
            let entries: Vec<(u32, f64)> = cut
                .indices
                .iter()
                .copied()
                .zip(cut.values.iter().copied())
                .collect();
            model.add_row(cut.lower, crate::model::INF, &entries);
            added += 1;
            round_added += 1;
        }
        if round_added == 0 {
            reusable = Some(sol);
            break;
        }
    }
    (added, reusable)
}

/// Solve `model` without callbacks. See [`solve_with`].
pub fn solve(model: &Model, options: &Options, clock: &dyn Clock) -> Solution {
    let mut callbacks = Callbacks::default();
    solve_with(model, options, clock, &mut callbacks)
}

/// Solve `model` with callbacks but no warm start.
pub fn solve_with(
    model: &Model,
    options: &Options,
    clock: &dyn Clock,
    callbacks: &mut Callbacks<'_>,
) -> Solution {
    solve_with_start(model, options, clock, callbacks, None)
}

/// Solve `model` with callbacks and an optional warm-start primal point.
///
/// Pipeline: a structure-aware heuristic finds an incumbent fast, then validated
/// lower bounds and branch & bound improve it and pursue an optimality proof. `check_solution`
/// is the source of truth for correctness, never the reported status.
pub fn solve_with_start(
    model: &Model,
    options: &Options,
    clock: &dyn Clock,
    callbacks: &mut Callbacks<'_>,
    warm_start: Option<&[f64]>,
) -> Solution {
    if model.num_col == 0 {
        return Solution {
            status: ModelStatus::Empty,
            ..Default::default()
        };
    }

    let solve_start = clock.now_secs();
    let presolved;
    let original = model;
    let model = if options.presolve {
        presolved =
            match crate::presolve::propagate(model, clock, solve_start + options.time_limit_secs) {
                Ok(m) => m,
                Err(()) => {
                    return Solution {
                        status: ModelStatus::Infeasible,
                        presolve_secs: clock.now_secs() - solve_start,
                        ..Default::default()
                    }
                }
            };
        &presolved
    } else {
        model
    };
    let presolve_secs = clock.now_secs() - solve_start;
    if clock.now_secs() >= solve_start + options.time_limit_secs {
        let point = warm_start.filter(|x| {
            x.len() == original.num_col && crate::model::check_solution(original, x, 1e-6).feasible
        });
        if let (Some(point), Some(f)) = (point, callbacks.on_incumbent.as_mut()) {
            f(&Incumbent {
                objective: original.objective(point),
                col_value: point.to_vec(),
            });
        }
        return Solution {
            status: ModelStatus::TimeLimit,
            objective: point.map_or(0., |x| original.objective(x)),
            col_value: point.map_or_else(Vec::new, |x| x.to_vec()),
            presolve_secs,
            ..Default::default()
        };
    }
    if options.presolve && (options.mip_objective_counts || options.mip_column_compression) {
        let transformation = if options.mip_objective_counts {
            crate::objective_counts::ObjectiveCounts::build(model).map(Transformation::Counts)
        } else {
            None
        }
        .or_else(|| {
            options
                .mip_column_compression
                .then(|| crate::column_compression::Compression::build(model))
                .flatten()
                .map(Transformation::Compression)
        })
        .filter(|t| {
            warm_start.is_none_or(|x| {
                !crate::model::check_solution(original, x, 1e-6).feasible
                    || t.project_start(x).is_some()
            })
        });
        if let Some(transformation) = transformation {
            let preparation = clock.now_secs() - solve_start;
            let inner_options = Options {
                mip_column_compression: false,
                mip_objective_counts: false,
                time_limit_secs: (options.time_limit_secs - preparation).max(0.),
                ..options.clone()
            };
            let start = warm_start.and_then(|x| transformation.project_start(x));
            let checked_primal = std::cell::Cell::new(f64::INFINITY);
            let mut transformed = {
                let Callbacks {
                    on_incumbent,
                    on_progress,
                    interrupt,
                } = callbacks;
                let mut emit = |inc: &Incumbent| {
                    if let Some(point) = transformation.lift(&inc.col_value) {
                        checked_primal.set(checked_primal.get().min(model.objective(&point)));
                        if let Some(f) = on_incumbent.as_mut() {
                            f(&Incumbent {
                                objective: model.objective(&point),
                                col_value: point,
                            });
                        }
                    }
                };
                let mut progress = |p: &Progress| {
                    if let Some(f) = on_progress.as_mut() {
                        let mut original_progress = *p;
                        original_progress.running_time += preparation;
                        original_progress.has_incumbent = checked_primal.get().is_finite();
                        original_progress.primal_bound = checked_primal.get();
                        original_progress.gap =
                            if original_progress.has_incumbent && p.dual_bound.is_finite() {
                                ((checked_primal.get() - p.dual_bound)
                                    / checked_primal.get().abs().max(1.))
                                .max(0.)
                            } else {
                                f64::INFINITY
                            };
                        f(&original_progress);
                    }
                };
                let mut mapped = Callbacks {
                    on_incumbent: Some(&mut emit),
                    on_progress: Some(&mut progress),
                    interrupt: *interrupt,
                };
                solve_with_start(
                    transformation.model(),
                    &inner_options,
                    clock,
                    &mut mapped,
                    start.as_deref(),
                )
            };
            transformed.presolve_secs += preparation;
            transformed.mip_stats.compressed_columns +=
                transformation.removed_columns(model.num_col);
            transformed.mip_stats.objective_count_columns +=
                transformation.added_columns(model.num_col);
            if transformed.col_value.is_empty() && transformed.status != ModelStatus::Unbounded {
                return transformed;
            }
            let mut lifted_fallback = None;
            if let Some(point) = transformation.lift(&transformed.col_value) {
                transformed.objective = model.objective(&point);
                transformed.col_value = point;
                // Outward continuous bounds form a relaxation. A checked lift
                // closes it only under the same numerical proof tolerance.
                if transformed.status != ModelStatus::Optimal
                    || transformed.dual_bound.is_some_and(|b| {
                        b <= transformed.objective
                            && transformed.objective - b
                                <= options
                                    .mip_abs_gap
                                    .max(options.mip_rel_gap * transformed.objective.abs())
                                    .max(1e-7 * (1. + transformed.objective.abs()))
                    })
                {
                    return transformed;
                }
                lifted_fallback = Some(transformed.col_value.clone());
            }
            // A tolerance-feasible reduced point need not lift. Retry the
            // original model with the remaining budget and original callbacks.
            let fallback_options = Options {
                mip_column_compression: false,
                mip_objective_counts: false,
                time_limit_secs: (options.time_limit_secs - (clock.now_secs() - solve_start))
                    .max(0.),
                ..options.clone()
            };
            let mut fallback = solve_with_start(
                model,
                &fallback_options,
                clock,
                callbacks,
                match (lifted_fallback.as_deref(), warm_start) {
                    (Some(lifted), Some(start))
                        if crate::model::check_solution(model, start, 1e-9).feasible
                            && model.objective(start) < model.objective(lifted) =>
                    {
                        Some(start)
                    }
                    (Some(lifted), _) => Some(lifted),
                    (None, start) => start,
                },
            );
            fallback.presolve_secs += transformed.presolve_secs;
            fallback.lp_stats += transformed.lp_stats;
            fallback.mip_stats += transformed.mip_stats;
            fallback.nodes += transformed.nodes;
            fallback.lp_iterations += transformed.lp_iterations;
            fallback.heuristic_secs += transformed.heuristic_secs;
            fallback.local_search_secs += transformed.local_search_secs;
            fallback.bb_secs += transformed.bb_secs;
            fallback.cut_secs += transformed.cut_secs;
            fallback.bound_secs += transformed.bound_secs;
            return fallback;
        }
    }
    if model.col_lower == model.col_upper {
        let chk = crate::model::check_solution(original, &model.col_lower, 1e-6);
        if chk.feasible {
            if let Some(f) = callbacks.on_incumbent.as_mut() {
                f(&Incumbent {
                    objective: chk.objective,
                    col_value: model.col_lower.clone(),
                });
            }
            return Solution {
                status: ModelStatus::Optimal,
                objective: chk.objective,
                dual_bound: Some(chk.objective),
                presolve_secs,
                col_value: model.col_lower.clone(),
                ..Default::default()
            };
        }
        return Solution {
            status: ModelStatus::Infeasible,
            ..Default::default()
        };
    }

    // Primal-first: an early incumbent from the LP-free heuristic, but only for
    // models that actually have integer variables — a pure LP needs no heuristic.
    let has_int = model.col_type.iter().any(|t| t.is_integral());
    let t0 = solve_start;
    // Validate the caller's incumbent before doing any search. A complete
    // placement can be much cheaper to polish than finding it again via an LP.
    let supplied_warm = warm_start.and_then(|w| {
        if w.len() != model.num_col {
            return None;
        }
        let mut x = w.to_vec();
        crate::heuristic::polish(model, &mut x).then_some(x)
    });
    // The root LP solved by the feasibility heuristic (when it takes the
    // LP-rounding fast path) is reused by branch & bound below, so the root
    // relaxation is solved once instead of twice.
    let mut root_lp: Option<LpSolution> = None;
    let (heuristic_warm, _initial_elapsed) = if has_int && supplied_warm.is_none() {
        let heur_limit = (options.time_limit_secs * 0.3).clamp(0.05, 2.0);
        let heur_started = clock.now_secs();
        let mut heur_opts = Options {
            time_limit_secs: heur_limit,
            ..options.clone()
        };
        // Same order (and laziness) as the previous `or_else` chain, but capture
        // the solved root LP from `find_feasible` when it provides one.
        let h = if let Some(x) = crate::block_planner::improve(model, None, clock, heur_limit) {
            Some(x)
        } else if let Some(x) = crate::local_search::construct_inlined_planning(model) {
            Some(x)
        } else if let Some(f) = {
            heur_opts.time_limit_secs = (heur_limit - (clock.now_secs() - heur_started)).max(0.);
            crate::heuristic::find_feasible(model, &heur_opts, clock, 5_000_000)
        } {
            root_lp = f.root_lp;
            Some(f.x)
        } else {
            crate::local_search::repair(
                model,
                &crate::solve::initial_point(model),
                200_000,
                options.seed,
            )
            .map(|r| r.x)
        };
        let e = clock.now_secs() - t0;
        (h, e)
    } else {
        (None, clock.now_secs() - t0)
    };

    // A rejected supplied start must not discard a valid heuristic incumbent.
    let warm = supplied_warm.or(heuristic_warm);

    let mut warm = warm.or_else(|| {
        if has_int {
            let remaining = (options.time_limit_secs - (clock.now_secs() - t0)).max(0.2);
            // The pump/dive are fallbacks for when the structure-aware heuristic
            // above fails; keep their share small so a hard feasibility case does
            // not spend most of the budget here instead of in the proof search.
            let pump_opts = Options {
                time_limit_secs: (remaining * 0.1).min(1.0),
                ..options.clone()
            };
            crate::mip::feasibility_pump(model, &pump_opts, clock)
                .map(|(_, x)| x)
                .or_else(|| crate::mip::dive_incumbent(model, &pump_opts, clock).map(|(_, x)| x))
        } else {
            None
        }
    });

    let elapsed = clock.now_secs() - t0;

    // Report the starting incumbent (the caller's warm start, or the
    // heuristic's first feasible plan) before optimising. The app uses the
    // first incumbent as the "current plan" baseline for replanning, so it must
    // be emitted before the local search improves it.
    if warm.is_some() {
        if let Some(w) = warm.as_ref() {
            let chk = crate::model::check_solution(model, w, 1e-6);
            if chk.feasible {
                if let Some(f) = callbacks.on_incumbent.as_mut() {
                    f(&Incumbent {
                        objective: chk.objective,
                        col_value: w.clone(),
                    });
                }
                // Give callback-based stopping rules a chance to accept this
                // incumbent before local search or LP preprocessing begins.
                if let Some(f) = callbacks.on_progress.as_mut() {
                    f(&Progress {
                        running_time: clock.now_secs() - t0,
                        primal_bound: chk.objective,
                        dual_bound: f64::NEG_INFINITY,
                        gap: f64::INFINITY,
                        has_incumbent: true,
                        ..Default::default()
                    });
                }
            }
        }
    }

    if let Some(w) = warm.as_ref() {
        if callbacks.interrupt.is_some_and(|f| f()) {
            return Solution {
                status: ModelStatus::Interrupt,
                objective: model.objective(w),
                col_value: w.clone(),
                heuristic_secs: elapsed,
                ..Default::default()
            };
        }
    }

    let bound_started = clock.now_secs();
    let mut has_scheduling_certificate = false;
    let mut row_bound = if has_int {
        let row = crate::block_planner::row_bound(original);
        let scheduling =
            crate::block_planner::scheduling_bound(original, clock, t0 + options.time_limit_secs);
        has_scheduling_certificate = scheduling.is_some();
        match (row, scheduling) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    } else {
        None
    };
    let mut bound_secs = clock.now_secs() - bound_started;
    if let (Some(x), Some(bound)) = (warm.as_ref(), row_bound) {
        let check = crate::model::check_solution(original, x, 1e-9);
        if check.feasible
            && bound <= check.objective
            && check.objective - bound
                <= options
                    .mip_abs_gap
                    .max(options.mip_rel_gap * check.objective.abs())
                    .max(1e-7 * (1. + check.objective.abs()))
        {
            if let Some(f) = callbacks.on_progress.as_mut() {
                f(&Progress {
                    running_time: clock.now_secs() - t0,
                    primal_bound: check.objective,
                    dual_bound: bound,
                    gap: ((check.objective - bound) / check.objective.abs().max(1.)).max(0.),
                    has_incumbent: true,
                    ..Default::default()
                });
            }
            return Solution {
                status: ModelStatus::Optimal,
                objective: check.objective,
                dual_bound: Some(bound),
                bound_secs,
                col_value: x.clone(),
                presolve_secs,
                heuristic_secs: (elapsed - presolve_secs).max(0.),
                ..Default::default()
            };
        }
    }

    let mut certificate_lp_stats = crate::lp::LpStats::default();
    let mut certificate_mip_stats = MipStats::default();
    let mut certificate_nodes = 0;
    let mut certificate_iterations = 0;

    // Prove a smaller aggregate-load relaxation, then lift its solution back
    // to an independently checked original schedule. Sampled planning patterns
    // are not part of this lower-bound calculation.
    // The exact scheduling certificate and its local polish already close
    // supported cases without a search tree. Preserve that fast path.
    if has_int && !has_scheduling_certificate {
        let started = clock.now_secs();
        let remaining = (options.time_limit_secs - (started - t0)).max(0.);
        if remaining > 0.25 {
            if let Some(proof) = crate::block_planner::prove(
                original,
                warm.as_deref(),
                options,
                clock,
                started + (remaining * 0.5).min(8.),
                callbacks.interrupt,
            ) {
                certificate_lp_stats += proof.lp_stats;
                certificate_mip_stats += proof.mip_stats;
                certificate_nodes += proof.nodes;
                certificate_iterations += proof.iterations;
                row_bound = Some(row_bound.map_or(proof.bound, |b| b.max(proof.bound)));
                if let Some(point) = proof.point {
                    let objective = original.objective(&point);
                    if warm
                        .as_ref()
                        .is_none_or(|x| objective < original.objective(x) - 1e-9)
                    {
                        if let Some(f) = callbacks.on_incumbent.as_mut() {
                            f(&Incumbent {
                                objective,
                                col_value: point.clone(),
                            });
                        }
                        warm = Some(point);
                    }
                }
                bound_secs += clock.now_secs() - started;
                if let (Some(point), Some(bound)) = (warm.as_ref(), row_bound) {
                    let check = crate::model::check_solution(original, point, 1e-9);
                    if check.feasible
                        && bound <= check.objective
                        && check.objective - bound
                            <= options
                                .mip_abs_gap
                                .max(options.mip_rel_gap * check.objective.abs())
                                .max(1e-7 * (1. + check.objective.abs()))
                    {
                        if let Some(f) = callbacks.on_progress.as_mut() {
                            f(&Progress {
                                running_time: clock.now_secs() - t0,
                                primal_bound: check.objective,
                                dual_bound: bound,
                                gap: ((check.objective - bound) / check.objective.abs().max(1.))
                                    .max(0.),
                                has_incumbent: true,
                                nodes: proof.nodes,
                                lp_iterations: proof.iterations,
                            })
                        }
                        return Solution {
                            status: ModelStatus::Optimal,
                            objective: check.objective,
                            dual_bound: Some(bound),
                            col_value: point.clone(),
                            nodes: proof.nodes,
                            lp_iterations: proof.iterations,
                            presolve_secs,
                            lp_stats: proof.lp_stats,
                            mip_stats: proof.mip_stats,
                            heuristic_secs: (elapsed - presolve_secs).max(0.),
                            bound_secs,
                            ..Default::default()
                        };
                    }
                }
            } else {
                bound_secs += clock.now_secs() - started;
            }
        }
    }

    // Objective local search for the planning shape: take the feasible incumbent
    // and minimise the true objective (peak / range / deviation + finals-day
    // preference) with transfer moves. Giving it a share of the budget is what
    // closes the quality gap to native HiGHS on the planning class. It is a
    // no-op on other models — shape detection returns `None` — so the generic
    // path is unaffected. `check_solution` validates whatever it returns.
    if has_int {
        if let Some(cur) = warm.as_ref() {
            let remaining = (options.time_limit_secs - (clock.now_secs() - t0)).max(0.0);
            let budget = (remaining * 0.5).clamp(0.0, 30.0);
            if budget > 0.01 {
                let mut reported_obj = model.objective(cur);
                let mut report = |x: &[f64]| {
                    let check = crate::model::check_solution(original, x, 1e-9);
                    if check.feasible && check.objective < reported_obj - 1e-9 {
                        reported_obj = check.objective;
                        if let Some(f) = callbacks.on_incumbent.as_mut() {
                            f(&Incumbent {
                                objective: check.objective,
                                col_value: x.to_vec(),
                            });
                        }
                    }
                };
                if let Some((o, x)) = plan_objective_search(
                    model,
                    cur,
                    clock,
                    budget,
                    options.seed,
                    options.threads,
                    options.adaptive_search,
                    &mut report,
                ) {
                    let cur_obj = crate::model::check_solution(model, cur, 1e-6).objective;
                    if o < cur_obj - 1e-9 {
                        if o < reported_obj - 1e-9 {
                            if let Some(f) = callbacks.on_incumbent.as_mut() {
                                f(&Incumbent {
                                    objective: o,
                                    col_value: x.clone(),
                                });
                            }
                        }
                        warm = Some(x);
                    }
                }
            }
        }
    }

    let local_secs = (clock.now_secs() - t0 - elapsed - bound_secs).max(0.0);

    if let (Some(x), Some(bound)) = (warm.as_ref(), row_bound) {
        let check = crate::model::check_solution(original, x, 1e-9);
        if check.feasible
            && bound <= check.objective
            && check.objective - bound
                <= options
                    .mip_abs_gap
                    .max(options.mip_rel_gap * check.objective.abs())
                    .max(1e-7 * (1. + check.objective.abs()))
        {
            if let Some(f) = callbacks.on_progress.as_mut() {
                f(&Progress {
                    running_time: clock.now_secs() - t0,
                    primal_bound: check.objective,
                    dual_bound: bound,
                    gap: ((check.objective - bound) / check.objective.abs().max(1.)).max(0.),
                    has_incumbent: true,
                    ..Default::default()
                });
            }
            return Solution {
                status: ModelStatus::Optimal,
                objective: check.objective,
                dual_bound: Some(bound),
                bound_secs,
                col_value: x.clone(),
                presolve_secs,
                heuristic_secs: (elapsed - presolve_secs).max(0.),
                local_search_secs: local_secs,
                lp_stats: certificate_lp_stats,
                mip_stats: certificate_mip_stats,
                nodes: certificate_nodes,
                lp_iterations: certificate_iterations,
                ..Default::default()
            };
        }
    }

    let cut_started = clock.now_secs();
    let mut strengthened;
    let mut root_cuts = 0;
    let bb_model = if has_int && options.mip_cuts {
        // A tolerance-feasible point can have an objective slightly below the
        // exact optimum. Do not derive hard objective-domain bounds from it.
        let cutoff = warm.as_ref().and_then(|x| {
            let check = crate::model::check_solution(original, x, 1e-9);
            if check.feasible {
                return Some(model.objective(x));
            }
            // Integer definitions often leave tiny fractional auxiliary values
            // in a rounded LP point. Round them, then independently validate the
            // original rows before using this new witness as a hard cutoff.
            let (typed, _) = crate::presolve::strengthen(model, original, None);
            let mut candidate = x.clone();
            for (j, value) in candidate.iter_mut().enumerate() {
                if typed.col_type[j].is_integral() {
                    *value = value.round();
                }
            }
            let check = crate::model::check_solution(original, &candidate, 1e-9);
            check.feasible.then(|| model.objective(&candidate))
        });
        let (m, n) = crate::presolve::strengthen(model, original, cutoff);
        root_cuts = n;
        if n > 0
            || m.row_lower != model.row_lower
            || m.row_upper != model.row_upper
            || m.col_lower != model.col_lower
            || m.col_upper != model.col_upper
        {
            root_lp = None;
        }
        strengthened = m;
        // Root cut loop. The generator runs on a copy of the presolved model,
        // which carries the *true* model integrality, so every inequality is valid
        // for the original feasible set; the cuts are then appended to the
        // strengthened model handed to branch & bound.
        let cut_deadline = (clock.now_secs()
            + ((t0 + options.time_limit_secs - clock.now_secs()) * 0.15).clamp(0.0, 4.0))
        .min(t0 + options.time_limit_secs);
        let mut cut_model = model.clone();
        let (gmi_added, _) = add_root_cuts(&mut cut_model, warm.as_deref(), clock, cut_deadline);
        if gmi_added > 0 {
            for r in model.num_row..cut_model.num_row {
                let entries: Vec<(u32, f64)> =
                    (cut_model.a_start[r] as usize..cut_model.a_start[r + 1] as usize)
                        .map(|k| (cut_model.a_index[k], cut_model.a_value[k]))
                        .collect();
                strengthened.add_row(cut_model.row_lower[r], cut_model.row_upper[r], &entries);
            }
            root_cuts += gmi_added;
            root_lp = None;
        }
        &strengthened
    } else {
        model
    };
    let cut_secs = clock.now_secs() - cut_started;

    // Branch & bound gets whatever wall-clock budget is left (the whole solve,
    // heuristics + local search + B&B, honours `time_limit_secs`).
    let bb_opts = Options {
        time_limit_secs: (options.time_limit_secs - (clock.now_secs() - t0)).max(0.),
        ..options.clone()
    };
    let mut sol = crate::mip::branch_and_bound(
        bb_model,
        &bb_opts,
        clock,
        callbacks,
        warm.as_deref(),
        root_lp.take(),
    );
    if let Some(bound) = row_bound {
        sol.dual_bound = Some(sol.dual_bound.map_or(bound, |b| b.max(bound)));
    }
    sol.bound_secs = bound_secs;
    sol.cut_secs = cut_secs;
    sol.root_cuts = root_cuts;
    sol.inferred_integer_cols = bb_model
        .col_type
        .iter()
        .zip(&model.col_type)
        .filter(|(a, b)| a != b)
        .count();
    sol.presolve_secs = presolve_secs;
    sol.heuristic_secs = (elapsed - presolve_secs).max(0.0);
    sol.lp_stats += certificate_lp_stats;
    sol.mip_stats += certificate_mip_stats;
    sol.nodes += certificate_nodes;
    sol.lp_iterations += certificate_iterations;
    sol.local_search_secs = local_secs;
    sol.bb_secs = (clock.now_secs() - t0 - elapsed - local_secs - cut_secs - bound_secs).max(0.0);
    if sol.has_solution() || sol.status.is_infeasible() || sol.status == ModelStatus::Optimal {
        return sol;
    }

    // B&B inconclusive: prefer the raw incumbent if it is feasible.
    if let Some(x) = warm {
        let check = crate::model::check_solution(model, &x, 1e-6);
        if check.feasible {
            return Solution {
                status: ModelStatus::SolutionLimit,
                objective: check.objective,
                dual_bound: sol.dual_bound,
                col_value: x,
                nodes: sol.nodes,
                lp_iterations: sol.lp_iterations,
                heuristic_secs: sol.heuristic_secs,
                local_search_secs: sol.local_search_secs,
                bb_secs: sol.bb_secs,
                presolve_secs: sol.presolve_secs,
                lp_stats: sol.lp_stats,
                mip_stats: sol.mip_stats,
                cut_secs: sol.cut_secs,
                bound_secs: sol.bound_secs,
                root_cuts: sol.root_cuts,
                inferred_integer_cols: sol.inferred_integer_cols,
            };
        }
    }

    // Otherwise report B&B's outcome (keeps node / iteration counts).
    sol
}

enum Transformation<'a> {
    Counts(crate::objective_counts::ObjectiveCounts<'a>),
    Compression(crate::column_compression::Compression<'a>),
}
impl Transformation<'_> {
    fn model(&self) -> &Model {
        match self {
            Self::Counts(c) => &c.augmented_model,
            Self::Compression(c) => &c.reduced_model,
        }
    }
    fn project_start(&self, point: &[f64]) -> Option<Vec<f64>> {
        match self {
            Self::Counts(c) => c.project_start(point),
            Self::Compression(c) => c.project_start(point),
        }
    }
    fn lift(&self, point: &[f64]) -> Option<Vec<f64>> {
        match self {
            Self::Counts(c) => c.lift(point),
            Self::Compression(c) => c.lift(point),
        }
    }
    fn removed_columns(&self, original: usize) -> u64 {
        original.saturating_sub(self.model().num_col) as u64
    }
    fn added_columns(&self, original: usize) -> u64 {
        self.model().num_col.saturating_sub(original) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::INF;

    #[test]
    fn opt_in_compression_lifts_and_proves_original_model() {
        let mut m = Model::new();
        for _ in 0..10 {
            let j = m.add_col(0.0, 1.0);
            m.set_integrality(j, crate::model::VarType::Binary);
            m.set_cost(j, -3.0);
        }
        m.add_row(-INF, 2.0, &(0..10).map(|j| (j, 1.0)).collect::<Vec<_>>());
        let opts = Options {
            mip_column_compression: true,
            ..Options::default()
        };
        let s = solve(&m, &opts, &StdClock::new());
        assert_eq!(s.status, ModelStatus::Optimal);
        assert_eq!(s.objective, -6.0);
        assert_eq!(s.mip_stats.compressed_columns, 9);
        assert!(crate::model::check_solution(&m, &s.col_value, 1e-9).feasible);
    }

    #[test]
    fn objective_counts_preserve_original_callbacks_and_checked_start() {
        let mut m = Model::new();
        for _ in 0..32 {
            let j = m.add_col(0.0, 1.0);
            m.set_integrality(j, crate::model::VarType::Binary);
            m.set_cost(j, -3.0);
        }
        m.add_row(-INF, 2.0, &(0..32).map(|j| (j, 1.0)).collect::<Vec<_>>());
        let mut start = vec![0.0; 32];
        start[0] = 1.0;
        start[1] = 1.0;
        let mut calls = 0;
        let mut emit = |i: &Incumbent| {
            calls += 1;
            assert_eq!(i.col_value.len(), 32);
            assert!(crate::model::check_solution(&m, &i.col_value, 1e-9).feasible);
            assert_eq!(i.objective, m.objective(&i.col_value));
        };
        let mut callbacks = Callbacks {
            on_incumbent: Some(&mut emit),
            ..Callbacks::default()
        };
        let s = solve_with_start(
            &m,
            &Options::default(),
            &StdClock::new(),
            &mut callbacks,
            Some(&start),
        );
        assert_eq!(s.status, ModelStatus::Optimal);
        assert_eq!(s.objective, -6.0);
        assert_eq!(s.mip_stats.objective_count_columns, 1);
        assert!(calls > 0);
    }

    #[test]
    fn objective_counts_keep_a_checked_start_at_a_numeric_row_edge() {
        let mut m = Model::new();
        for _ in 0..32 {
            let j = m.add_col(0.0, 1.0);
            m.set_integrality(j, crate::model::VarType::Binary);
            m.set_cost(j, -3.0);
        }
        m.add_row(
            -INF,
            3.0f64.next_down(),
            &(0..32).map(|j| (j, 1.0)).collect::<Vec<_>>(),
        );
        let mut start = vec![0.0; 32];
        start[..3].fill(1.0);
        assert!(crate::model::check_solution(&m, &start, 1e-9).feasible);
        let s = solve_with_start(
            &m,
            &Options::default(),
            &StdClock::new(),
            &mut Callbacks::default(),
            Some(&start),
        );
        assert!(!s.col_value.is_empty());
        assert!(s.objective <= m.objective(&start));
    }

    #[test]
    fn initial_point_respects_bounds() {
        let mut m = Model::new();
        m.add_col(2.0, 5.0); // clamp(0) -> 2
        m.add_col(-INF, -1.0); // clamp(0) -> -1
        m.add_col(-1.0, 1.0); // clamp(0) -> 0
        assert_eq!(initial_point(&m), vec![2.0, -1.0, 0.0]);
    }
}
