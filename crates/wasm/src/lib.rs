//! WebAssembly bindings. Compiled only for `wasm32`; the native workspace build
//! sees an empty crate.
//!
//! The surface deliberately mirrors the `highs-js` extended API used by the
//! planning code (`createModel`, `addVar`, `addRow`, `changeColCost`,
//! `changeColIntegrality`, `passColName`/`passRowName`, `setSolution`,
//! `run`/`run({callbacks})`, `getModelStatus`, `getObjectiveValue`,
//! `getSolution().colValue`, `info.get`, `getIis`, `exportModel('lp')`,
//! `dispose`), so the app can swap implementations and the differential oracle
//! can run both.
//!
//! Naming note: `wasm-bindgen` exposes the Rust identifiers verbatim, so the JS
//! surface is `snake_case` (`add_var`, `pass_col_name`). The thin TypeScript
//! shim in `crates/wasm/ts/highs-shim.ts` maps the app's `highs` import
//! (`addVar`, `passColName`, ...) onto these.
#![cfg(target_arch = "wasm32")]

use core::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

// Optional smaller allocator. The standard allocator is the default because
// queued proof nodes fragment this linked free list and make cleanup costly.
// Left out on non-wasm targets and threaded wasm builds.
#[cfg(all(
    target_arch = "wasm32",
    not(target_feature = "atomics"),
    not(feature = "default-alloc")
))]
#[global_allocator]
static ALLOCATOR: lol_alloc::AssumeSingleThreaded<lol_alloc::FreeListAllocator> =
    unsafe { lol_alloc::AssumeSingleThreaded::new(lol_alloc::FreeListAllocator::new()) };

use optio_core::model::{check_solution, Model as CoreModel, RowId, VarId, VarType, INF};
use optio_core::solve::{solve_with_start, Callbacks, Clock, Incumbent, Progress, Solution};
use optio_core::{ModelStatus, Options};

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

/// `std::time` is unusable on `wasm32-unknown-unknown`; time comes from JS.
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn monotonic_now() -> f64;
}
struct WasmClock;

impl Clock for WasmClock {
    fn now_secs(&self) -> f64 {
        monotonic_now() / 1000.0
    }
}

// ---------------------------------------------------------------------------
// Small JS helpers
// ---------------------------------------------------------------------------

fn set_prop(obj: &js_sys::Object, key: &str, value: &JsValue) {
    // `Reflect::set` only fails for exotic (non-extensible) targets; our fresh
    // objects always accept the write.
    let _ = js_sys::Reflect::set(obj, &JsValue::from_str(key), value);
}

fn set_f64(obj: &js_sys::Object, key: &str, value: f64) {
    set_prop(obj, key, &JsValue::from_f64(value));
}

fn set_bool(obj: &js_sys::Object, key: &str, value: bool) {
    set_prop(obj, key, &JsValue::from_bool(value));
}

/// Read `obj[name]` if it is a JS function.
fn get_fn(obj: &JsValue, name: &str) -> Option<js_sys::Function> {
    let value = js_sys::Reflect::get(obj, &JsValue::from_str(name)).ok()?;
    value.dyn_into::<js_sys::Function>().ok()
}

fn progress_to_js(p: &Progress) -> JsValue {
    let o = js_sys::Object::new();
    set_f64(&o, "runningTime", p.running_time);
    set_f64(&o, "nodes", p.nodes as f64);
    set_f64(&o, "lpIterations", p.lp_iterations as f64);
    set_f64(&o, "gap", p.gap);
    set_f64(&o, "primalBound", p.primal_bound);
    set_f64(&o, "dualBound", p.dual_bound);
    set_bool(&o, "hasIncumbent", p.has_incumbent);
    o.into()
}

fn incumbent_to_js(inc: &Incumbent) -> JsValue {
    let o = js_sys::Object::new();
    set_f64(&o, "objective", inc.objective);
    let values = js_sys::Float64Array::from(inc.col_value.as_slice());
    set_prop(&o, "colValue", &values);
    o.into()
}

// ---------------------------------------------------------------------------
// Model export helpers
// ---------------------------------------------------------------------------

/// Bounds at/over `INF` are treated as infinite, matching `optio_core::INF`.
fn is_plus_inf(v: f64) -> bool {
    v >= INF
}

fn is_minus_inf(v: f64) -> bool {
    v <= -INF
}

