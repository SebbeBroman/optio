//! Branch & bound on top of the LP relaxation (`lp.rs`).
//!
//! Primal-first: the caller passes the best incumbent found by the feasibility
//! heuristics (`heuristic.rs` + `local_search.rs`), and B&B only improves or
//! proves it. Node LPs are solved with the revised simplex; a node is pruned
//! when its LP bound cannot beat the incumbent. Incumbents and progress are
//! reported through `Callbacks`.

use crate::lp::{LpOptions, LpRelaxation, LpSolution, LpStatus};
use crate::model::{check_solution, Model};
use crate::options::Options;
use crate::solve::{Callbacks, Clock, Solution};
use crate::status::ModelStatus;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

const INT_TOL: f64 = 1e-6;
const CHECK_TOL: f64 = 1e-6;

/// Round the integer columns of `x` and return `(objective, rounded)` if the
/// rounded point is feasible at a tight tolerance. This is a cheap primal
/// heuristic and also the *validation* used before accepting an incumbent, so a
/// near-integer but infeasible LP point can never leak through.
fn round_integer_point(model: &Model, x: &[f64], int_cols: &[usize]) -> Option<(f64, Vec<f64>)> {
    let mut xr = x.to_vec();
    for &j in int_cols {
        xr[j] = xr[j].round();
    }
    // Project continuous auxiliaries first: the LP point can carry a small
    // original-space residual from equilibration scaling (see
    // `heuristic::polish`), which would otherwise reject a valid placement.
    if !crate::heuristic::polish(model, &mut xr) {
        return None;
    }
    let chk = check_solution(model, &xr, CHECK_TOL);
    if chk.feasible {
        Some((chk.objective, xr))
    } else {
        None
    }
}

#[derive(Clone)]
struct Node {
    lo: std::sync::Arc<[f64]>,
    hi: std::sync::Arc<[f64]>,
    /// Lower bound on the objective of any solution in this subtree.
    bound: f64,
    /// True only for the initial node, whose LP the caller may have pre-solved.
    is_root: bool,
    basis: Option<std::sync::Arc<crate::lp::LpBasis>>,
    branch: Option<(usize, bool, f64, f64)>,
    cached: Option<Box<LpSolution>>,
    propagated: bool,
    propagate_full: bool,
    depth: u16,
}

#[path = "branching.rs"]
mod branching;
use branching::Pseudocosts;
#[path = "objective_lattice.rs"]
mod objective_lattice;
use objective_lattice::ObjectiveLattice;

fn children(
    node: &Node,
    j: usize,
    value: f64,
    bound: f64,
    basis: Option<std::sync::Arc<crate::lp::LpBasis>>,
) -> [Node; 2] {
    let mut down = node.clone();
    let mut up = node.clone();
    std::sync::Arc::make_mut(&mut down.hi)[j] = value.floor();
    std::sync::Arc::make_mut(&mut up.lo)[j] = value.ceil();
    for (child, side, distance) in [
        (&mut down, false, value - value.floor()),
        (&mut up, true, value.ceil() - value),
    ] {
        child.bound = bound;
        child.is_root = false;
        child.basis = basis.clone();
        child.branch = Some((j, side, distance, bound));
        child.cached = None;
        child.propagated = false;
        child.propagate_full = false;
        child.depth = node.depth.saturating_add(1);
    }
    [down, up]
}

fn propagate_node(
    node: &mut Node,
    propagation: &mut crate::presolve::PreparedPropagation<'_>,
    clock: &dyn Clock,
    deadline: f64,
    stats: &mut crate::solve::MipStats,
) -> crate::presolve::PropagationResult {
    use crate::presolve::PropagationResult;
    if node.propagated {
        return PropagationResult::Complete { changed: false };
    }
    let started = clock.now_secs();
    let result = propagation.run(
        std::sync::Arc::make_mut(&mut node.lo),
        std::sync::Arc::make_mut(&mut node.hi),
        if node.propagate_full {
            None
        } else {
            node.branch.map(|b| b.0)
        },
        clock,
        deadline,
    );
    stats.propagation_seconds += clock.now_secs() - started;
    match result {
        PropagationResult::Complete { changed } => {
            node.propagated = true;
            if changed {
                node.cached = None;
            }
        }
        PropagationResult::Infeasible => stats.propagation_prunes += 1,
        PropagationResult::TimedOut { .. } => node.propagate_full = true,
    }
    result
}