/// Format a number compactly for LP/MPS output (integers without `.0`).
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Make a name acceptable to the CPLEX LP / MPS readers and unique within
/// `used`, falling back to `<prefix><index>` when the source name is empty.
fn unique_name(
    raw: Option<&String>,
    index: usize,
    fallback_prefix: &str,
    used: &mut Vec<String>,
) -> String {
    let mut base: String = match raw {
        Some(s) if !s.trim().is_empty() => s
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$' {
                    c
                } else {
                    '_'
                }
            })
            .collect(),
        _ => format!("{fallback_prefix}{index}"),
    };
    if base.is_empty() {
        base = format!("{fallback_prefix}{index}");
    }
    // LP names must not start with a digit or '.', and must not look like
    // scientific notation.
    if base
        .chars()
        .next()
        .map_or(false, |c| c.is_ascii_digit() || c == '.')
    {
        base.insert(0, '_');
    }
    if base.starts_with('e') && base.chars().nth(1).map_or(false, |c| c.is_ascii_digit()) {
        base.insert(0, '_');
    }
    let mut candidate = base.clone();
    let mut k = 1usize;
    while used.iter().any(|u| u == &candidate) {
        candidate = format!("{base}_{k}");
        k += 1;
    }
    used.push(candidate.clone());
    candidate
}

/// Build `"<coef> <name>"` terms joined with signs, e.g. `+ 2 x0 - x1`.
fn fmt_terms(terms: &[(String, f64)]) -> String {
    let mut out = String::new();
    let mut first = true;
    for (name, coef) in terms {
        if *coef == 0.0 {
            continue;
        }
        let neg = *coef < 0.0;
        let mag = coef.abs();
        if first {
            out.push(if neg { '-' } else { '+' });
            first = false;
        } else {
            out.push_str(if neg { " - " } else { " + " });
        }
        if (mag - 1.0).abs() > 1e-12 {
            out.push_str(&fmt_num(mag));
            out.push(' ');
        }
        out.push_str(name);
    }
    if first {
        out.push('0');
    }
    out
}

/// One emitted row. A ranged source row (finite lower and upper) is split into
/// a `>=` and a `<=` pair, as neither LP nor MPS has a native ranged form.
struct OutRow {
    label: String,
    /// 'L' (<=), 'G' (>=) or 'E' (=).
    sense: char,
    rhs: f64,
}

/// Build the emitted row set and, per source row, the map to emitted rows.
fn build_rows(model: &CoreModel, row_names: &[String]) -> (Vec<OutRow>, Vec<Vec<usize>>) {
    let mut out_rows = Vec::new();
    let mut mapping = vec![Vec::new(); model.num_row];
    for r in 0..model.num_row {
        let lo = model.row_lower[r];
        let hi = model.row_upper[r];
        let base = row_names[r].clone();
        let mut push = |label: String, sense: char, rhs: f64| {
            let idx = out_rows.len();
            out_rows.push(OutRow { label, sense, rhs });
            mapping[r].push(idx);
        };
        if is_minus_inf(lo) && is_plus_inf(hi) {
            // Free row: contributes nothing; skip (documented in export docs).
        } else if lo == hi {
            push(base, 'E', lo);
        } else if is_minus_inf(lo) {
            push(base, 'L', hi);
        } else if is_plus_inf(hi) {
            push(base, 'G', lo);
        } else {
            // Ranged row: emit both sides.
            push(format!("{base}_hi"), 'L', hi);
            push(format!("{base}_lo"), 'G', lo);
        }
    }
    (out_rows, mapping)
}

// ---------------------------------------------------------------------------
// wasm-bindgen class
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[wasm_bindgen]
pub struct Model {
    inner: CoreModel,
    options: Options,

    // User-supplied names; `None` falls back to `x{n}` / `r{n}`.
    col_names: Vec<Option<String>>,
    row_names: Vec<Option<String>>,

    // Last solve result / derived info exposed through `get_*` and `get_info`.
    col_value: Vec<f64>,
    status: i32,
    objective: f64,
    nodes: u64,
    lp_iterations: u64,
    mip_gap: f64,
    mip_dual_bound: f64,
    primal_solution_status: i32,
    dual_solution_status: i32,
    max_integrality_violation: f64,

    // Warm start from `set_solution`. Stored; see `set_solution` for why it is
    // not yet forwarded to the solver.
    warm_start: Option<Vec<f64>>,

    // Set when a `mipInterrupt` JS callback asked the solve to stop.
    interrupted: bool,

    // Last-solve phase timings (seconds), exposed via `get_info("time_*")`.
    heuristic_secs: f64,
    local_search_secs: f64,
    bb_secs: f64,
    presolve_secs: f64,
    cut_secs: f64,
    bound_secs: f64,
    root_cuts: usize,
    inferred_integer_cols: usize,
    lp_stats: optio_core::lp::LpStats,
    mip_stats: optio_core::solve::MipStats,
}

#[wasm_bindgen]
impl Model {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Model {
        Model {
            inner: CoreModel::new(),
            options: Options::default(),
            col_names: Vec::new(),
            row_names: Vec::new(),
            col_value: Vec::new(),
            status: ModelStatus::NotSet.as_i32(),
            objective: 0.0,
            nodes: 0,
            lp_iterations: 0,
            mip_gap: 0.0,
            mip_dual_bound: f64::NEG_INFINITY,
            primal_solution_status: 0,
            dual_solution_status: 0,
            max_integrality_violation: 0.0,
            warm_start: None,
            interrupted: false,
            heuristic_secs: 0.0,
            local_search_secs: 0.0,
            bb_secs: 0.0,
            presolve_secs: 0.0,
            cut_secs: 0.0,
            bound_secs: 0.0,
            root_cuts: 0,
            inferred_integer_cols: 0,
            lp_stats: Default::default(),
            mip_stats: Default::default(),
        }
    }

    // -- model construction -------------------------------------------------

    /// Append a column; returns its index.
    pub fn add_var(&mut self, lower: f64, upper: f64) -> u32 {
        let id = self.inner.add_col(lower, upper);
        self.col_names.push(None);
        id.0
    }

    /// Bulk-load a whole model in one call from flat CSR arrays (the wasm fast
    /// path — avoids one JS↔wasm call per var/row for large models). Columns are
    /// `0..num_cols`; `starts` has `row_lower.len() + 1` entries and
    /// `indices`/`values` are the CSR payload.
    #[allow(clippy::too_many_arguments)]
    pub fn add_model(
        &mut self,
        num_cols: u32,
        col_cost: &[f64],
        col_lower: &[f64],
        col_upper: &[f64],
        integrality: &[i32],
        row_lower: &[f64],
        row_upper: &[f64],
        starts: &[u32],
        indices: &[u32],
        values: &[f64],
    ) {
        self.inner.add_csr(
            num_cols as usize,
            col_cost,
            col_lower,
            col_upper,
            integrality,
            row_lower,
            row_upper,
            starts,
            indices,
            values,
        );
        self.col_names.resize(self.inner.num_col, None);
        self.row_names.resize(self.inner.num_row, None);
    }

    /// Append a row from parallel index/value arrays.
    pub fn add_row(&mut self, lower: f64, upper: f64, indices: &[i32], values: &[f64]) {
        debug_assert_eq!(indices.len(), values.len());
        let entries: Vec<(u32, f64)> = indices
            .iter()
            .zip(values)
            .map(|(&i, &v)| (i as u32, v))
            .collect();
        self.inner.add_row(lower, upper, &entries);
        self.row_names.push(None);
    }

    pub fn change_col_cost(&mut self, col: u32, cost: f64) {
        self.inner.set_cost(VarId(col), cost);
    }

    /// `ty`: `0` continuous, anything nonzero integer. Binary is an integer
    /// with `[0, 1]` bounds.
    pub fn change_col_integrality(&mut self, col: u32, ty: i32) {
        let ty = if ty == 0 {
            VarType::Continuous
        } else {
            VarType::Integer
        };
        self.inner.set_integrality(VarId(col), ty);
    }

    pub fn change_col_lower(&mut self, col: u32, lower: f64) {
        self.inner.col_lower[col as usize] = lower;
    }

    pub fn change_col_upper(&mut self, col: u32, upper: f64) {
        self.inner.col_upper[col as usize] = upper;
    }

    pub fn change_col_bounds(&mut self, col: u32, lower: f64, upper: f64) {
        self.inner.col_lower[col as usize] = lower;
        self.inner.col_upper[col as usize] = upper;
    }