// Strong branching probes use actual certified LP results for bounds, and
// estimates solely for choosing a branch. Selected children retain their LP for
// reuse. The candidate shortlist and the probe budget are kept generous (rather
// than a small near-root allowance) because on MIPs whose objective barely
// moves, learned pseudocosts are a weak signal and real child-LP gains branch
// far better — that difference is what closes a hard instance like `2122`.
fn select_children(
    node: &Node,
    x: &[f64],
    bound: f64,
    basis: Option<std::sync::Arc<crate::lp::LpBasis>>,
    int_cols: &[usize],
    costs: &mut Pseudocosts,
    lpr: &mut LpRelaxation,
    propagation: &mut crate::presolve::PreparedPropagation<'_>,
    options: &Options,
    lp_opts: &LpOptions,
    clock: &dyn Clock,
    deadline: f64,
    incumbent: Option<f64>,
    iterations: &mut u64,
    lp_stats: &mut crate::lp::LpStats,
    stats: &mut crate::solve::MipStats,
) -> Option<[Node; 2]> {
    let candidates = costs.shortlist(int_cols, x, options.mip_pseudocost, 8);
    let first = candidates.first()?;
    let mut best = children(node, first.column, x[first.column], bound, basis.clone());
    if !options.mip_strong_branch
        || node.depth > 40
        || stats.strong_branch_probes >= 100_000
        || !bound.is_finite()
    {
        return Some(best);
    }
    let mut best_score = f64::NEG_INFINITY;
    let probe_deadline =
        (clock.now_secs() + ((deadline - clock.now_secs()) * 0.05).min(0.1)).min(deadline);
    for candidate in candidates {
        if clock.now_secs() >= probe_deadline || stats.strong_branch_probes >= 100_000 {
            break;
        }
        let mut pair = children(
            node,
            candidate.column,
            x[candidate.column],
            bound,
            basis.clone(),
        );
        pair[0].branch = Some((candidate.column, false, candidate.down_distance, bound));
        pair[1].branch = Some((candidate.column, true, candidate.up_distance, bound));
        let mut gains = [0.; 2];
        let mut completed = true;
        if !candidate.needs_probe {
            if candidate.score > best_score {
                best_score = candidate.score;
                best = pair;
            }
            continue;
        }
        for (side, child) in pair.iter_mut().enumerate() {
            if clock.now_secs() >= probe_deadline || stats.strong_branch_probes >= 100_000 {
                completed = false;
                break;
            }
            if options.presolve && options.mip_node_presolve {
                match propagate_node(child, propagation, clock, probe_deadline, stats) {
                    crate::presolve::PropagationResult::Infeasible => {
                        gains[side] =
                            incumbent.map_or(bound.abs().max(1.), |v| (v - bound).abs().max(1.));
                        child.cached = Some(Box::new(LpSolution {
                            status: LpStatus::Infeasible,
                            ..Default::default()
                        }));
                        child.propagated = true;
                        continue;
                    }
                    crate::presolve::PropagationResult::TimedOut { .. } => {
                        completed = false;
                        break;
                    }
                    _ => {}
                }
            }
            let opts = LpOptions {
                time_limit_secs: (probe_deadline - clock.now_secs()).max(0.),
                max_iterations: 64,
                ..lp_opts.clone()
            };
            let mut lp = lpr.solve_with_basis(
                &child.lo,
                &child.hi,
                &opts,
                clock,
                if options.lp_warm_start {
                    child.basis.as_deref()
                } else {
                    None
                },
            );
            stats.strong_branch_probes += 1;
            *iterations += lp.iterations;
            *lp_stats += lp.stats;
            match lp.status {
                LpStatus::Optimal => {
                    if let Some(b) = lp.dual_bound.filter(|b| b.is_finite()) {
                        gains[side] = (b - bound).max(0.);
                        costs.update(child.branch, b);
                        child.bound = bound.max(b);
                    } else {
                        completed = false;
                    }
                }
                LpStatus::Infeasible => {
                    gains[side] =
                        incumbent.map_or(bound.abs().max(1.), |v| (v - bound).abs().max(1.));
                }
                _ => {
                    completed = false;
                    continue;
                }
            }
            // Work is accounted at the probe, not again when this node pops.
            lp.stats = Default::default();
            lp.iterations = 0;
            child.cached = Some(Box::new(lp));
            child.branch = None; // Already contributed its pseudocost observation.
        }
        let score = if completed {
            branching::gain_score(gains[0], gains[1])
        } else {
            candidate.score
        };
        if score > best_score {
            best_score = score;
            best = pair;
        }
    }
    Some(best)
}

/// Min-heap ordering on the node bound (best-bound search).
struct HeapNode(Node);
impl PartialEq for HeapNode {
    fn eq(&self, other: &Self) -> bool {
        self.0.bound == other.0.bound
    }
}
impl Eq for HeapNode {}
impl PartialOrd for HeapNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for HeapNode {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reversed: BinaryHeap is a max-heap, so the smallest bound pops first.
        other
            .0
            .bound
            .partial_cmp(&self.0.bound)
            .unwrap_or(Ordering::Equal)
    }
}

/// Feasibility pump: alternate (a) rounding the LP point to integers and
/// (b) re-solving an LP that minimises the L1 distance to that rounded point,
/// until the rounding is feasible. General and structure-free.
pub fn feasibility_pump(
    model: &Model,
    options: &Options,
    clock: &dyn Clock,
) -> Option<(f64, Vec<f64>)> {
    use crate::model::VarId;
    use crate::model::INF;

    let int_cols: Vec<usize> = (0..model.num_col)
        .filter(|&j| model.col_type[j].is_integral())
        .collect();
    if int_cols.is_empty() {
        return None;
    }
    let start = clock.now_secs();
    let deadline = (options.time_limit_secs * 0.5).clamp(0.2, 8.0);
    let lp_opts = LpOptions {
        max_iterations: 20_000,
        time_limit_secs: deadline,
        ..LpOptions::default()
    };

    // Initial LP solve.
    let mut lpr = LpRelaxation::build(model);
    let lp = lpr.solve(&lp_opts, clock);
    if lp.status != LpStatus::Optimal {
        return None;
    }
    if let Some(r) = round_integer_point(model, &lp.col_value, &int_cols) {
        return Some(r);
    }
    let mut xtilde: Vec<f64> = lp.col_value.clone();
    for &j in &int_cols {
        xtilde[j] = xtilde[j].round();
    }

    let mut state = 0x9E3779B97F4A7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };

    for it in 0..200 {
        if clock.now_secs() - start > deadline {
            break;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let dbg = std::env::var("OPTIO_LPDEBUG").is_ok();
        #[cfg(target_arch = "wasm32")]
        let dbg = false;
        // Distance variables for every integer column (classic pump).
        let active: Vec<usize> = int_cols.clone();
        if active.is_empty() {
            break;
        }
        let mut aux = model.clone();
        for j in 0..model.num_col {
            aux.set_cost(VarId(j as u32), 0.0);
        }
        for &j in &active {
            let p = aux.add_col(0.0, INF).0;
            let m = aux.add_col(0.0, INF).0;
            aux.set_cost(VarId(p), 1.0);
            aux.set_cost(VarId(m), 1.0);
            aux.add_row(
                xtilde[j],
                xtilde[j],
                &[(j as u32, 1.0), (p, 1.0), (m, -1.0)],
            );
        }
        let mut alpr = LpRelaxation::build(&aux);
        let res = alpr.solve(&lp_opts, clock);
        if dbg && it % 10 == 0 {
            eprintln!(
                "[pump] it={} aux_status={:?} obj={}",
                it, res.status, res.objective
            );
        }
        if res.status != LpStatus::Optimal || res.col_value.len() < model.num_col {
            break;
        }
        let x = &res.col_value[0..model.num_col];
        if let Some(r) = round_integer_point(model, x, &int_cols) {
            return Some(r);
        }
        let mut newtilde = x.to_vec();
        for &j in &int_cols {
            newtilde[j] = newtilde[j].round();
        }
        // Escape cycles: if unchanged (or periodically), randomly perturb a few
        // integer variables so the next pump iteration moves somewhere new.
        let changed = int_cols
            .iter()
            .any(|&j| (newtilde[j] - xtilde[j]).abs() > 0.5);
        if !changed || it % 4 == 3 {
            let k = 1 + (next() * 4.0) as usize;
            for _ in 0..k {
                let j = int_cols[(next() * int_cols.len() as f64) as usize % int_cols.len()];
                newtilde[j] = (x[j].floor() + (next() * 2.0).floor()).max(0.0);
            }
        }
        xtilde = newtilde;
    }
    None
}