    pub fn change_row_bounds(&mut self, row: u32, lower: f64, upper: f64) {
        self.inner.row_lower[row as usize] = lower;
        self.inner.row_upper[row as usize] = upper;
    }

    /// Store a column name. Also accepted before the column exists, in which
    /// case it is buffered.
    pub fn pass_col_name(&mut self, col: u32, name: String) {
        let idx = col as usize;
        if self.col_names.len() <= idx {
            self.col_names.resize(idx + 1, None);
        }
        self.col_names[idx] = Some(name);
    }

    /// Store a row name.
    pub fn pass_row_name(&mut self, row: u32, name: String) {
        let idx = row as usize;
        if self.row_names.len() <= idx {
            self.row_names.resize(idx + 1, None);
        }
        self.row_names[idx] = Some(name);
    }

    pub fn col_name(&self, col: u32) -> String {
        self.col_names
            .get(col as usize)
            .and_then(|n| n.clone())
            .unwrap_or_else(|| format!("x{col}"))
    }

    pub fn row_name(&self, row: u32) -> String {
        self.row_names
            .get(row as usize)
            .and_then(|n| n.clone())
            .unwrap_or_else(|| format!("r{row}"))
    }

    // -- warm start ---------------------------------------------------------

    /// Store a warm-start column vector.
    ///
    /// The core validates and polishes this point before using it as an incumbent.
    pub fn set_solution(&mut self, col_value: &[f64]) {
        self.warm_start = Some(col_value.to_vec());
    }

    /// The stored warm start, or an empty vector.
    pub fn get_warm_start(&self) -> Vec<f64> {
        self.warm_start.clone().unwrap_or_default()
    }

    pub fn has_warm_start(&self) -> bool {
        self.warm_start.is_some()
    }

    // -- options ------------------------------------------------------------

    /// Generic option setter. Unknown names are ignored.
    pub fn set_option(&mut self, name: String, value: f64) {
        match name.as_str() {
            "output_flag" => self.options.output_flag = value != 0.0,
            "time_limit" | "time_limit_secs" => self.options.time_limit_secs = value,
            "mip_rel_gap" => self.options.mip_rel_gap = value,
            "mip_abs_gap" => self.options.mip_abs_gap = value,
            "threads" => self.options.threads = value.max(0.0) as usize,
            "random_seed" | "seed" => self.options.seed = value.max(0.0) as u64,
            "presolve" => self.options.presolve = value != 0.0,
            "lp_warm_start" => self.options.lp_warm_start = value != 0.0,
            "mip_cuts" => self.options.mip_cuts = value != 0.0,
            "mip_pseudocost" => self.options.mip_pseudocost = value != 0.0,
            "mip_node_presolve" => self.options.mip_node_presolve = value != 0.0,
            "mip_column_compression" => self.options.mip_column_compression = value != 0.0,
            "mip_objective_counts" => self.options.mip_objective_counts = value != 0.0,
            "mip_objective_lattice" => self.options.mip_objective_lattice = value != 0.0,
            "mip_strong_branch" => self.options.mip_strong_branch = value != 0.0,
            "mip_max_nodes" if value.is_finite() && value >= 0.0 => {
                self.options.mip_max_nodes = value as u64
            }
            "adaptive_search" => self.options.adaptive_search = value != 0.0,
            "balance_target_ratio" => self.options.balance_target_ratio = value,
            _ => {}
        }
    }

    pub fn set_time_limit(&mut self, secs: f64) {
        self.options.time_limit_secs = secs;
    }

    pub fn set_mip_rel_gap(&mut self, gap: f64) {
        self.options.mip_rel_gap = gap;
    }

    pub fn set_output_flag(&mut self, on: bool) {
        self.options.output_flag = on;
    }

    // -- solve --------------------------------------------------------------

    pub fn run(&mut self) {
        let mut callbacks = Callbacks::default();
        let solution = solve_with_start(
            &self.inner,
            &self.options,
            &WasmClock,
            &mut callbacks,
            self.warm_start.as_deref(),
        );
        self.store_solution(solution, f64::INFINITY);
    }

    /// Solve with JS callbacks. `callbacks` is an object with any of:
    ///
    /// * `mipLogging(progress)` - progress snapshot (also used as a log hook);
    /// * `mipImprovingSolution(incumbent)` - new incumbent `{objective, colValue}`;
    /// * `mipInterrupt(progress)` - return a truthy value to request an
    ///   interrupt. The request is captured in a flag and stored on the model
    ///   (`was_interrupted`). NOTE: the core solve loop does not yet consult it;
    ///   the branch & bound workstream must honor the interrupt to stop early.
    ///
    /// All three receive fresh JS objects; their return values other than
    /// `mipInterrupt` are ignored.
    pub fn run_with_callbacks(&mut self, callbacks: JsValue) {
        let log_fn = get_fn(&callbacks, "mipLogging");
        let improving_fn = get_fn(&callbacks, "mipImprovingSolution");
        let interrupt_fn = get_fn(&callbacks, "mipInterrupt");

        // `Cell`/atomics let us observe values written inside the `FnMut`
        // closures after the solve returns without moving the closures.
        let last_gap = Cell::new(f64::INFINITY);
        let interrupt = AtomicBool::new(false);

        let mut on_progress = |p: &Progress| {
            if p.gap.is_finite() {
                last_gap.set(p.gap);
            }
            let js = progress_to_js(p);
            if let Some(f) = &log_fn {
                let _ = f.call1(&JsValue::NULL, &js);
            }
            if let Some(f) = &interrupt_fn {
                if let Ok(ret) = f.call1(&JsValue::NULL, &js) {
                    if ret.is_truthy() {
                        interrupt.store(true, Ordering::Relaxed);
                    }
                }
            }
        };
        let mut on_incumbent = |inc: &Incumbent| {
            if let Some(f) = &improving_fn {
                let js = incumbent_to_js(inc);
                let _ = f.call1(&JsValue::NULL, &js);
            }
        };

        let mut cbs = Callbacks::default();
        if log_fn.is_some() || interrupt_fn.is_some() {
            cbs.on_progress = Some(&mut on_progress);
        }
        if improving_fn.is_some() {
            cbs.on_incumbent = Some(&mut on_incumbent);
        }
        // Let the core observe the interrupt flag set by `mipInterrupt`.
        let interrupt_probe = || interrupt.load(Ordering::Relaxed);
        cbs.interrupt = Some(&interrupt_probe);

        let solution = solve_with_start(
            &self.inner,
            &self.options,
            &WasmClock,
            &mut cbs,
            self.warm_start.as_deref(),
        );
        let gap = last_gap.get();
        drop(cbs);
        self.store_solution(solution, gap);
        self.interrupted = interrupt.load(Ordering::Relaxed);
    }

    fn store_solution(&mut self, solution: Solution, gap_hint: f64) {
        self.mip_dual_bound = solution.dual_bound.unwrap_or(f64::NEG_INFINITY);
        self.status = solution.status.as_i32();
        self.objective = solution.objective;
        self.nodes = solution.nodes;
        self.lp_iterations = solution.lp_iterations;
        self.heuristic_secs = solution.heuristic_secs;
        self.local_search_secs = solution.local_search_secs;
        self.bb_secs = solution.bb_secs;
        self.presolve_secs = solution.presolve_secs;
        self.cut_secs = solution.cut_secs;
        self.bound_secs = solution.bound_secs;
        self.root_cuts = solution.root_cuts;
        self.inferred_integer_cols = solution.inferred_integer_cols;
        self.lp_stats = solution.lp_stats;
        self.mip_stats = solution.mip_stats;
        self.col_value = solution.col_value;

        if self.col_value.len() == self.inner.num_col && !self.col_value.is_empty() {
            let check = check_solution(&self.inner, &self.col_value, 1e-6);
            self.max_integrality_violation = check.max_integrality_violation;
            self.primal_solution_status = if check.feasible { 2 } else { 1 };
        } else {
            self.max_integrality_violation = 0.0;
            self.primal_solution_status = 0;
        }
        self.dual_solution_status = 0;

        // Gap: the core does not report one yet, so derive what we can.
        self.mip_gap = if solution.status == ModelStatus::Optimal {
            0.0
        } else if solution.dual_bound.is_some() && self.primal_solution_status == 2 {
            ((self.objective - self.mip_dual_bound) / self.objective.abs().max(1e-10)).max(0.0)
        } else if gap_hint.is_finite() {
            gap_hint
        } else {
            f64::INFINITY
        };
    }

    /// True if a `mipInterrupt` callback requested a stop during the last solve.
    pub fn was_interrupted(&self) -> bool {
        self.interrupted
    }

    // -- results ------------------------------------------------------------