/// Depth-first **LP diving** to find a feasible integer point: repeatedly solve
/// the LP, fix the most-fractional integer variable toward an integer, backtrack
/// on infeasibility. Returns a validated `(objective, x)` incumbent, or `None`.
/// This is the general incumbent-finding workhorse that runs before B&B.
pub fn dive_incumbent(
    model: &Model,
    options: &Options,
    clock: &dyn Clock,
) -> Option<(f64, Vec<f64>)> {
    let int_cols: Vec<usize> = (0..model.num_col)
        .filter(|&j| model.col_type[j].is_integral())
        .collect();
    if int_cols.is_empty() {
        return None;
    }
    let mut lpr = LpRelaxation::build(model);
    let start = clock.now_secs();
    let deadline = (options.time_limit_secs * 0.5).clamp(0.2, 8.0);
    let mut lp_opts = LpOptions {
        max_iterations: 20_000,
        time_limit_secs: deadline,
        ..LpOptions::default()
    };
    let max_nodes = 3_000u64;
    let mut nodes = 0u64;
    let mut stack: Vec<(Vec<f64>, Vec<f64>)> =
        vec![(model.col_lower.clone(), model.col_upper.clone())];

    while let Some((lo, hi)) = stack.pop() {
        if nodes >= max_nodes || clock.now_secs() - start > deadline {
            break;
        }
        nodes += 1;
        #[cfg(not(target_arch = "wasm32"))]
        let dbg = std::env::var("OPTIO_LPDEBUG").is_ok();
        #[cfg(target_arch = "wasm32")]
        let dbg = false;
        lp_opts.time_limit_secs = (deadline - (clock.now_secs() - start)).max(0.02);
        let lp = lpr.solve_with_bounds(&lo, &hi, &lp_opts, clock);
        if dbg && nodes % 500 == 0 {
            eprintln!(
                "[dive] node {} status={:?} obj={}",
                nodes, lp.status, lp.objective
            );
        }
        if lp.status != LpStatus::Optimal {
            continue;
        }
        if let Some(r) = round_integer_point(model, &lp.col_value, &int_cols) {
            return Some(r);
        }
        // Branch on the most fractional integer variable; explore the "down"
        // side first (pushed last -> popped first).
        let mut bj = usize::MAX;
        let mut bf = INT_TOL;
        for &j in &int_cols {
            let v = lp.col_value[j];
            let f = (v - v.round()).abs();
            if f > bf {
                bf = f;
                bj = j;
            }
        }
        if bj == usize::MAX {
            continue;
        }
        let v = lp.col_value[bj];
        let down = v.floor();
        let up = v.ceil();
        let lo_d = lo.clone();
        let mut hi_d = hi.clone();
        hi_d[bj] = hi_d[bj].min(down);
        let mut lo_u = lo;
        let hi_u = hi;
        lo_u[bj] = lo_u[bj].max(up);
        stack.push((lo_u, hi_u));
        stack.push((lo_d, hi_d));
    }
    None
}