    pub fn get_model_status(&self) -> i32 {
        self.status
    }

    pub fn get_objective_value(&self) -> f64 {
        self.objective
    }

    /// Column values. Phase 3 replaces this with a zero-copy `Float64Array`
    /// view over the wasm heap; `wasm-bindgen` already materializes this as a
    /// `Float64Array` on the JS side.
    pub fn get_solution(&self) -> Vec<f64> {
        self.col_value.clone()
    }

    /// Zero-copy `Float64Array` view of the last solution, aliasing wasm linear
    /// memory. Assumes wasm-bindgen's memory (no `externref` heap). The view is
    /// invalidated by any later call that grows/reallocates memory — i.e. the
    /// next `run`, `add_var`/`add_row`/`add_model`, or another solve. Copy it
    /// (`Float64Array.prototype.slice`) if you need to retain it across calls.
    pub fn get_solution_view(&self) -> js_sys::Float64Array {
        unsafe { js_sys::Float64Array::view(self.col_value.as_slice()) }
    }

    /// Solution object: `{ colValue: Float64Array }`, the `Float64Array`
    /// being a zero-copy view (see `get_solution_view`).
    pub fn get_solution_object(&self) -> JsValue {
        let o = js_sys::Object::new();
        let values = unsafe { js_sys::Float64Array::view(self.col_value.as_slice()) };
        set_prop(&o, "colValue", &values);
        o.into()
    }

    /// Subset of the supported info keys (objective, bounds, gaps, counters,
    /// timings — see the README table). Unknown keys return `0.0`.
    pub fn get_info(&self, name: String) -> f64 {
        match name.as_str() {
            "objective_function_value" => self.objective,
            "mip_gap" => self.mip_gap,
            "mip_dual_bound" => self.mip_dual_bound,
            "mip_node_count" => self.nodes as f64,
            "simplex_iteration_count" => self.lp_iterations as f64,
            "primal_solution_status" => self.primal_solution_status as f64,
            "dual_solution_status" => self.dual_solution_status as f64,
            "max_integrality_violation" => self.max_integrality_violation,
            "num_col" => self.inner.num_col as f64,
            "num_row" => self.inner.num_row as f64,
            "time_bound_certificate_secs" => self.bound_secs,
            "time_root_cuts_secs" => self.cut_secs,
            "mip_inferred_integer_cols" => self.inferred_integer_cols as f64,
            "mip_root_cuts" => self.root_cuts as f64,
            "time_presolve_secs" => self.presolve_secs,
            "time_node_lp_worker_secs" => self.lp_stats.seconds,
            "node_lp_solves" => self.lp_stats.solves as f64,
            "node_lp_phase1_iterations" => self.lp_stats.phase1_iterations as f64,
            "node_lp_phase2_iterations" => self.lp_stats.phase2_iterations as f64,
            "node_lp_warm_solves" => self.lp_stats.warm_solves as f64,
            "node_lp_warm_fallbacks" => self.lp_stats.warm_fallbacks as f64,
            "node_lp_warm_invalid_basis" => self.lp_stats.warm_invalid_basis as f64,
            "node_lp_warm_singular_basis" => self.lp_stats.warm_singular_basis as f64,
            "node_lp_warm_residual_failures" => self.lp_stats.warm_residual_failures as f64,
            "node_lp_warm_dual_failures" => self.lp_stats.warm_dual_failures as f64,
            "node_lp_warm_no_pivot" => self.lp_stats.warm_no_pivot as f64,
            "node_lp_warm_pivot_failures" => self.lp_stats.warm_pivot_failures as f64,
            "node_lp_warm_iteration_limits" => self.lp_stats.warm_iteration_limits as f64,
            "node_lp_warm_bound_failures" => self.lp_stats.warm_bound_failures as f64,
            "node_lp_warm_bound_flips" => self.lp_stats.warm_bound_flips as f64,
            "node_propagation_prunes" => self.mip_stats.propagation_prunes as f64,
            "objective_count_columns" => self.mip_stats.objective_count_columns as f64,
            "compressed_columns" => self.mip_stats.compressed_columns as f64,
            "time_node_propagation_secs" => self.mip_stats.propagation_seconds,
            "strong_branch_probes" => self.mip_stats.strong_branch_probes as f64,
            "numerical_retries" => self.mip_stats.numerical_retries as f64,
            "time_heuristic_secs" => self.heuristic_secs,
            "time_local_search_secs" => self.local_search_secs,
            "time_branch_and_bound_secs" => self.bb_secs,
            _ => 0.0,
        }
    }

    /// Infeasibility certificate. Returns `{ col_index, row_index }` as
    /// `Int32Array`s. The core does not compute an IIS yet, so these are always
    /// empty; the call is non-throwing so the app's infeasible branch is safe.
    /// TODO(solver): populate the actual irreducible infeasible subsystem.
    pub fn get_iis(&self) -> JsValue {
        let o = js_sys::Object::new();
        let cols = js_sys::Int32Array::new_with_length(0);
        let rows = js_sys::Int32Array::new_with_length(0);
        set_prop(&o, "col_index", &cols);
        set_prop(&o, "row_index", &rows);
        o.into()
    }

    // -- export -------------------------------------------------------------

    /// Export the model. `"lp"` writes CPLEX LP format; `"mps"` writes free
    /// MPS. Any other value returns an empty string.
    pub fn export_model(&self, what: String) -> String {
        match what.as_str() {
            "lp" => self.export_lp(),
            "mps" => self.export_mps(),
            _ => String::new(),
        }
    }

    fn column_labels(&self) -> Vec<String> {
        let mut used = Vec::with_capacity(self.inner.num_col);
        (0..self.inner.num_col)
            .map(|c| {
                unique_name(
                    self.col_names.get(c).and_then(|n| n.as_ref()),
                    c,
                    "x",
                    &mut used,
                )
            })
            .collect()
    }

    fn row_labels(&self) -> Vec<String> {
        let mut used = Vec::with_capacity(self.inner.num_row);
        (0..self.inner.num_row)
            .map(|r| {
                unique_name(
                    self.row_names.get(r).and_then(|n| n.as_ref()),
                    r,
                    "r",
                    &mut used,
                )
            })
            .collect()
    }

    fn export_lp(&self) -> String {
        let m = &self.inner;
        let cols = self.column_labels();
        let row_labels = self.row_labels();
        let (out_rows, mapping) = build_rows(m, &row_labels);

        let mut out = String::new();
        out.push_str("\\* optio LP export *\\\n");

        // Objective.
        out.push_str("Minimize\n obj:");
        let obj_terms: Vec<(String, f64)> = (0..m.num_col)
            .filter(|&c| m.col_cost[c] != 0.0)
            .map(|c| (cols[c].clone(), m.col_cost[c]))
            .collect();
        out.push(' ');
        out.push_str(&fmt_terms(&obj_terms));
        out.push('\n');

        // Constraints. Each source row contributes its coefficients once; a
        // ranged source row emits the same LHS twice with different senses.
        out.push_str("Subject To\n");
        for r in 0..m.num_row {
            let terms: Vec<(String, f64)> = m
                .row(RowId(r as u32))
                .map(|(c, v)| (cols[c as usize].clone(), v))
                .collect();
            for &ori in &mapping[r] {
                let row = &out_rows[ori];
                let sense = match row.sense {
                    'L' => "<=",
                    'G' => ">=",
                    _ => "=",
                };
                out.push_str(&format!(
                    " {}: {} {} {}\n",
                    row.label,
                    fmt_terms(&terms),
                    sense,
                    fmt_num(row.rhs)
                ));
            }
        }

        // Bounds (LP default is 0 <= x).
        out.push_str("Bounds\n");
        for c in 0..m.num_col {
            let lo = m.col_lower[c];
            let hi = m.col_upper[c];
            let name = &cols[c];
            if is_minus_inf(lo) && is_plus_inf(hi) {
                out.push_str(&format!(" {name} free\n"));
            } else if is_minus_inf(lo) {
                out.push_str(&format!(" {name} <= {}\n", fmt_num(hi)));
            } else if is_plus_inf(hi) {
                if lo != 0.0 {
                    out.push_str(&format!(" {name} >= {}\n", fmt_num(lo)));
                }
            } else {
                out.push_str(&format!(" {} <= {name} <= {}\n", fmt_num(lo), fmt_num(hi)));
            }
        }

        // Integrality. Integer vars with [0,1] bounds export as binary.
        let mut generals = Vec::new();
        let mut binaries = Vec::new();
        for c in 0..m.num_col {
            let lo = m.col_lower[c];
            let hi = m.col_upper[c];
            match m.col_type[c] {
                VarType::Binary => binaries.push(cols[c].clone()),
                VarType::Integer if lo >= -1e-9 && hi <= 1.0 + 1e-9 => {
                    binaries.push(cols[c].clone())
                }
                VarType::Integer => generals.push(cols[c].clone()),
                VarType::Continuous => {}
            }
        }
        if !generals.is_empty() {
            out.push_str("Generals\n");
            for name in &generals {
                out.push_str(&format!(" {name}\n"));
            }
        }
        if !binaries.is_empty() {
            out.push_str("Binaries\n");
            for name in &binaries {
                out.push_str(&format!(" {name}\n"));
            }
        }
        out.push_str("End\n");
        out
    }