/// Native multi-core branch & bound (feature `parallel`). Workers share a node
/// stack and an atomic incumbent; the `Model` stays immutable and each worker
/// owns its own `LpRelaxation`, so there is no shared mutable LP state.
///
/// Progress callbacks are not invoked here (the callback closures are not
/// `Send`); the caller gets the final solution. The web/wasm build never takes
/// this path (`parallel` is off, `threads == 1`).
#[cfg(feature = "parallel")]
fn branch_and_bound_mt(
    model: &Model,
    options: &Options,
    clock: &dyn Clock,
    warm_start: Option<&[f64]>,
    root_lp: Option<LpSolution>,
    interrupt: Option<&dyn Fn() -> bool>,
) -> Solution {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::Mutex;

    let int_cols: Vec<usize> = (0..model.num_col)
        .filter(|&j| model.col_type[j].is_integral())
        .collect();
    if int_cols.is_empty() {
        let mut lpr = LpRelaxation::build(model);
        let lp = lpr.solve(
            &LpOptions {
                max_iterations: 20_000,
                time_limit_secs: options.time_limit_secs,
                ..LpOptions::default()
            },
            clock,
        );
        return match lp.status {
            LpStatus::Optimal => Solution {
                status: ModelStatus::Optimal,
                objective: lp.objective,
                dual_bound: lp.dual_bound,
                col_value: lp.col_value,
                nodes: 1,
                lp_iterations: lp.iterations,
                lp_stats: lp.stats,
                ..Default::default()
            },
            LpStatus::Infeasible => Solution {
                status: ModelStatus::Infeasible,
                nodes: 1,
                lp_iterations: lp.iterations,
                lp_stats: lp.stats,
                ..Default::default()
            },
            LpStatus::Unbounded => Solution {
                status: ModelStatus::Unbounded,
                nodes: 1,
                lp_iterations: lp.iterations,
                lp_stats: lp.stats,
                ..Default::default()
            },
            _ => Solution {
                status: ModelStatus::Unknown,
                nodes: 1,
                lp_iterations: lp.iterations,
                lp_stats: lp.stats,
                ..Default::default()
            },
        };
    }

    let start = clock.now_secs();
    let deadline = options.time_limit_secs;
    let lattice = options
        .mip_objective_lattice
        .then(|| ObjectiveLattice::build(model))
        .flatten();
    let threads = if options.threads == 0 {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        options.threads
    };

    // Incumbent: objective as f64 bits (fast pruning) + the solution vector.
    let inc_obj = AtomicU64::new(f64::to_bits(f64::INFINITY));
    let inc_sol: Mutex<Option<Vec<f64>>> = Mutex::new(None);
    if let Some(x0) = warm_start {
        let chk = check_solution(model, x0, CHECK_TOL);
        if chk.feasible {
            inc_obj.store(f64::to_bits(chk.objective), Ordering::Relaxed);
            *inc_sol.lock().unwrap() = Some(x0.to_vec());
        }
    }

    // One LIFO work-stealing deque per worker: local `push`/`pop` are lock-free;
    // only a steal (when a worker runs dry) touches any synchronization. This
    // keeps the per-node hot path free of locks — the only shared state is the
    // atomic incumbent objective and a task counter.
    let root = Node {
        lo: model.col_lower.clone().into(),
        hi: model.col_upper.clone().into(),
        bound: f64::NEG_INFINITY,
        is_root: true,
        basis: None,
        branch: None,
        cached: None,
        propagated: false,
        propagate_full: true,
        depth: 0,
    };
    let workers: Vec<crossbeam_deque::Worker<Node>> = (0..threads)
        .map(|_| crossbeam_deque::Worker::new_lifo())
        .collect();
    let stealers: std::sync::Arc<Vec<crossbeam_deque::Stealer<Node>>> =
        std::sync::Arc::new(workers.iter().map(|w| w.stealer()).collect());
    // Pre-solved root LP (if the caller's heuristic already solved it). Only the
    // root node consumes it; the mutex `take` makes that a single, lock-free-ish
    // (one-time) handoff regardless of which worker ends up processing the root.
    let root_lp_cache: std::sync::Arc<Mutex<Option<LpSolution>>> =
        std::sync::Arc::new(Mutex::new(root_lp));

    // Pending tasks = queued nodes + workers currently processing one; the
    // search ends when it reaches zero. Two atomics per node, no lock.
    let work = AtomicUsize::new(1);
    let nodes = AtomicU64::new(0);
    let lp_iters = AtomicU64::new(0);
    let stats = Mutex::new(crate::lp::LpStats::default());
    let mip_stats = Mutex::new(crate::solve::MipStats::default());
    let node_limit = AtomicBool::new(false);
    let interrupted = AtomicBool::new(false);
    let saw_unbounded = AtomicBool::new(false);
    let timed_out = AtomicBool::new(false);
    let unresolved = AtomicBool::new(false);
    let global_bound = AtomicU64::new(f64::NEG_INFINITY.to_bits());
    let worker_bounds: Vec<AtomicU64> = (0..threads)
        .map(|_| AtomicU64::new(f64::INFINITY.to_bits()))
        .collect();
    // Borrows shared read-only/atomic state into the workers.
    let (inc_obj_r, inc_sol_r) = (&inc_obj, &inc_sol);
    let (work_r, nodes_r, lp_iters_r) = (&work, &nodes, &lp_iters);
    let (unbounded_r, timeout_r) = (&saw_unbounded, &timed_out);
    let int_cols_r: &[usize] = &int_cols;
    let (unresolved_r, global_bound_r) = (&unresolved, &global_bound);
    let worker_bounds_r = &worker_bounds;
    let stats_r = &stats;
    let mip_stats_r = &mip_stats;
    let node_limit_r = &node_limit;
    let interrupted_r = &interrupted;
    let lattice_r = &lattice;

    std::thread::scope(|scope| {
        for (me, deque) in workers.into_iter().enumerate() {
            let stealers = std::sync::Arc::clone(&stealers);
            let root_lp_cache = std::sync::Arc::clone(&root_lp_cache);
            let root = if me == 0 { Some(root.clone()) } else { None };
            scope.spawn(move || {
                if let Some(r) = root {
                    deque.push(r);
                }
                let mut pseudocosts = Pseudocosts::new(model.num_col);
                let mut local_stats = crate::lp::LpStats::default();
                let mut local_mip_stats = crate::solve::MipStats::default();
                let mut propagation = crate::presolve::PreparedPropagation::new(model);
                let mut lpr = LpRelaxation::build(model);
                let mut lp_opts = LpOptions {
                    max_iterations: 20_000,
                    time_limit_secs: deadline,
                    ..LpOptions::default()
                };
                let nworkers = stealers.len();
                let mut remaining_bound = f64::INFINITY;
                loop {
                    if interrupted_r.load(Ordering::Relaxed) || work_r.load(Ordering::Acquire) == 0
                    {
                        break;
                    }
                    if clock.now_secs() - start > deadline {
                        timeout_r.store(true, Ordering::Relaxed);
                        break;
                    }
                    if nodes_r.load(Ordering::Relaxed) >= options.mip_max_nodes {
                        node_limit_r.store(true, Ordering::Relaxed);
                        break;
                    }
                    let mut node = match deque.pop() {
                        Some(n) => n,
                        None => {
                            let mut stolen = None;
                            for k in 1..=nworkers {
                                let idx = (me + k) % nworkers;
                                if idx == me {
                                    continue;
                                }
                                if let crossbeam_deque::Steal::Success(t) = stealers[idx].steal() {
                                    stolen = Some(t);
                                    break;
                                }
                            }
                            match stolen {
                                Some(t) => t,
                                None => {
                                    std::thread::yield_now();
                                    continue;
                                }
                            }
                        }
                    };
                    let inc = f64::from_bits(inc_obj_r.load(Ordering::Relaxed));
                    let tol = options.mip_abs_gap.max(options.mip_rel_gap * inc.abs());
                    if inc.is_finite() && node.bound >= inc - tol - 1e-9 {
                        remaining_bound = remaining_bound.min(node.bound);
                        work_r.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    if options.presolve && options.mip_node_presolve {
                        match propagate_node(
                            &mut node,
                            &mut propagation,
                            clock,
                            start + deadline,
                            &mut local_mip_stats,
                        ) {
                            crate::presolve::PropagationResult::Infeasible => {
                                work_r.fetch_sub(1, Ordering::AcqRel);
                                continue;
                            }
                            crate::presolve::PropagationResult::TimedOut { .. } => {
                                remaining_bound = remaining_bound.min(node.bound);
                                timeout_r.store(true, Ordering::Relaxed);
                                work_r.fetch_sub(1, Ordering::AcqRel);
                                break;
                            }
                            crate::presolve::PropagationResult::Complete { changed } => {
                                if changed && node.is_root {
                                    root_lp_cache.lock().unwrap().take();
                                }
                            }
                        }
                    }
                    let elapsed = clock.now_secs() - start;
                    lp_opts.time_limit_secs = (deadline - elapsed).max(0.0);
                    // The root node reuses the root LP the primal heuristic already
                    // solved, if one was handed in; every other node solves fresh.
                    let mut lp = if let Some(cached) = node.cached.take() {
                        *cached
                    } else if node.is_root {
                        let cached = root_lp_cache.lock().unwrap().take();
                        cached.unwrap_or_else(|| {
                            lpr.solve_with_basis(
                                &node.lo,
                                &node.hi,
                                &lp_opts,
                                clock,
                                if options.lp_warm_start {
                                    node.basis.as_deref()
                                } else {
                                    None
                                },
                            )
                        })
                    } else {
                        lpr.solve_with_basis(
                            &node.lo,
                            &node.hi,
                            &lp_opts,
                            clock,
                            if options.lp_warm_start {
                                node.basis.as_deref()
                            } else {
                                None
                            },
                        )
                    };
                    local_stats += lp.stats;
                    nodes_r.fetch_add(1, Ordering::Relaxed);
                    lp_iters_r.fetch_add(lp.iterations, Ordering::Relaxed);

                    if matches!(lp.status, LpStatus::Numerical | LpStatus::IterationLimit)
                        && clock.now_secs() < start + deadline
                    {
                        let retry_opts = LpOptions {
                            time_limit_secs: (start + deadline - clock.now_secs()).max(0.),
                            max_iterations: lp_opts.max_iterations.saturating_mul(2),
                            ..lp_opts.clone()
                        };
                        let retry =
                            lpr.solve_with_basis(&node.lo, &node.hi, &retry_opts, clock, None);
                        local_stats += retry.stats;
                        lp_iters_r.fetch_add(retry.iterations, Ordering::Relaxed);
                        local_mip_stats.numerical_retries += 1;
                        lp = retry;
                    }
                    let child_basis = lp.basis.clone();
                    let x = match lp.status {
                        LpStatus::Optimal => lp.col_value,
                        LpStatus::Infeasible => {
                            work_r.fetch_sub(1, Ordering::AcqRel);
                            continue;
                        }
                        LpStatus::Unbounded => {
                            unbounded_r.store(true, Ordering::Relaxed);
                            unresolved_r.store(true, Ordering::Relaxed);
                            remaining_bound = remaining_bound.min(node.bound);
                            work_r.fetch_sub(1, Ordering::AcqRel);
                            continue;
                        }
                        _ => {
                            unresolved_r.store(true, Ordering::Relaxed);
                            remaining_bound = remaining_bound.min(node.bound);
                            work_r.fetch_sub(1, Ordering::AcqRel);
                            continue;
                        }
                    };
                    let obj = lp.dual_bound.unwrap_or(f64::NEG_INFINITY);
                    let obj = lattice_r.as_ref().map_or(obj, |l| l.round_up(obj));
                    if node.is_root {
                        global_bound_r.store(obj.to_bits(), Ordering::Relaxed);
                    }
                    let inc = f64::from_bits(inc_obj_r.load(Ordering::Relaxed));
                    let tol = options.mip_abs_gap.max(options.mip_rel_gap * inc.abs());
                    if inc.is_finite() && obj >= inc - tol - 1e-9 {
                        remaining_bound = remaining_bound.min(obj);
                        work_r.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    if let Some((robj, rx)) = round_integer_point(model, &x, int_cols_r) {
                        let cur = f64::from_bits(inc_obj_r.load(Ordering::Relaxed));
                        if !cur.is_finite() || robj < cur - 1e-9 {
                            let mut sol = inc_sol_r.lock().unwrap();
                            let cur = f64::from_bits(inc_obj_r.load(Ordering::Relaxed));
                            if !cur.is_finite() || robj < cur - 1e-9 {
                                inc_obj_r.store(f64::to_bits(robj), Ordering::Relaxed);
                                *sol = Some(rx);
                            }
                        }
                    }
                    pseudocosts.update(node.branch, obj);
                    let mut probe_iters = 0;
                    let current_inc = f64::from_bits(inc_obj_r.load(Ordering::Relaxed));
                    if let Some(pair) = select_children(
                        &node,
                        &x,
                        obj,
                        child_basis,
                        int_cols_r,
                        &mut pseudocosts,
                        &mut lpr,
                        &mut propagation,
                        options,
                        &lp_opts,
                        clock,
                        start + deadline,
                        current_inc.is_finite().then_some(current_inc),
                        &mut probe_iters,
                        &mut local_stats,
                        &mut local_mip_stats,
                    ) {
                        work_r.fetch_add(2, Ordering::AcqRel);
                        for child in pair {
                            deque.push(child);
                        }
                    } else {
                        remaining_bound = remaining_bound.min(obj);
                        if round_integer_point(model, &x, int_cols_r).is_none() {
                            unresolved_r.store(true, Ordering::Relaxed);
                        }
                    }
                    lp_iters_r.fetch_add(probe_iters, Ordering::Relaxed);
                    work_r.fetch_sub(1, Ordering::AcqRel);
                }
                while let Some(node) = deque.pop() {
                    remaining_bound = remaining_bound.min(node.bound);
                }
                *stats_r.lock().unwrap() += local_stats;
                *mip_stats_r.lock().unwrap() += local_mip_stats;
                worker_bounds_r[me].store(remaining_bound.to_bits(), Ordering::Relaxed);
            });
        }
        // Poll the non-Send callback on the caller thread while workers use an
        // atomic stop flag. Worker LP deadlines remain independently enforced.
        while work.load(Ordering::Acquire) != 0
            && !timed_out.load(Ordering::Relaxed)
            && !node_limit.load(Ordering::Relaxed)
        {
            if interrupt.is_some_and(|f| f()) {
                interrupted.store(true, Ordering::Relaxed);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    });

    let nodes_v = nodes.load(Ordering::Relaxed);
    let iters_v = lp_iters.load(Ordering::Relaxed);
    let obj = f64::from_bits(inc_obj.load(Ordering::Relaxed));
    let final_sol: Option<Vec<f64>> = inc_sol.lock().unwrap().take();
    let frontier_bound = worker_bounds
        .iter()
        .map(|b| f64::from_bits(b.load(Ordering::Relaxed)))
        .fold(obj, f64::min);
    let final_bound = frontier_bound
        .max(f64::from_bits(global_bound.load(Ordering::Relaxed)))
        .min(obj);
    match final_sol {
        Some(x) => Solution {
            status: if interrupted.load(Ordering::Relaxed) {
                ModelStatus::Interrupt
            } else if obj - final_bound
                <= options
                    .mip_abs_gap
                    .max(options.mip_rel_gap * obj.abs())
                    .max(1e-7 * (1.0 + obj.abs()))
            {
                ModelStatus::Optimal
            } else if timed_out.load(Ordering::Relaxed) || unresolved.load(Ordering::Relaxed) {
                ModelStatus::TimeLimit
            } else {
                ModelStatus::SolutionLimit
            },
            objective: obj,
            dual_bound: Some(final_bound),
            col_value: x,
            nodes: nodes_v,
            lp_iterations: iters_v,
            lp_stats: *stats.lock().unwrap(),
            mip_stats: *mip_stats.lock().unwrap(),
            ..Default::default()
        },
        None => Solution {
            dual_bound: Some(final_bound),
            nodes: nodes_v,
            lp_iterations: iters_v,
            lp_stats: *stats.lock().unwrap(),
            mip_stats: *mip_stats.lock().unwrap(),
            status: if interrupted.load(Ordering::Relaxed) {
                ModelStatus::Interrupt
            } else if saw_unbounded.load(Ordering::Relaxed) {
                ModelStatus::UnboundedOrInfeasible
            } else if node_limit.load(Ordering::Relaxed) {
                ModelStatus::SolutionLimit
            } else if timed_out.load(Ordering::Relaxed) || unresolved.load(Ordering::Relaxed) {
                ModelStatus::Unknown
            } else {
                ModelStatus::Infeasible
            },
            ..Default::default()
        },
    }
}

/// Branch & bound for the MILP.
#[allow(clippy::too_many_arguments)]
pub fn branch_and_bound(
    model: &Model,
    options: &Options,
    clock: &dyn Clock,
    callbacks: &mut Callbacks<'_>,
    warm_start: Option<&[f64]>,
    root_lp: Option<LpSolution>,
) -> Solution {
    if model.num_col == 0 {
        return Solution {
            status: ModelStatus::Empty,
            ..Default::default()
        };
    }

    #[cfg(feature = "parallel")]
    if options.threads != 1 {
        return branch_and_bound_mt(
            model,
            options,
            clock,
            warm_start,
            root_lp,
            callbacks.interrupt,
        );
    }

    let int_cols: Vec<usize> = (0..model.num_col)
        .filter(|&j| model.col_type[j].is_integral())
        .collect();

    let start = clock.now_secs();
    let deadline = options.time_limit_secs;
    let lattice = options
        .mip_objective_lattice
        .then(|| ObjectiveLattice::build(model))
        .flatten();
    let mut lpr = LpRelaxation::build(model);
    let mut lp_opts = LpOptions {
        max_iterations: 20_000,
        time_limit_secs: options.time_limit_secs,
        ..LpOptions::default()
    };

    lp_opts.time_limit_secs = (deadline - (clock.now_secs() - start)).max(0.);

    // Primal incumbent (seeded by the caller's heuristic / warm start).
    let mut incumbent: Option<(f64, Vec<f64>)> = None;
    let report =
        |inc: &mut Option<(f64, Vec<f64>)>, x: Vec<f64>, obj: f64, cb: &mut Callbacks<'_>| {
            let better = inc.as_ref().map_or(true, |(o, _)| obj < *o - 1e-9);
            if better {
                *inc = Some((obj, x.clone()));
                if let Some(f) = cb.on_incumbent.as_mut() {
                    f(&crate::solve::Incumbent {
                        objective: obj,
                        col_value: x,
                    });
                }
            }
        };
    if let Some(x0) = warm_start {
        let chk = check_solution(model, x0, 1e-6);
        if chk.feasible {
            report(&mut incumbent, x0.to_vec(), chk.objective, callbacks);
        }
    }

    // No integer variables: the LP relaxation is the answer.
    if int_cols.is_empty() {
        let lp = lpr.solve(&lp_opts, clock);
        return match lp.status {
            LpStatus::Optimal => Solution {
                status: ModelStatus::Optimal,
                objective: lp.objective,
                dual_bound: lp.dual_bound,
                col_value: lp.col_value,
                nodes: 1,
                lp_iterations: lp.iterations,
                lp_stats: lp.stats,
                ..Default::default()
            },
            LpStatus::Infeasible => Solution {
                status: ModelStatus::Infeasible,
                nodes: 1,
                lp_iterations: lp.iterations,
                lp_stats: lp.stats,
                ..Default::default()
            },
            LpStatus::Unbounded => Solution {
                status: ModelStatus::Unbounded,
                nodes: 1,
                lp_iterations: lp.iterations,
                lp_stats: lp.stats,
                ..Default::default()
            },
            LpStatus::TimeLimit => Solution {
                status: ModelStatus::TimeLimit,
                nodes: 1,
                lp_iterations: lp.iterations,
                lp_stats: lp.stats,
                ..Default::default()
            },
            _ => Solution {
                status: ModelStatus::Unknown,
                nodes: 1,
                lp_iterations: lp.iterations,
                lp_stats: lp.stats,
                ..Default::default()
            },
        };
    }

    let mut nodes = 0u64;
    let mut lp_iterations = 0u64;
    let mut lp_stats = crate::lp::LpStats::default();
    let mut pseudocosts = Pseudocosts::new(model.num_col);
    let mut propagation = crate::presolve::PreparedPropagation::new(model);
    let mut mip_stats = crate::solve::MipStats::default();
    let mut proved = true;
    let mut node_limit_hit = false;
    let mut saw_unbounded = false;
    let mut global_bound = None;
    let mut remaining_bound = f64::INFINITY;
    #[cfg(not(target_arch = "wasm32"))]
    let dbg = std::env::var("OPTIO_LPDEBUG").is_ok();
    #[cfg(target_arch = "wasm32")]
    let dbg = false;
    if dbg {
        eprintln!(
            "[mip] int_cols={} deadline={:.3} has_warm={}",
            int_cols.len(),
            deadline,
            incumbent.is_some()
        );
    }
    // The root LP the primal heuristic solved (if any). Consumed by the root
    // node below so the root relaxation is solved once, not twice.
    let mut root_lp = root_lp;
    let mut stack: BinaryHeap<HeapNode> = BinaryHeap::new();
    stack.push(HeapNode(Node {
        lo: model.col_lower.clone().into(),
        hi: model.col_upper.clone().into(),
        bound: f64::NEG_INFINITY,
        is_root: true,
        basis: None,
        branch: None,
        cached: None,
        propagated: false,
        propagate_full: true,
        depth: 0,
    }));

    while let Some(HeapNode(mut node)) = stack.pop() {
        if clock.now_secs() - start > deadline {
            remaining_bound = remaining_bound.min(node.bound);
            proved = false;
            break;
        }
        if let Some(f) = callbacks.interrupt {
            if f() {
                remaining_bound = remaining_bound.min(node.bound);
                proved = false;
                break;
            }
        }
        if nodes >= options.mip_max_nodes {
            node_limit_hit = true;
            remaining_bound = remaining_bound.min(node.bound);
            proved = false;
            break;
        }
        // Best-bound prune: if this subtree's optimistic bound cannot beat the
        // incumbent, we can discard it without solving its LP.
        if let Some((inc, _)) = &incumbent {
            let tol = options.mip_abs_gap.max(options.mip_rel_gap * inc.abs());
            if node.bound >= *inc - tol - 1e-9 {
                remaining_bound = remaining_bound.min(node.bound);
                continue;
            }
        }
        if options.presolve && options.mip_node_presolve {
            match propagate_node(
                &mut node,
                &mut propagation,
                clock,
                start + deadline,
                &mut mip_stats,
            ) {
                crate::presolve::PropagationResult::Infeasible => continue,
                crate::presolve::PropagationResult::TimedOut { .. } => {
                    remaining_bound = remaining_bound.min(node.bound);
                    proved = false;
                    break;
                }
                crate::presolve::PropagationResult::Complete { changed } => {
                    if changed && node.is_root {
                        root_lp = None;
                    }
                }
            }
        }
        // Give each node LP only the remaining wall-clock budget.
        let elapsed = clock.now_secs() - start;
        lp_opts.time_limit_secs = (deadline - elapsed).max(0.0);

        // The root node reuses the root LP the primal heuristic solved (if one
        // was handed in); it is the exact same relaxation with model bounds, so
        // re-solving would only repeat work. Every other node solves fresh.
        let mut lp = if let Some(cached) = node.cached.take() {
            *cached
        } else if node.is_root {
            match root_lp.take() {
                Some(cached) => cached,
                None => lpr.solve_with_basis(
                    &node.lo,
                    &node.hi,
                    &lp_opts,
                    clock,
                    if options.lp_warm_start {
                        node.basis.as_deref()
                    } else {
                        None
                    },
                ),
            }
        } else {
            lpr.solve_with_basis(
                &node.lo,
                &node.hi,
                &lp_opts,
                clock,
                if options.lp_warm_start {
                    node.basis.as_deref()
                } else {
                    None
                },
            )
        };
        nodes += 1;
        lp_iterations += lp.iterations;
        lp_stats += lp.stats;
        if dbg {
            eprintln!(
                "[mip] node {} bound={} status={:?} obj={} iters={}",
                nodes, node.bound, lp.status, lp.objective, lp.iterations
            );
        }

        // A single cold retry can recover a suspect warm basis or pivot.
        if matches!(lp.status, LpStatus::Numerical | LpStatus::IterationLimit)
            && clock.now_secs() < start + deadline
        {
            let retry_opts = LpOptions {
                time_limit_secs: (start + deadline - clock.now_secs()).max(0.),
                max_iterations: lp_opts.max_iterations.saturating_mul(2),
                ..lp_opts.clone()
            };
            let retry = lpr.solve_with_basis(&node.lo, &node.hi, &retry_opts, clock, None);
            lp_iterations += retry.iterations;
            lp_stats += retry.stats;
            mip_stats.numerical_retries += 1;
            lp = retry;
        }
        let child_basis = lp.basis.clone();
        let (obj, x) = match lp.status {
            LpStatus::Optimal => {
                let bound = lp.dual_bound.unwrap_or(f64::NEG_INFINITY);
                (
                    lattice.as_ref().map_or(bound, |l| l.round_up(bound)),
                    lp.col_value,
                )
            }
            LpStatus::Infeasible => continue,
            LpStatus::Unbounded => {
                // The relaxation is unbounded; for a bounded MIP this means the
                // model is unbounded (or the node is numerically nasty).
                remaining_bound = remaining_bound.min(node.bound);
                saw_unbounded = true;
                proved = false;
                continue;
            }
            _ => {
                remaining_bound = remaining_bound.min(node.bound);
                // Numerical / iteration limit: cannot use this node for bounds.
                proved = false;
                continue;
            }
        };

        if node.is_root {
            global_bound = Some(obj);
        }

        // Prune if this node cannot beat the incumbent.
        if let Some((inc, _)) = &incumbent {
            let tol = options.mip_abs_gap.max(options.mip_rel_gap * inc.abs());
            if obj >= *inc - tol - 1e-9 {
                remaining_bound = remaining_bound.min(obj);
                continue;
            }
        }

        // Primal heuristic: round this LP point and validate tightly. A cheap
        // and effective way to get incumbents (esp. near-integral LPs).
        if let Some((robj, rx)) = round_integer_point(model, &x, &int_cols) {
            report(&mut incumbent, rx, robj, callbacks);
        }

        pseudocosts.update(node.branch, obj);
        if let Some(pair) = select_children(
            &node,
            &x,
            obj,
            child_basis,
            &int_cols,
            &mut pseudocosts,
            &mut lpr,
            &mut propagation,
            options,
            &lp_opts,
            clock,
            start + deadline,
            incumbent.as_ref().map(|v| v.0),
            &mut lp_iterations,
            &mut lp_stats,
            &mut mip_stats,
        ) {
            for child in pair {
                stack.push(HeapNode(child));
            }
        } else {
            remaining_bound = remaining_bound.min(obj);
            if round_integer_point(model, &x, &int_cols).is_none() {
                proved = false;
            }
        }

        // Progress callback (cheap, every 32 nodes).
        if nodes % 32 == 0 {
            if let Some(f) = callbacks.on_progress.as_mut() {
                let frontier = stack
                    .peek()
                    .map_or(f64::INFINITY, |n| n.0.bound)
                    .min(remaining_bound);
                let (primal, dual, has) = match &incumbent {
                    Some((o, _)) => (Some(*o), Some(frontier.min(*o)), true),
                    None => (None, Some(frontier), false),
                };
                let gap = match (primal, dual) {
                    (Some(p), Some(d)) if d.abs() > 1e-12 => ((p - d) / d.abs()).max(0.0),
                    _ => f64::INFINITY,
                };
                f(&crate::solve::Progress {
                    running_time: clock.now_secs() - start,
                    nodes,
                    lp_iterations,
                    gap,
                    primal_bound: primal.unwrap_or(f64::NAN),
                    dual_bound: dual.unwrap_or(f64::NAN),
                    has_incumbent: has,
                });
            }
        }
    }

    let status = if saw_unbounded && incumbent.is_none() {
        ModelStatus::UnboundedOrInfeasible
    } else if let Some(_) = &incumbent {
        if proved && clock.now_secs() - start <= deadline {
            ModelStatus::Optimal
        } else if node_limit_hit {
            ModelStatus::SolutionLimit
        } else {
            ModelStatus::TimeLimit
        }
    } else if proved {
        ModelStatus::Infeasible
    } else if node_limit_hit {
        ModelStatus::SolutionLimit
    } else {
        ModelStatus::Unknown
    };

    let frontier = stack
        .peek()
        .map_or(f64::INFINITY, |n| n.0.bound)
        .min(remaining_bound);
    let cap = incumbent.as_ref().map_or(f64::INFINITY, |(o, _)| *o);
    let final_bound = frontier
        .max(global_bound.unwrap_or(f64::NEG_INFINITY))
        .min(cap);
    let (objective, col_value) = incumbent.unwrap_or((0.0, Vec::new()));
    let status = if !saw_unbounded
        && !col_value.is_empty()
        && objective - final_bound
            <= options
                .mip_abs_gap
                .max(options.mip_rel_gap * objective.abs())
                .max(1e-7 * (1. + objective.abs()))
    {
        ModelStatus::Optimal
    } else {
        status
    };
    let status = if status == ModelStatus::Optimal
        && objective - final_bound
            > options
                .mip_abs_gap
                .max(options.mip_rel_gap * objective.abs())
                .max(1e-7 * (1.0 + objective.abs()))
    {
        ModelStatus::SolutionLimit
    } else {
        status
    };
    Solution {
        status,
        objective,
        dual_bound: Some(final_bound),
        col_value,
        nodes,
        lp_iterations,
        lp_stats,
        mip_stats,
        ..Default::default()
    }
}

#[cfg(test)]
mod proof_engine_tests {
    use super::*;
    use crate::{model::VarType, StdClock};

    fn knapsack() -> Model {
        let mut m = Model::new();
        for cost in [-12., -7., -6.] {
            let j = m.add_col(0., 1.);
            m.set_cost(j, cost);
            m.set_integrality(j, VarType::Integer);
        }
        m.add_row(-crate::model::INF, 4., &[(0, 3.), (1, 2.), (2, 2.)]);
        m
    }

    #[test]
    fn propagation_and_probes_preserve_exhaustive_optimum() {
        let m = knapsack();
        let mut optimum = f64::INFINITY;
        for bits in 0..8 {
            let x: Vec<_> = (0..3).map(|j| ((bits >> j) & 1) as f64).collect();
            let check = check_solution(&m, &x, 1e-9);
            if check.feasible {
                optimum = optimum.min(check.objective);
            }
        }
        for enabled in [false, true] {
            let clock = StdClock::new();
            let opts = Options {
                mip_node_presolve: enabled,
                mip_strong_branch: enabled,
                mip_rel_gap: 0.,
                time_limit_secs: 2.,
                ..Default::default()
            };
            let s = branch_and_bound(&m, &opts, &clock, &mut Callbacks::default(), None, None);
            assert_eq!(s.status, ModelStatus::Optimal);
            assert_eq!(s.objective, optimum);
            assert!(check_solution(&m, &s.col_value, 1e-9).feasible);
            assert!(s.dual_bound.unwrap() <= optimum);
            if enabled {
                assert!(s.mip_stats.strong_branch_probes > 0);
            }
        }
    }

    #[test]
    fn explicit_node_limit_is_reported_without_false_proof() {
        let m = knapsack();
        let clock = StdClock::new();
        let opts = Options {
            mip_max_nodes: 0,
            ..Default::default()
        };
        let s = branch_and_bound(
            &m,
            &opts,
            &clock,
            &mut Callbacks::default(),
            Some(&[0., 1., 1.]),
            None,
        );
        assert_eq!(s.status, ModelStatus::SolutionLimit);
        assert_eq!(s.nodes, 0);
        assert!(s.dual_bound.unwrap() < s.objective);
    }

    #[test]
    fn selected_probe_results_are_accounted_once() {
        let m = knapsack();
        let clock = StdClock::new();
        let mut lpr = LpRelaxation::build(&m);
        let lp_opts = LpOptions::default();
        let lp = lpr.solve(&lp_opts, &clock);
        assert_eq!(lp.status, LpStatus::Optimal);
        let node = Node {
            lo: m.col_lower.clone().into(),
            hi: m.col_upper.clone().into(),
            bound: lp.dual_bound.unwrap(),
            is_root: true,
            basis: lp.basis.clone(),
            branch: None,
            cached: None,
            propagated: false,
            propagate_full: true,
            depth: 0,
        };
        let mut costs = Pseudocosts::new(m.num_col);
        let mut propagation = crate::presolve::PreparedPropagation::new(&m);
        let mut stats = crate::solve::MipStats::default();
        let mut lp_stats = crate::lp::LpStats::default();
        let mut iters = 0;
        let pair = select_children(
            &node,
            &lp.col_value,
            node.bound,
            lp.basis,
            &[0, 1, 2],
            &mut costs,
            &mut lpr,
            &mut propagation,
            &Options::default(),
            &lp_opts,
            &clock,
            2.,
            Some(-13.),
            &mut iters,
            &mut lp_stats,
            &mut stats,
        )
        .unwrap();
        assert!(stats.strong_branch_probes > 0);
        for child in pair {
            if let Some(cached) = child.cached {
                assert_eq!(cached.iterations, 0);
                assert_eq!(cached.stats.solves, 0);
                if cached.status == LpStatus::Optimal {
                    assert!(child.propagated);
                    assert!(child.branch.is_none());
                    let mut relaxed = m.clone();
                    relaxed.col_type.fill(VarType::Continuous);
                    assert!(check_solution(&relaxed, &cached.col_value, 1e-6).feasible);
                    for j in 0..m.num_col {
                        assert!(
                            cached.col_value[j] >= child.lo[j] - 1e-6
                                && cached.col_value[j] <= child.hi[j] + 1e-6
                        );
                    }
                }
            }
        }
    }

    #[cfg(feature = "parallel")]
    #[test]
    fn parallel_probes_and_propagation_preserve_optimum_and_interrupt() {
        let m = knapsack();
        let clock = StdClock::new();
        let opts = Options {
            threads: 2,
            mip_rel_gap: 0.,
            time_limit_secs: 2.,
            ..Default::default()
        };
        let s = branch_and_bound(&m, &opts, &clock, &mut Callbacks::default(), None, None);
        assert_eq!(s.status, ModelStatus::Optimal);
        assert_eq!(s.objective, -13.);
        assert!(check_solution(&m, &s.col_value, 1e-9).feasible);
        let interrupt = || true;
        let mut cb = Callbacks {
            interrupt: Some(&interrupt),
            ..Default::default()
        };
        let s = branch_and_bound(&m, &opts, &clock, &mut cb, None, None);
        // Workers may finish this tiny proof before the caller polls; either
        // certified completion or an acknowledged interrupt is valid.
        assert!(matches!(
            s.status,
            ModelStatus::Interrupt | ModelStatus::Optimal
        ));
    }
}