    fn export_mps(&self) -> String {
        let m = &self.inner;
        let cols = self.column_labels();
        let row_labels = self.row_labels();
        let (out_rows, mapping) = build_rows(m, &row_labels);

        let mut out = String::new();
        out.push_str("NAME          optio\nROWS\n");
        out.push_str(" N  COST\n");
        for row in &out_rows {
            out.push_str(&format!(" {}  {}\n", row.sense, row.label));
        }

        // Column-major entries: objective plus each mapped row coefficient.
        let mut col_entries: Vec<Vec<(usize, f64)>> = vec![Vec::new(); m.num_col];
        for r in 0..m.num_row {
            for (c, v) in m.row(RowId(r as u32)) {
                for &ori in &mapping[r] {
                    col_entries[c as usize].push((ori, v));
                }
            }
        }

        out.push_str("COLUMNS\n");
        let mut in_int = false;
        for c in 0..m.num_col {
            let is_int = m.col_type[c].is_integral();
            if is_int && !in_int {
                out.push_str("    MARK0000  'MARKER'                 'INTORG'\n");
                in_int = true;
            } else if !is_int && in_int {
                out.push_str("    MARK0001  'MARKER'                 'INTEND'\n");
                in_int = false;
            }
            let cost = m.col_cost[c];
            if cost != 0.0 {
                out.push_str(&format!("    {}  COST  {}\n", cols[c], fmt_num(cost)));
            }
            for &(ri, v) in &col_entries[c] {
                if v != 0.0 {
                    out.push_str(&format!(
                        "    {}  {}  {}\n",
                        cols[c],
                        out_rows[ri].label,
                        fmt_num(v)
                    ));
                }
            }
        }
        if in_int {
            out.push_str("    MARK0001  'MARKER'                 'INTEND'\n");
        }

        out.push_str("RHS\n");
        for row in &out_rows {
            if row.rhs != 0.0 {
                out.push_str(&format!("    RHS  {}  {}\n", row.label, fmt_num(row.rhs)));
            }
        }

        out.push_str("BOUNDS\n");
        for c in 0..m.num_col {
            let lo = m.col_lower[c];
            let hi = m.col_upper[c];
            let name = &cols[c];
            let binary = matches!(m.col_type[c], VarType::Binary)
                || (m.col_type[c].is_integral() && lo >= -1e-9 && hi <= 1.0 + 1e-9);
            if binary {
                out.push_str(&format!(" BV BND  {name}\n"));
            } else if is_minus_inf(lo) && is_plus_inf(hi) {
                out.push_str(&format!(" FR BND  {name}\n"));
            } else if is_minus_inf(lo) {
                out.push_str(&format!(" MI BND  {name}\n"));
                out.push_str(&format!(" UP BND  {name}  {}\n", fmt_num(hi)));
            } else if is_plus_inf(hi) {
                if lo != 0.0 {
                    out.push_str(&format!(" LO BND  {name}  {}\n", fmt_num(lo)));
                }
            } else if lo == hi {
                out.push_str(&format!(" FX BND  {name}  {}\n", fmt_num(lo)));
            } else {
                if lo != 0.0 {
                    out.push_str(&format!(" LO BND  {name}  {}\n", fmt_num(lo)));
                }
                out.push_str(&format!(" UP BND  {name}  {}\n", fmt_num(hi)));
            }
        }
        out.push_str("ENDATA\n");
        out
    }

    // -- sizes / lifecycle --------------------------------------------------

    pub fn num_col(&self) -> usize {
        self.inner.num_col
    }

    pub fn num_row(&self) -> usize {
        self.inner.num_row
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}
