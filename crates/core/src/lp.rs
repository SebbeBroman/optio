//! LP relaxation: a bounded-variable, two-phase **revised primal simplex**.
//!
//! This is the unblocking piece from `PLAN.md` (Phase 1): no dual bounds and no
//! branch & bound are possible without it. It is intentionally small and
//! dependency-free (wasm-first), and is verified differentially against HiGHS by
//! the harness rather than trusted through its own status.
//!
//! Form solved (the model's own bounds, integrality ignored):
//!
//! ```text
//!   minimise   c'x
//!   subject to  L_r <= a_r' x <= U_r      (rows)
//!               l_j <=    x_j   <= u_j    (columns)
//! ```
//!
//! Internals: each row gets a *logical* variable `s_r` with `a_r'x - s_r = 0`
//! and bounds `[L_r, U_r]`. Rows whose origin is infeasible get an *artificial*
//! variable for Phase 1. The basis is `m x m`; `B^{-1}` is a sparse LU (partial
//! pivoting, dense-accumulator left-looking factorisation) refined by a product
//! of eta matrices and refactorised every `refactor_interval` pivots.

use crate::model::{Model, INF};
use crate::solve::Clock;

/// Outcome of an LP solve.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LpStatus {
    /// Optimal solution found.
    Optimal,
    /// The (relaxed) problem is primal infeasible.
    Infeasible,
    /// The objective is unbounded below.
    Unbounded,
    /// Hit the iteration cap without proving optimality.
    IterationLimit,
    /// Ran out of the caller's time budget.
    TimeLimit,
    /// Numerical breakdown (singular basis that could not be repaired).
    Numerical,
}

/// Tolerances and limits for the simplex.
#[derive(Clone, Debug)]
pub struct LpOptions {
    /// Primal feasibility tolerance.
    pub primal_tol: f64,
    /// Dual (reduced-cost) tolerance.
    pub dual_tol: f64,
    /// Maximum simplex iterations (per phase).
    pub max_iterations: u64,
    /// Refactorise the basis after this many eta updates.
    pub refactor_interval: usize,
    /// Wall-clock budget for the solve, in seconds.
    pub time_limit_secs: f64,
    /// Generate valid inequalities (root cut loop only).
    pub generate_cuts: bool,
}

impl Default for LpOptions {
    fn default() -> Self {
        LpOptions {
            primal_tol: 1e-7,
            dual_tol: 1e-7,
            max_iterations: 200_000,
            refactor_interval: 128,
            time_limit_secs: 3600.0,
            generate_cuts: false,
        }
    }
}

/// Aggregate LP work. Seconds sum across workers, rather than wall-clock time.
#[derive(Clone, Copy, Debug, Default)]
pub struct LpStats {
    pub solves: u64,
    pub seconds: f64,
    pub phase1_iterations: u64,
    pub phase2_iterations: u64,
    pub warm_solves: u64,
    pub warm_fallbacks: u64,
    pub warm_invalid_basis: u64,
    pub warm_singular_basis: u64,
    pub warm_residual_failures: u64,
    pub warm_dual_failures: u64,
    pub warm_no_pivot: u64,
    pub warm_pivot_failures: u64,
    pub warm_iteration_limits: u64,
    pub warm_bound_failures: u64,
    pub warm_bound_flips: u64,
}
impl std::ops::AddAssign for LpStats {
    fn add_assign(&mut self, rhs: Self) {
        self.solves += rhs.solves;
        self.seconds += rhs.seconds;
        self.phase1_iterations += rhs.phase1_iterations;
        self.phase2_iterations += rhs.phase2_iterations;
        self.warm_solves += rhs.warm_solves;
        self.warm_fallbacks += rhs.warm_fallbacks;
        self.warm_invalid_basis += rhs.warm_invalid_basis;
        self.warm_singular_basis += rhs.warm_singular_basis;
        self.warm_residual_failures += rhs.warm_residual_failures;
        self.warm_dual_failures += rhs.warm_dual_failures;
        self.warm_no_pivot += rhs.warm_no_pivot;
        self.warm_pivot_failures += rhs.warm_pivot_failures;
        self.warm_iteration_limits += rhs.warm_iteration_limits;
        self.warm_bound_failures += rhs.warm_bound_failures;
        self.warm_bound_flips += rhs.warm_bound_flips;
    }
}

/// A sparse valid inequality `lower <= Σ values[j]·x_j` in original column
/// space, generated from an optimal LP basis.
#[derive(Clone, Debug, Default)]
pub struct Cut {
    pub indices: Vec<u32>,
    pub values: Vec<f64>,
    pub lower: f64,
}

/// LP result in the original column space.
#[derive(Clone, Debug, Default)]
pub struct LpSolution {
    pub status: LpStatus,
    pub objective: f64,
    /// Lagrangian lower bound using true costs and all variable bounds.
    pub dual_bound: Option<f64>,
    pub col_value: Vec<f64>,
    pub col_dual: Vec<f64>,
    pub row_dual: Vec<f64>,
    pub iterations: u64,
    pub stats: LpStats,
    pub basis: Option<std::sync::Arc<LpBasis>>,
    /// Valid inequalities violated by this LP point (empty unless the solve
    /// requested cut generation).
    pub cuts: Vec<Cut>,
}

impl Default for LpStatus {
    fn default() -> Self {
        LpStatus::Numerical
    }
}

/// Snapshot in scaled LP coordinates; shared by siblings. A fresh numerical
/// factorization is built on restore, while simplex iterations reuse the basis.
#[derive(Clone, Debug)]
pub struct LpBasis {
    basis: Vec<usize>,
    x: Vec<f64>,
}

/// A reusable LP relaxation of a `Model` (structure is fixed; bounds change).
pub struct LpRelaxation {
    n: usize,
    m: usize,
    /// Column costs (structural, column-scaled, solver copy; may be perturbed).
    cost: Vec<f64>,
    /// Unperturbed column costs, for reporting the true objective.
    cost_true: Vec<f64>,
    /// Model column bounds (default).
    col_lo: Vec<f64>,
    col_hi: Vec<f64>,
    row_lo: Vec<f64>,
    row_hi: Vec<f64>,
    /// Sparse matrix, CSC by column.
    csc_start: Vec<u32>,
    csc_index: Vec<u32>,
    csc_value: Vec<f64>,
    /// Sparse matrix, CSR by row (same scaled values as the CSC). Used by the
    /// primal crash to pick a structural variable per row.
    csr_start: Vec<u32>,
    csr_index: Vec<u32>,
    csr_value: Vec<f64>,
    /// Column equilibration factors: solver variable `x'_j = x_j / col_scale[j]`.
    col_scale: Vec<f64>,
    /// Structural columns that are integer-constrained (true model integrality).
    is_int: Vec<bool>,
}

/// Detect equality rows implied by other equality rows (rank-deficient rows).
/// Returns `drop[r] = true` for redundant rows. Rows whose coefficients reduce
/// to zero but whose right-hand side is nonzero are kept (phase 1 flags the
/// resulting infeasibility).
fn redundant_equalities(model: &Model) -> Vec<bool> {
    let n = model.num_col;
    let m = model.num_row;
    let mut drop = vec![false; m];
    // `piv[c] = (normalized reduced row, reduced rhs)` keyed by its pivot column.
    let mut piv: Vec<Option<(Vec<(u32, f64)>, f64)>> = (0..n).map(|_| None).collect();
    let mut w = vec![0.0f64; n];
    let mut list: Vec<u32> = Vec::new();

    for r in 0..m {
        let lo = model.row_lower[r];
        let hi = model.row_upper[r];
        let scale = 1.0 + hi.abs().max(lo.abs());
        if (hi - lo).abs() > 1e-9 * scale {
            continue; // only equalities participate
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        if ks == ke {
            continue; // empty row: leave for phase 1
        }
        let mut maxc = 0.0f64;
        for k in ks..ke {
            maxc = maxc.max(model.a_value[k].abs());
        }
        if maxc == 0.0 {
            continue;
        }
        let inv = 1.0 / maxc;
        let mut rhs = lo * inv;
        list.clear();
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            if w[c] == 0.0 {
                list.push(c as u32);
            }
            w[c] += model.a_value[k] * inv;
        }
        let mut head = 0usize;
        while head < list.len() {
            let c = list[head] as usize;
            head += 1;
            let v = w[c];
            if v == 0.0 {
                continue;
            }
            if let Some((prow, prhs)) = &piv[c] {
                for &(cc, pv) in prow {
                    let cc = cc as usize;
                    if w[cc] == 0.0 {
                        list.push(cc as u32);
                    }
                    w[cc] -= v * pv;
                }
                rhs -= v * *prhs;
                w[c] = 0.0;
            }
        }
        let mut pivot_col = None;
        for &c in &list {
            if w[c as usize].abs() > 1e-10 {
                pivot_col = Some(c as usize);
                break;
            }
        }
        match pivot_col {
            None => {
                if rhs.abs() <= 1e-8 {
                    drop[r] = true;
                }
            }
            Some(pc) => {
                let pivval = w[pc];
                let mut prow: Vec<(u32, f64)> = Vec::new();
                for &c in &list {
                    let c = c as usize;
                    if w[c].abs() > 1e-10 {
                        prow.push((c as u32, w[c] / pivval));
                    }
                }
                piv[pc] = Some((prow, rhs / pivval));
            }
        }
        for &c in &list {
            w[c as usize] = 0.0;
        }
    }
    drop
}

impl LpRelaxation {
    /// Build the relaxation from a model (builds CSC once) and equilibrate it.
    pub fn build(model: &Model) -> LpRelaxation {
        let n = model.num_col;
        let m0 = model.num_row;

        // Drop equality rows that are implied by other equality rows. Such rows
        // make the constraint matrix rank-deficient, so every square basis is
        // singular and the simplex cannot stay consistent (this is the Sudoku /
        // planning failure mode). HiGHS does the same in presolve.
        let drop = redundant_equalities(model);
        let row_new: Vec<u32> = {
            let mut v = vec![u32::MAX; m0];
            let mut i = 0u32;
            for r in 0..m0 {
                if !drop[r] {
                    v[r] = i;
                    i += 1;
                }
            }
            v
        };
        let m = row_new.iter().filter(|&&x| x != u32::MAX).count();

        let mut start = vec![0u32; n + 1];
        for r in 0..m0 {
            if drop[r] {
                continue;
            }
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                start[model.a_index[k] as usize + 1] += 1;
            }
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let nnz = start[n] as usize;
        let mut index = vec![0u32; nnz];
        let mut value = vec![0.0f64; nnz];
        let mut cursor = start.clone();
        for r in 0..m0 {
            if drop[r] {
                continue;
            }
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                let c = model.a_index[k] as usize;
                let p = cursor[c] as usize;
                index[p] = row_new[r];
                value[p] = model.a_value[k];
                cursor[c] += 1;
            }
        }

        // --- Equilibration scaling (inf-norm, clamped). ---
        // Column scale: 1 / max_i |a_ij|.
        let mut col_scale = vec![1.0f64; n];
        for j in 0..n {
            let mut mx = 0.0f64;
            for k in start[j] as usize..start[j + 1] as usize {
                mx = mx.max(value[k].abs());
            }
            if mx > 1e-12 {
                col_scale[j] = (1.0 / mx).clamp(1e-3, 1e3);
            }
        }
        for j in 0..n {
            let s = col_scale[j];
            for k in start[j] as usize..start[j + 1] as usize {
                value[k] *= s;
            }
        }
        // Row scale: 1 / max_j |a_ij * s_j| (on the column-scaled values).
        let mut row_max = vec![0.0f64; m];
        for k in 0..nnz {
            let r = index[k] as usize;
            row_max[r] = row_max[r].max(value[k].abs());
        }
        let mut row_scale = vec![1.0f64; m];
        for r in 0..m {
            if row_max[r] > 1e-12 {
                row_scale[r] = (1.0 / row_max[r]).clamp(1e-3, 1e3);
            }
        }
        for k in 0..nnz {
            value[k] *= row_scale[index[k] as usize];
        }

        let col_lo0: Vec<f64> = model.col_lower.iter().map(|&v| to_lo(v)).collect();
        let col_hi0: Vec<f64> = model.col_upper.iter().map(|&v| to_hi(v)).collect();
        let mut row_lo = vec![0.0f64; m];
        let mut row_hi = vec![0.0f64; m];
        for r in 0..m0 {
            let i = row_new[r];
            if i == u32::MAX {
                continue;
            }
            row_lo[i as usize] = to_lo(model.row_lower[r]) * row_scale[i as usize];
            row_hi[i as usize] = to_hi(model.row_upper[r]) * row_scale[i as usize];
        }
        // Row-major copy (CSR) of the same scaled CSC, for the crash.
        let mut csr_start = vec![0u32; m + 1];
        for k in 0..nnz {
            csr_start[index[k] as usize + 1] += 1;
        }
        for r in 0..m {
            csr_start[r + 1] += csr_start[r];
        }
        let mut csr_index = vec![0u32; nnz];
        let mut csr_value = vec![0.0f64; nnz];
        let mut csr_cursor = csr_start.clone();
        for j in 0..n {
            for k in start[j] as usize..start[j + 1] as usize {
                let r = index[k] as usize;
                let p = csr_cursor[r] as usize;
                csr_index[p] = j as u32;
                csr_value[p] = value[k];
                csr_cursor[r] += 1;
            }
        }

        let cost: Vec<f64> = model
            .col_cost
            .iter()
            .enumerate()
            .map(|(j, &c)| c * col_scale[j])
            .collect();
        // A tiny deterministic objective perturbation breaks the dual degeneracy
        // that makes Dantzig pricing cycle on large degenerate LPs. The reported
        // objective uses `cost_true`, so the perturbation is invisible outside.
        let cost_true = cost.clone();
        let mut cost = cost;
        for (j, c) in cost.iter_mut().enumerate() {
            let mut h = (j as u64).wrapping_mul(0x9E3779B97F4A7C15);
            h ^= h >> 29;
            h = h.wrapping_mul(0xBF58476D1CE4E5B9);
            let r = (h >> 11) as f64 / (1u64 << 53) as f64 - 0.5;
            *c += 1e-9 * (1.0 + c.abs()) * r;
        }

        LpRelaxation {
            n,
            m,
            cost,
            cost_true,
            col_lo: col_lo0,
            col_hi: col_hi0,
            row_lo,
            row_hi,
            csc_start: start,
            csc_index: index,
            csc_value: value,
            csr_start,
            csr_index,
            csr_value,
            col_scale,
            is_int: (0..n)
                .map(|j| model.col_type[j].is_integral())
                .collect(),
        }
    }

    #[inline]
    fn col(&self, j: usize) -> impl Iterator<Item = (usize, f64)> + '_ {
        let lo = self.csc_start[j] as usize;
        let hi = self.csc_start[j + 1] as usize;
        (lo..hi).map(move |k| (self.csc_index[k] as usize, self.csc_value[k]))
    }

    pub fn num_col(&self) -> usize {
        self.n
    }
    pub fn num_row(&self) -> usize {
        self.m
    }

    /// Solve with the model's own bounds.
    pub fn solve(&mut self, opts: &LpOptions, clock: &dyn Clock) -> LpSolution {
        let lo = self.col_lo.clone();
        let hi = self.col_hi.clone();
        self.solve_with_bounds(&lo, &hi, opts, clock)
    }

    /// Solve with overridden column bounds (used by branching). Bounds are in
    /// the model's original units.
    pub fn solve_with_bounds(
        &mut self,
        col_lo: &[f64],
        col_hi: &[f64],
        opts: &LpOptions,
        clock: &dyn Clock,
    ) -> LpSolution {
        self.solve_with_basis(col_lo, col_hi, opts, clock, None)
    }

    pub fn solve_with_basis(
        &mut self,
        col_lo: &[f64],
        col_hi: &[f64],
        opts: &LpOptions,
        clock: &dyn Clock,
        basis: Option<&LpBasis>,
    ) -> LpSolution {
        let started = clock.now_secs();
        if col_lo.len() != self.n
            || col_hi.len() != self.n
            || col_lo.iter().chain(col_hi).any(|v| v.is_nan())
        {
            return LpSolution {
                status: LpStatus::Numerical,
                ..Default::default()
            };
        }
        if col_lo.iter().zip(col_hi).any(|(lo, hi)| lo > hi) {
            return LpSolution {
                status: LpStatus::Infeasible,
                ..Default::default()
            };
        }
        let lo_s: Vec<f64> = (0..self.n)
            .map(|j| to_lo(col_lo[j]) / self.col_scale[j])
            .collect();
        let hi_s: Vec<f64> = (0..self.n)
            .map(|j| to_hi(col_hi[j]) / self.col_scale[j])
            .collect();
        let mut solver = Simplex::new(self, &lo_s, &hi_s, opts);
        let mut attempted = LpStats::default();
        if let Some(basis) = basis {
            let warm_options = LpOptions {
                time_limit_secs: (opts.time_limit_secs - (clock.now_secs() - started)).max(0.0),
                ..opts.clone()
            };
            if let Some(mut result) = solver.reoptimize(basis, &warm_options, clock, &mut attempted)
            {
                if result.status != LpStatus::Optimal
                    || respects_bounds(&result.col_value, col_lo, col_hi)
                {
                    result.stats.seconds = clock.now_secs() - started;
                    result.stats.warm_solves = 1;
                    result.stats += attempted;
                    return result;
                }
                attempted.warm_bound_failures += 1;
            }
            attempted.phase2_iterations = solver.warm_iterations;
            attempted.warm_fallbacks = 1;
            solver = Simplex::new(self, &lo_s, &hi_s, opts);
        }
        let remaining = LpOptions {
            time_limit_secs: (opts.time_limit_secs - (clock.now_secs() - started)).max(0.0),
            ..opts.clone()
        };
        let mut result = solver.run(&remaining, clock);
        result.iterations += attempted.phase2_iterations;
        result.stats += attempted;
        result.stats.seconds = clock.now_secs() - started;
        if result.status == LpStatus::Optimal && !respects_bounds(&result.col_value, col_lo, col_hi)
        {
            result.status = LpStatus::Numerical;
            result.dual_bound = None;
            result.basis = None;
        }
        result
    }
}

/// Whether the LP point is inside the requested box, to a tolerance scaled by
/// the bound magnitude. An absolute tolerance is too strict for large-valued
/// columns (e.g. a value of 4000 with a 1e-5 excess is 3e-9 relative, clearly
/// numerical, but exceeds a fixed 1e-6).
fn respects_bounds(x: &[f64], lo: &[f64], hi: &[f64]) -> bool {
    x.iter()
        .zip(lo.iter().zip(hi))
        .all(|(&v, (&l, &u))| {
            v.is_finite()
                && v >= l - 1e-6 - 1e-7 * l.abs()
                && v <= u + 1e-6 + 1e-7 * u.abs()
        })
}

/// `INF` (1e30) -> true infinity for the solver.
#[inline]
fn to_lo(v: f64) -> f64 {
    if v <= -INF / 2.0 {
        f64::NEG_INFINITY
    } else {
        v
    }
}
#[inline]
fn to_hi(v: f64) -> f64 {
    if v >= INF / 2.0 {
        f64::INFINITY
    } else {
        v
    }
}

/// A bound to shift a nonbasic variable from, for tableau cut generation.
enum Shift {
    Lower(f64),
    Upper(f64),
    /// Free on both sides: cannot be shifted to a nonnegative variable.
    Invalid,
}

/// Gomory mixed-integer rounding coefficient for one tableau coefficient
/// (`a`), given the fractional part `f0` of the basic integer variable.
fn gmi_coeff(a: f64, is_int: bool, f0: f64) -> f64 {
    if a == 0.0 {
        return 0.0;
    }
    if is_int {
        let f = a - a.floor();
        if f <= f0 {
            f / f0
        } else {
            (1.0 - f) / (1.0 - f0)
        }
    } else if a > 0.0 {
        a / f0
    } else {
        -a / (1.0 - f0)
    }
}

// ---------------------------------------------------------------------------
// Sparse LU (partial pivoting, dense-accumulator left-looking factorisation)
// ---------------------------------------------------------------------------

/// Data-oriented sparse LU: L and U are stored as flat, compressed columns
/// (`start`/`idx`/`val`) rather than a `Vec<Vec<..>>`, so the forward/back
/// substitutions stream contiguous memory instead of chasing pointers. A whole
/// solve does two of each per iteration, so this is the hot path.
struct SparseLu {
    n: usize,
    work: std::cell::RefCell<Vec<f64>>,
    /// `perm[k]` = original row at elimination position `k`.
    perm: Vec<usize>,
    /// `pinv[r]` = position of original row `r`.
    pinv: Vec<usize>,
    /// L column `k` occupies `l_start[k]..l_start[k+1]`; `l_idx` holds the
    /// ORIGINAL row of each entry.
    l_start: Vec<u32>,
    l_idx: Vec<u32>,
    l_val: Vec<f64>,
    /// U column `k` occupies `u_start[k]..u_start[k+1]`; `u_idx` holds rows `< k`.
    u_start: Vec<u32>,
    u_idx: Vec<u32>,
    u_val: Vec<f64>,
    /// U diagonal `k`.
    diag: Vec<f64>,
    /// Set when a pivot is numerically small; surfaced in the native LP debug
    /// log only.
    #[allow(dead_code)]
    singular: bool,
}

impl SparseLu {
    fn identity(n: usize) -> Self {
        Self {
            n,
            work: std::cell::RefCell::new(vec![0.0; n]),
            perm: (0..n).collect(),
            pinv: (0..n).collect(),
            l_start: vec![0; n + 1],
            l_idx: Vec::new(),
            l_val: Vec::new(),
            u_start: vec![0; n + 1],
            u_idx: Vec::new(),
            u_val: Vec::new(),
            diag: vec![1.0; n],
            singular: false,
        }
    }
    /// Factor the basis whose columns are given sparsely (`cols[k]` = column k).
    fn factor(
        cols: &[Vec<(u32, f64)>],
        n: usize,
        sparse: bool,
        deadline: Option<(&dyn Clock, f64)>,
    ) -> Option<SparseLu> {
        let expired = || deadline.is_some_and(|(clock, until)| clock.now_secs() >= until);
        const TINY: f64 = 1e-12;
        let mut perm: Vec<usize> = vec![0; n];
        let mut pinv: Vec<usize> = vec![usize::MAX; n];
        let mut used = vec![false; n];
        let mut diag = vec![1.0f64; n];
        let mut x = vec![0.0f64; n]; // indexed by ORIGINAL row
                                     // Sparse support of the current column: `active` lists the rows that may
                                     // hold a nonzero `x[r]` (lazily; entries may be zero), `mark[r]` keeps a
                                     // row from being listed twice within one column.
        let mut mark = vec![false; n];
        let mut active: Vec<u32> = Vec::with_capacity(64);
        let mut singular = false;
        let mut l_start: Vec<u32> = Vec::with_capacity(n + 1);
        let mut l_idx: Vec<u32> = Vec::new();
        let mut l_val: Vec<f64> = Vec::new();
        let mut u_start: Vec<u32> = Vec::with_capacity(n + 1);
        let mut u_idx: Vec<u32> = Vec::new();
        let mut u_val: Vec<f64> = Vec::new();
        // Reused min-heap over (pivot index, row): the spike's pivot rows, so
        // elimination touches only the spike, not all `k` prior pivots.
        let mut heap: std::collections::BinaryHeap<std::cmp::Reverse<(u32, u32)>> =
            std::collections::BinaryHeap::new();

        for k in 0..n {
            if k % 32 == 0 && expired() {
                return None;
            }
            l_start.push(l_idx.len() as u32);
            u_start.push(u_idx.len() as u32);
            active.clear();
            for &(r, val) in &cols[k] {
                let ri = r as usize;
                if !mark[ri] {
                    mark[ri] = true;
                    active.push(r);
                }
                x[ri] += val;
            }
            // Eliminate the spike against previously chosen pivot rows, in
            // increasing pivot order. Only the spike (`active`) is touched, not
            // all `k` prior pivots: eliminating pivot `j` only creates entries at
            // rows not in `perm[0..=j]`, so once processed a pivot stays zeroed.
            // Doing it output-sensitively avoids a `x[perm[j]]` gather over the
            // whole basis every column.
            if sparse {
                // Sparse basis: walk only the spike's pivot rows via a min-heap
                // (output-sensitive), touching `l_start[j]` columns in order.
                heap.clear();
                for &r in &active {
                    let j = pinv[r as usize];
                    if j < k {
                        heap.push(std::cmp::Reverse((j as u32, r)));
                    }
                }
                let mut eliminations = 0;
                while let Some(std::cmp::Reverse((j, r))) = heap.pop() {
                    if eliminations % 256 == 0 && expired() {
                        return None;
                    }
                    eliminations += 1;
                    let ri = r as usize;
                    if x[ri] == 0.0 {
                        continue;
                    }
                    let u = x[ri];
                    x[ri] = 0.0;
                    u_idx.push(j);
                    u_val.push(u);
                    let lo = l_start[j as usize] as usize;
                    let hi = l_start[j as usize + 1] as usize;
                    for t in lo..hi {
                        let ii = l_idx[t] as usize;
                        if x[ii] == 0.0 && !mark[ii] {
                            mark[ii] = true;
                            active.push(l_idx[t]);
                        }
                        x[ii] -= l_val[t] * u;
                    }
                    for t in lo..hi {
                        let ii = l_idx[t] as usize;
                        let pj = pinv[ii];
                        if pj < k && x[ii] != 0.0 {
                            heap.push(std::cmp::Reverse((pj as u32, l_idx[t])));
                        }
                    }
                }
            } else {
                // Dense basis: a simple pivot-order scan is cheaper than the
                // heap bookkeeping (most entries are zero and skipped cheaply).
                for j in 0..k {
                    if j % 256 == 0 && expired() {
                        return None;
                    }
                    let pj = perm[j];
                    let u = x[pj];
                    if u != 0.0 {
                        x[pj] = 0.0;
                        u_idx.push(j as u32);
                        u_val.push(u);
                        for t in l_start[j] as usize..l_start[j + 1] as usize {
                            let ii = l_idx[t] as usize;
                            if x[ii] == 0.0 && !mark[ii] {
                                mark[ii] = true;
                                active.push(l_idx[t]);
                            }
                            x[ii] -= l_val[t] * u;
                        }
                    }
                }
            }
            // Partial pivot among the support rows that are not yet pivots.
            let mut pr = usize::MAX;
            let mut best = 0.0f64;
            for &r in &active {
                let ri = r as usize;
                if used[ri] {
                    continue;
                }
                let v = x[ri].abs();
                // Largest magnitude; ties broken by smallest row index (matches a
                // row-order scan), so the factorization — and hence the pivot
                // path — is deterministic and stable on degenerate LPs.
                if v > best || (v == best && v > 0.0 && ri < pr) {
                    best = v;
                    pr = ri;
                }
            }
            if pr == usize::MAX {
                singular = true;
                pr = (0..n).find(|&r| !used[r]).unwrap_or(0);
            }
            if best < TINY {
                singular = true;
            }
            used[pr] = true;
            perm[k] = pr;
            pinv[pr] = k;
            diag[k] = if x[pr].abs() < TINY { 1.0 } else { x[pr] };
            let inv = 1.0 / diag[k];
            for &r in &active {
                let ri = r as usize;
                if !used[ri] && x[ri] != 0.0 {
                    l_idx.push(r);
                    l_val.push(x[ri] * inv);
                }
            }
            for &r in &active {
                let ri = r as usize;
                x[ri] = 0.0;
                mark[ri] = false;
            }
        }
        l_start.push(l_idx.len() as u32);
        u_start.push(u_idx.len() as u32);

        Some(SparseLu {
            n,
            work: std::cell::RefCell::new(vec![0.0; n]),
            perm,
            pinv,
            l_start,
            l_idx,
            l_val,
            u_start,
            u_idx,
            u_val,
            diag,
            singular,
        })
    }

    fn ftran(&self, b: &mut [f64]) {
        let n = self.n;
        let mut work = self.work.borrow_mut();
        let z = &mut *work;
        let (l_start, l_idx, l_val) = (&self.l_start, &self.l_idx, &self.l_val);
        let (u_start, u_idx, u_val) = (&self.u_start, &self.u_idx, &self.u_val);
        let pinv = &self.pinv;
        for k in 0..n {
            z[k] = b[self.perm[k]];
        }
        // L z = pb (forward; L entries are stored by original row).
        for k in 0..n {
            let zk = z[k];
            if zk != 0.0 {
                for t in l_start[k] as usize..l_start[k + 1] as usize {
                    z[pinv[l_idx[t] as usize]] -= l_val[t] * zk;
                }
            }
        }
        // U x = z (back substitution). z is the solution B^{-1}b.
        for k in (0..n).rev() {
            let xk = z[k] / self.diag[k];
            for t in u_start[k] as usize..u_start[k + 1] as usize {
                z[u_idx[t] as usize] -= u_val[t] * xk;
            }
            z[k] = xk;
        }
        b.copy_from_slice(z);
    }

    fn btran(&self, c: &mut [f64]) {
        let n = self.n;
        let (u_start, u_idx, u_val) = (&self.u_start, &self.u_idx, &self.u_val);
        let (l_start, l_idx, l_val) = (&self.l_start, &self.l_idx, &self.l_val);
        let pinv = &self.pinv;
        // U^T u = c (forward).
        let mut work = self.work.borrow_mut();
        work.copy_from_slice(c);
        let u = &mut *work;
        for k in 0..n {
            let mut s = u[k];
            for t in u_start[k] as usize..u_start[k + 1] as usize {
                s -= u_val[t] * u[u_idx[t] as usize];
            }
            u[k] = s / self.diag[k];
        }
        // L^T w = u (back substitution; column k of L, rows below the diagonal).
        let w = u;
        for k in (0..n).rev() {
            let mut s = w[k];
            for t in l_start[k] as usize..l_start[k + 1] as usize {
                s -= l_val[t] * w[pinv[l_idx[t] as usize]];
            }
            w[k] = s;
        }
        // y = P^T w.
        for k in 0..n {
            c[self.perm[k]] = w[k];
        }
    }
}

// ---------------------------------------------------------------------------
// Simplex
// ---------------------------------------------------------------------------

const LOG_OFF: usize = 0; // placeholder to keep indexing explicit below

/// Maximum cuts generated per LP solve.
const MAX_CUTS: usize = 8;

struct Simplex<'a> {
    lpr: &'a LpRelaxation,
    n: usize,
    m: usize,
    v: usize, // n + 2m

    lo: Vec<f64>,
    hi: Vec<f64>,
    cost: Vec<f64>,
    x: Vec<f64>,

    basis: Vec<usize>,
    in_basis: Vec<i32>,

    /// Artificial sign per row (0.0 = unused).
    art_sign: Vec<f64>,
    /// 1 during Phase 1, 2 during Phase 2 (diagnostics only).
    phase: u8,
    phase1_iterations: u64,
    warm_iterations: u64,

    lu: SparseLu,
    /// Eta file, flattened to parallel arrays: eta `e` has pivot row `eta_p[e]`
    /// and entries `eta_start[e]..eta_start[e+1]` in `eta_idx` / `eta_val` (the
    /// first entry is `(p, 1/d_p)`). Contiguous, so `apply_etas` streams.
    eta_p: Vec<u32>,
    eta_start: Vec<u32>,
    eta_idx: Vec<u32>,
    eta_val: Vec<f64>,
    /// Cycling start for partial pricing over structural columns.
    price_pos: usize,
    /// Nonzeros in the current L/U (cost of one factorization), used to decide
    /// when eta growth makes a fresh factorization worthwhile.
    factor_nnz: usize,
    /// Whether to factor with the sparse (spike/heap) or dense (pivot-scan)
    /// elimination; chosen from the previous factor's fill. Identical L/U
    /// either way — this only picks the cheaper traversal.
    sparse_factor: bool,
    base_cols: Vec<Vec<(u32, f64)>>,
    /// Generate cuts at optimality (root cut loop only).
    generate_cuts: bool,
    /// Wall-time accumulators (seconds): btran, price, ftran, ratio+update,
    /// refactor, reserve. Populated only under the `profiling` feature.
    #[cfg(feature = "profiling")]
    prof: [f64; 6],
}

#[inline]
fn log_of(n: usize, r: usize) -> usize {
    n + r
}
#[inline]
fn art_of(n: usize, m: usize, r: usize) -> usize {
    n + m + r
}

impl<'a> Simplex<'a> {
    fn new(
        lpr: &'a LpRelaxation,
        col_lo: &[f64],
        col_hi: &[f64],
        opts: &LpOptions,
    ) -> Simplex<'a> {
        let n = lpr.n;
        let m = lpr.m;
        let v = n + 2 * m;
        let mut lo = vec![0.0f64; v];
        let mut hi = vec![0.0f64; v];
        for j in 0..n {
            lo[j] = col_lo[j];
            hi[j] = col_hi[j];
        }
        for r in 0..m {
            lo[log_of(n, r)] = lpr.row_lo[r];
            hi[log_of(n, r)] = lpr.row_hi[r];
            lo[art_of(n, m, r)] = 0.0;
            hi[art_of(n, m, r)] = f64::INFINITY;
        }
        let cost = vec![0.0f64; v];
        let x = vec![0.0f64; v];
        let basis = vec![0usize; m];
        let in_basis = vec![-1i32; v];
        let art_sign = vec![0.0f64; m];
        let lu = SparseLu::identity(m);
        let base_cols = vec![Vec::new(); m];
        Simplex {
            lpr,
            n,
            m,
            v,
            lo,
            hi,
            cost,
            x,
            basis,
            in_basis,
            art_sign,
            phase: 0,
            phase1_iterations: 0,
            warm_iterations: 0,
            lu,
            eta_p: Vec::new(),
            eta_start: vec![0],
            eta_idx: Vec::new(),
            eta_val: Vec::new(),
            price_pos: 0,
            factor_nnz: 0,
            sparse_factor: true,
            base_cols,
            generate_cuts: opts.generate_cuts,
            #[cfg(feature = "profiling")]
            prof: [0.0; 6],
        }
    }

    /// Sparse column of an extended variable.
    fn column(&self, var: usize, out: &mut Vec<(u32, f64)>) {
        out.clear();
        if var < self.n {
            for (r, a) in self.lpr.col(var) {
                out.push((r as u32, a));
            }
        } else if var < self.n + self.m {
            let r = var - self.n;
            out.push((r as u32, -1.0));
        } else {
            let r = var - self.n - self.m;
            out.push((r as u32, self.art_sign[r]));
        }
    }

    fn fill_column(&self, var: usize, d: &mut [f64]) {
        d.fill(0.0);
        if var < self.n {
            for (r, a) in self.lpr.col(var) {
                d[r] += a;
            }
        } else if var < self.n + self.m {
            d[var - self.n] = -1.0;
        } else {
            d[var - self.n - self.m] = self.art_sign[var - self.n - self.m];
        }
    }

    fn dense_column(&self, var: usize) -> Vec<f64> {
        let mut d = vec![0.0; self.m];
        self.fill_column(var, &mut d);
        d
    }

    fn apply_etas_ftran(&self, z: &mut [f64]) {
        let (eta_p, eta_start, eta_idx, eta_val) =
            (&self.eta_p, &self.eta_start, &self.eta_idx, &self.eta_val);
        for e in 0..eta_p.len() {
            let p = eta_p[e] as usize;
            let zp = z[p];
            if zp != 0.0 {
                let mut newp = zp;
                for t in eta_start[e] as usize..eta_start[e + 1] as usize {
                    let i = eta_idx[t] as usize;
                    let wi = eta_val[t];
                    if i == p {
                        newp = wi * zp;
                    } else {
                        z[i] += wi * zp;
                    }
                }
                z[p] = newp;
            }
        }
    }

    fn apply_etas_btran(&self, z: &mut [f64]) {
        let (eta_p, eta_start, eta_idx, eta_val) =
            (&self.eta_p, &self.eta_start, &self.eta_idx, &self.eta_val);
        for e in (0..eta_p.len()).rev() {
            let p = eta_p[e] as usize;
            let mut dot = 0.0;
            for t in eta_start[e] as usize..eta_start[e + 1] as usize {
                dot += eta_val[t] * z[eta_idx[t] as usize];
            }
            z[p] = dot;
        }
    }

    /// Append the eta for a pivot in row `p` whose (already ftran'd) column is
    /// `d` with pivot element `dp`.
    #[inline]
    fn push_eta(&mut self, p: usize, d: &[f64], dp: f64) {
        self.eta_p.push(p as u32);
        self.eta_idx.push(p as u32);
        self.eta_val.push(1.0 / dp);
        let inv = -1.0 / dp;
        for (i, &di) in d.iter().enumerate() {
            if i != p && di != 0.0 {
                self.eta_idx.push(i as u32);
                self.eta_val.push(di * inv);
            }
        }
        self.eta_start.push(self.eta_idx.len() as u32);
    }

    #[inline]
    fn clear_etas(&mut self) {
        self.eta_p.clear();
        self.eta_start.clear();
        self.eta_start.push(0);
        self.eta_idx.clear();
        self.eta_val.clear();
    }

    fn ftran(&self, b: &mut [f64]) {
        self.lu.ftran(b);
        self.apply_etas_ftran(b);
    }
    fn btran(&self, c: &mut [f64]) {
        self.apply_etas_btran(c);
        self.lu.btran(c);
    }

    /// Max residual of the basis equations `A'x' - s + sigma*alpha = 0`.
    /// Used by the native LP debug log only.
    #[allow(dead_code)]
    fn equation_residual(&self) -> f64 {
        let mut act = vec![0.0f64; self.m];
        for j in 0..self.n {
            if self.x[j] != 0.0 {
                for (r, a) in self.lpr.col(j) {
                    act[r] += a * self.x[j];
                }
            }
        }
        let mut mx = 0.0f64;
        for r in 0..self.m {
            let res = act[r] - self.x[log_of(self.n, r)]
                + self.art_sign[r] * self.x[art_of(self.n, self.m, r)];
            mx = mx.max(res.abs());
        }
        mx
    }

    /// Rebuild the basis matrix and factor it freshly (drops all etas).
    fn refactorize(&mut self, clock: &dyn Clock, deadline: f64) -> bool {
        #[cfg(feature = "profiling")]
        let _t0 = std::time::Instant::now();
        for k in 0..self.m {
            if k % 128 == 0 && clock.now_secs() >= deadline {
                return false;
            }
            let var = self.basis[k];
            let mut col = std::mem::take(&mut self.base_cols[k]);
            self.column(var, &mut col);
            self.base_cols[k] = col;
        }
        let Some(lu) = SparseLu::factor(
            &self.base_cols,
            self.m,
            self.sparse_factor,
            Some((clock, deadline)),
        ) else {
            return false;
        };
        self.lu = lu;
        self.factor_nnz = self.lu.l_idx.len() + self.lu.u_idx.len();
        // Dense baselines prefer the plain pivot scan; sparse ones the heap.
        self.sparse_factor = self.factor_nnz <= 8 * self.m;
        #[cfg(feature = "profiling")]
        {
            self.prof[0] += _t0.elapsed().as_secs_f64();
        }
        self.clear_etas();
        self.recompute_basic();
        if clock.now_secs() >= deadline {
            return false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if std::env::var("OPTIO_LPDEBUG").is_ok() {
            eprintln!(
                "[lp]   refactor residual={:.3e} singular={} m={}",
                self.equation_residual(),
                self.lu.singular,
                self.m
            );
        }
        true
    }

    /// Recompute basic values from the nonbasic point: `B x_B = -(N x_N)`.
    fn recompute_basic(&mut self) {
        let mut rhs = vec![0.0f64; self.m];
        for var in 0..self.v {
            if self.in_basis[var] >= 0 {
                continue;
            }
            let xv = self.x[var];
            if xv != 0.0 {
                let mut col = Vec::new();
                self.column(var, &mut col);
                for (r, a) in col {
                    rhs[r as usize] += a * xv;
                }
            }
        }
        for r in 0..self.m {
            rhs[r] = -rhs[r];
        }
        self.lu.ftran(&mut rhs);
        self.apply_etas_ftran(&mut rhs);
        for k in 0..self.m {
            let var = self.basis[k];
            self.x[var] = rhs[k];
        }
    }

    /// Reset `x` from scratch (all nonbasics at a bound), choose the initial
    /// basis (logical where the row activity is already within bounds, else an
    /// artificial), then let `recompute_basic` set consistent basic values.
    fn initialize(&mut self, clock: &dyn Clock, deadline: f64) -> bool {
        for var in 0..self.v {
            self.in_basis[var] = -1;
            self.x[var] = 0.0;
        }
        // Structural nonbasics at the bound nearest to zero.
        for j in 0..self.n {
            self.x[j] = clamp0(self.lo[j], self.hi[j]);
        }
        // Row activity of that point (scaled).
        let mut act = vec![0.0f64; self.m];
        for j in 0..self.n {
            if self.x[j] != 0.0 {
                for (r, a) in self.lpr.col(j) {
                    act[r] += a * self.x[j];
                }
            }
        }
        // Greedy primal crash: push structural variables to remove row
        // violations so most rows are already satisfied and only a few need an
        // artificial in Phase 1.
        if !self.crash(&mut act, clock, deadline) {
            return false;
        }
        for r in 0..self.m {
            let s = log_of(self.n, r);
            let a_var = art_of(self.n, self.m, r);
            let lo_s = self.lo[s];
            let hi_s = self.hi[s];
            if act[r] >= lo_s - 1e-9 && act[r] <= hi_s + 1e-9 {
                // Row already satisfied: logical is basic.
                self.art_sign[r] = 0.0;
                self.basis[r] = s;
                self.in_basis[s] = r as i32;
            } else {
                let b = clamp_bound(act[r], lo_s, hi_s);
                self.x[s] = b;
                let diff = b - act[r];
                self.art_sign[r] = if diff >= 0.0 { 1.0 } else { -1.0 };
                self.basis[r] = a_var;
                self.in_basis[a_var] = r as i32;
            }
        }
        if !self.refactorize_no_recompute(clock, deadline) {
            return false;
        }
        self.clear_etas();
        self.recompute_basic();
        clock.now_secs() < deadline
    }

    /// Best-effort primal crash on the nonbasic structural point. Sweeps the rows
    /// and moves structural variables (within bounds) to satisfy violated rows.
    /// Any assignment is a valid nonbasic point — artificials cover whatever
    /// remains — so this cannot change the optimum; it only reduces the number of
    /// Phase-1 pivots. `act` is kept in sync with `x`.
    fn crash(&mut self, act: &mut [f64], clock: &dyn Clock, deadline: f64) -> bool {
        const SWEEPS: usize = 8;
        let n = self.n;
        let lpr = self.lpr;
        for _ in 0..SWEEPS {
            let mut changed = false;
            for r in 0..self.m {
                if r % 32 == 0 && clock.now_secs() >= deadline {
                    return false;
                }
                let lo = self.lo[log_of(n, r)];
                let hi = self.hi[log_of(n, r)];
                // Satisfy this row when visited (a bounded number of moves).
                let mut guard = 0usize;
                loop {
                    if guard % 128 == 0 && clock.now_secs() >= deadline {
                        return false;
                    }
                    let ar = act[r];
                    let (dir, need) = if ar < lo - 1e-9 {
                        (1.0, lo - ar)
                    } else if ar > hi + 1e-9 {
                        (-1.0, ar - hi)
                    } else {
                        break;
                    };
                    // Pick the structural variable with the largest |a| that can
                    // move in the needed direction.
                    let mut bj = usize::MAX;
                    let mut ba = 0.0f64;
                    for k in lpr.csr_start[r] as usize..lpr.csr_start[r + 1] as usize {
                        let j = lpr.csr_index[k] as usize;
                        if j >= n {
                            continue;
                        }
                        let a = lpr.csr_value[k];
                        if a == 0.0 {
                            continue;
                        }
                        let mdir = if a * dir > 0.0 { 1.0 } else { -1.0 };
                        let room = if mdir > 0.0 {
                            self.hi[j] - self.x[j]
                        } else {
                            self.x[j] - self.lo[j]
                        };
                        if room <= 1e-12 {
                            continue;
                        }
                        if a.abs() > ba {
                            ba = a.abs();
                            bj = j;
                        }
                    }
                    if bj == usize::MAX {
                        break;
                    }
                    // Re-derive the coefficient of `bj` in row `r`.
                    let mut coef = 0.0f64;
                    for k in lpr.csr_start[r] as usize..lpr.csr_start[r + 1] as usize {
                        if lpr.csr_index[k] as usize == bj {
                            coef = lpr.csr_value[k];
                            break;
                        }
                    }
                    let desired = dir * need / coef;
                    let old = self.x[bj];
                    let nv = (old + desired).max(self.lo[bj]).min(self.hi[bj]);
                    let dd = nv - old;
                    if dd.abs() <= 1e-12 {
                        break;
                    }
                    self.x[bj] = nv;
                    for (rr, a2) in lpr.col(bj) {
                        act[rr] += a2 * dd;
                    }
                    changed = true;
                    guard += 1;
                    if guard > n {
                        break;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        true
    }

    fn refactorize_no_recompute(&mut self, clock: &dyn Clock, deadline: f64) -> bool {
        for k in 0..self.m {
            if k % 128 == 0 && clock.now_secs() >= deadline {
                return false;
            }
            let var = self.basis[k];
            let mut col = std::mem::take(&mut self.base_cols[k]);
            self.column(var, &mut col);
            self.base_cols[k] = col;
        }
        let Some(lu) = SparseLu::factor(
            &self.base_cols,
            self.m,
            self.sparse_factor,
            Some((clock, deadline)),
        ) else {
            return false;
        };
        self.lu = lu;
        true
    }

    /// Set costs for phase 1 (minimise sum of artificials).
    fn set_phase1(&mut self) {
        self.phase = 1;
        for var in 0..self.v {
            self.cost[var] = 0.0;
        }
        for r in 0..self.m {
            if self.art_sign[r] != 0.0 {
                self.cost[art_of(self.n, self.m, r)] = 1.0;
            }
        }
    }

    /// Set costs for phase 2 (real objective; artificials fixed at 0).
    fn set_phase2(&mut self) {
        self.phase = 2;
        for j in 0..self.n {
            self.cost[j] = self.lpr.cost[j];
        }
        for r in 0..self.m {
            let s = log_of(self.n, r);
            self.cost[s] = 0.0;
            let a = art_of(self.n, self.m, r);
            self.cost[a] = 0.0;
            // Freeze any artificial (whether basic at 0 or not).
            self.lo[a] = 0.0;
            self.hi[a] = 0.0;
        }
    }

    fn run(&mut self, opts: &LpOptions, clock: &dyn Clock) -> LpSolution {
        let start = clock.now_secs();
        if !self.initialize(clock, start + opts.time_limit_secs) {
            return self.timeout(0);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if std::env::var("OPTIO_LPDEBUG").is_ok() {
            eprintln!("[lp] after init residual={:.3e}", self.equation_residual());
        }
        self.set_phase1();

        let mut iters = 0u64;
        let st1 = self.optimize(opts, clock, start, &mut iters);
        self.phase1_iterations = iters;
        #[cfg(not(target_arch = "wasm32"))]
        let lpdebug = std::env::var("OPTIO_LPDEBUG").is_ok();
        #[cfg(target_arch = "wasm32")]
        let lpdebug = false;
        if st1 != LpStatus::Optimal {
            return self.finish(st1, iters);
        }
        // Phase-1 objective = sum of artificial values.
        let mut infeas = 0.0f64;
        for r in 0..self.m {
            infeas += self.x[art_of(self.n, self.m, r)].max(0.0);
        }
        if lpdebug {
            // Real row-activity violation of the phase-1 point (scaled space).
            let mut act = vec![0.0f64; self.m];
            for j in 0..self.n {
                if self.x[j] != 0.0 {
                    for (r, a) in self.lpr.col(j) {
                        act[r] += a * self.x[j];
                    }
                }
            }
            let mut maxv = 0.0f64;
            for r in 0..self.m {
                if self.lpr.row_lo[r].is_finite() {
                    maxv = maxv.max(self.lpr.row_lo[r] - act[r]);
                }
                if self.lpr.row_hi[r].is_finite() {
                    maxv = maxv.max(act[r] - self.lpr.row_hi[r]);
                }
            }
            eprintln!(
                "[lp] phase1 status={:?} infeas={:.3e} real_row_viol={:.3e} iters={} m={} n={}",
                st1, infeas, maxv, iters, self.m, self.n
            );
            let (mut amin, mut amax) = (f64::INFINITY, f64::NEG_INFINITY);
            for r in 0..self.m {
                let a = self.x[art_of(self.n, self.m, r)];
                amin = amin.min(a);
                amax = amax.max(a);
            }
            eprintln!("[lp] artificial range [{:.3e}, {:.3e}]", amin, amax);
            // Consistency residual of the maintained point: A'x' - s' + sigma*alpha.
            let mut maxres = 0.0f64;
            for r in 0..self.m {
                let s = self.x[log_of(self.n, r)];
                let a = self.x[art_of(self.n, self.m, r)];
                let res = act[r] - s + self.art_sign[r] * a;
                maxres = maxres.max(res.abs());
            }
            eprintln!("[lp] max equation residual = {:.3e}", maxres);
            for r in 0..self.m {
                if self.art_sign[r] != 0.0 {
                    let a = art_of(self.n, self.m, r);
                    let s = log_of(self.n, r);
                    eprintln!(
                        "[lp]   row {} art_sign={:+.0} art={:.6} basic(art)={} logical={:.6} basic(log)={} x{}=[{:.3},{:.3}]",
                        r,
                        self.art_sign[r],
                        self.x[a],
                        self.in_basis[a] >= 0,
                        self.x[s],
                        self.in_basis[s] >= 0,
                        r,
                        self.lo[s],
                        self.hi[s]
                    );
                }
            }
        }
        if infeas > 1e-5 * (1.0 + self.m as f64) {
            return self.finish(LpStatus::Infeasible, iters);
        }

        self.set_phase2();
        // Artificials now fixed; try to purge basic ones so the basis is clean.
        if !self.purge_artificials(clock, start + opts.time_limit_secs) {
            return self.timeout(iters);
        }
        let st2 = self.optimize(opts, clock, start, &mut iters);
        #[cfg(feature = "profiling")]
        if std::env::var("OPTIO_LPTIME").is_ok() {
            eprintln!(
                "[lptime] iters={} btran={:.3} price={:.3} ftran={:.3} ratio_update={:.3} (refactor={:.3}) factor_nnz={} n={} m={}",
                iters, self.prof[1], self.prof[2], self.prof[3], self.prof[4], self.prof[0], self.factor_nnz, self.n, self.m
            );
        }
        self.finish(st2, iters)
    }

    /// Bounded dual simplex: preserve reduced-cost feasibility while repairing
    /// basic variables made infeasible by tighter child bounds. A suspect
    /// basis/pivot falls back to the cold solver; it never certifies infeasibility.
    fn reoptimize(
        &mut self,
        saved: &LpBasis,
        opts: &LpOptions,
        clock: &dyn Clock,
        diagnostics: &mut LpStats,
    ) -> Option<LpSolution> {
        let start = clock.now_secs();
        if saved.basis.len() != self.m || saved.x.len() != self.n + self.m {
            diagnostics.warm_invalid_basis += 1;
            return None;
        }
        self.set_phase2();
        self.basis.clone_from(&saved.basis);
        self.in_basis.fill(-1);
        for (p, &j) in self.basis.iter().enumerate() {
            if j >= self.n + self.m || self.in_basis[j] >= 0 {
                diagnostics.warm_invalid_basis += 1;
                return None;
            }
            self.in_basis[j] = p as i32;
        }
        self.x[..self.n + self.m].copy_from_slice(&saved.x);
        for j in 0..self.n + self.m {
            if self.in_basis[j] < 0 {
                self.x[j] = self.x[j].clamp(self.lo[j], self.hi[j]);
            }
        }
        if !self.refactorize(clock, start + opts.time_limit_secs) {
            return Some(self.timeout(self.warm_iterations));
        }
        if self.lu.singular {
            diagnostics.warm_singular_basis += 1;
            return None;
        }
        if self.equation_residual() > 1e-6 {
            diagnostics.warm_residual_failures += 1;
            return None;
        }
        let mut y = vec![0.0; self.m];
        let mut row = vec![0.0; self.m];
        let mut d = vec![0.0; self.m];
        let mut rc = vec![0.0; self.n + self.m];
        loop {
            if clock.now_secs() - start >= opts.time_limit_secs {
                return Some(self.finish(LpStatus::TimeLimit, self.warm_iterations));
            }
            if self.warm_iterations >= opts.max_iterations {
                diagnostics.warm_iteration_limits += 1;
                return None;
            }
            for p in 0..self.m {
                y[p] = self.cost[self.basis[p]];
            }
            self.btran(&mut y);
            let mut flipped = false;
            for j in 0..self.n + self.m {
                rc[j] = self.cost[j];
                if j < self.n {
                    for (r, a) in self.lpr.col(j) {
                        rc[j] -= a * y[r];
                    }
                } else {
                    rc[j] += y[j - self.n];
                }
                if self.in_basis[j] >= 0 || self.lo[j] == self.hi[j] {
                    continue;
                }
                let at_lo = self.x[j] <= self.lo[j] + opts.primal_tol;
                let at_hi = self.x[j] >= self.hi[j] - opts.primal_tol;
                if (!at_hi && rc[j] < -opts.dual_tol * 10.0)
                    || (!at_lo && rc[j] > opts.dual_tol * 10.0)
                {
                    // A tightened box may change the active endpoint. Keep the
                    // basis and restore the reduced-cost sign by moving to its
                    // minimizing endpoint, then repair basic feasibility. This
                    // is safe only with a finite opposite bound; free rays still
                    // require the cold solver.
                    let target = if rc[j] > 0.0 { self.lo[j] } else { self.hi[j] };
                    if !target.is_finite() {
                        diagnostics.warm_dual_failures += 1;
                        return None;
                    }
                    self.x[j] = target;
                    diagnostics.warm_bound_flips += 1;
                    flipped = true;
                }
            }
            if flipped {
                self.recompute_basic();
                if self.equation_residual() > 1e-6 {
                    diagnostics.warm_residual_failures += 1;
                    return None;
                }
            }
            let mut leave = None;
            let mut violation = opts.primal_tol;
            for p in 0..self.m {
                let j = self.basis[p];
                let low = self.lo[j] - self.x[j];
                let high = self.x[j] - self.hi[j];
                let v = low.max(high);
                if v > violation {
                    violation = v;
                    leave = Some((p, if low > high { 1.0 } else { -1.0 }));
                }
            }
            let Some((p, sign)) = leave else {
                if self.equation_residual() > 1e-6 {
                    diagnostics.warm_residual_failures += 1;
                    return None;
                }
                return Some(self.finish(LpStatus::Optimal, self.warm_iterations));
            };
            row.fill(0.0);
            row[p] = 1.0;
            self.btran(&mut row);
            let mut best = None;
            let mut ratio = f64::INFINITY;
            let mut best_alpha = 0.0;
            for j in 0..self.n + self.m {
                if self.in_basis[j] >= 0 || self.lo[j] == self.hi[j] {
                    continue;
                }
                let alpha = if j < self.n {
                    self.lpr.col(j).map(|(r, a)| row[r] * a).sum::<f64>()
                } else {
                    -row[j - self.n]
                };
                for dir in [-1.0, 1.0] {
                    if (dir > 0.0 && self.x[j] >= self.hi[j] - opts.primal_tol)
                        || (dir < 0.0 && self.x[j] <= self.lo[j] + opts.primal_tol)
                    {
                        continue;
                    }
                    let q = -sign * alpha * dir;
                    if q < 1e-9 {
                        continue;
                    }
                    let candidate = (rc[j] * dir).max(0.0) / q;
                    if candidate < ratio - 1e-10
                        || ((candidate - ratio).abs() <= 1e-10 && alpha.abs() > best_alpha)
                    {
                        ratio = candidate;
                        best = Some((j, alpha));
                        best_alpha = alpha.abs();
                    }
                }
            }
            let Some((enter, alpha)) = best else {
                return if self.infeasibility_certificate(&row) {
                    Some(self.finish(LpStatus::Infeasible, self.warm_iterations))
                } else {
                    diagnostics.warm_no_pivot += 1;
                    None
                };
            };
            d.fill(0.0);
            self.fill_column(enter, &mut d);
            self.ftran(&mut d);
            if d[p].abs() < 1e-9 || (d[p] - alpha).abs() > 1e-6 * (1.0 + alpha.abs()) {
                diagnostics.warm_pivot_failures += 1;
                return None;
            }
            let leaving = self.basis[p];
            let target = if sign > 0.0 {
                self.lo[leaving]
            } else {
                self.hi[leaving]
            };
            let step = (self.x[leaving] - target) / d[p];
            if !step.is_finite() {
                diagnostics.warm_pivot_failures += 1;
                return None;
            }
            self.x[enter] += step;
            for k in 0..self.m {
                self.x[self.basis[k]] -= d[k] * step;
            }
            self.x[leaving] = target;
            self.basis[p] = enter;
            self.in_basis[leaving] = -1;
            self.in_basis[enter] = p as i32;
            self.push_eta(p, &d, d[p]);
            self.warm_iterations += 1;
            if self.eta_p.len() >= opts.refactor_interval {
                if !self.refactorize(clock, start + opts.time_limit_secs) {
                    return Some(self.timeout(self.warm_iterations));
                }
                if self.lu.singular {
                    diagnostics.warm_singular_basis += 1;
                    return None;
                }
            }
        }
    }

    /// Check a numerical Farkas witness independently of the basis: any row
    /// multiplier gives A^T y, whose box interval must contain zero for a
    /// feasible extended point. Outward coefficient intervals protect signs.
    fn infeasibility_certificate(&self, y: &[f64]) -> bool {
        let mut lower = 0.0;
        let mut upper = 0.0;
        let mut magnitude = 1.0;
        for j in 0..self.n + self.m {
            let mut a = 0.0;
            let mut dot = 0.0;
            let mut ops = 0;
            if j < self.n {
                for (r, c) in self.lpr.col(j) {
                    let v = c * y[r];
                    a += v;
                    dot += v.abs();
                    ops += 2;
                }
            } else {
                a = -y[j - self.n];
            }
            let error = 8.0 * f64::EPSILON * (ops + 1) as f64 * dot;
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for c in [a - error, a + error] {
                if c == 0.0 {
                    lo = lo.min(0.0);
                    hi = hi.max(0.0);
                    continue;
                }
                let l = c * if c > 0.0 { self.lo[j] } else { self.hi[j] };
                let u = c * if c > 0.0 { self.hi[j] } else { self.lo[j] };
                lo = lo.min(l);
                hi = hi.max(u);
            }
            lower += lo;
            upper += hi;
            if lo.is_finite() {
                magnitude += lo.abs();
            }
            if hi.is_finite() {
                magnitude += hi.abs();
            }
        }
        let guard = 64.0 * f64::EPSILON * (self.n + self.m + 1) as f64 * magnitude;
        (lower.is_finite() && lower > guard) || (upper.is_finite() && upper < -guard)
    }

    /// Drive basic artificials (at 0) out of the basis where a pivot exists.
    fn purge_artificials(&mut self, clock: &dyn Clock, deadline: f64) -> bool {
        for p in 0..self.m {
            if clock.now_secs() >= deadline {
                return false;
            }
            let b = self.basis[p];
            if b < self.n + self.m {
                continue;
            }
            // Basic artificial at 0: look for a structural/logical nonbasic
            // with a nonzero pivot entry.
            let mut y = vec![0.0f64; self.m];
            y[p] = 1.0;
            self.btran(&mut y); // y = B^{-T} e_p
            let mut chosen: Option<(usize, f64)> = None;
            for j in 0..self.n + self.m {
                if self.in_basis[j] >= 0 {
                    continue;
                }
                let mut col = Vec::new();
                self.column(j, &mut col);
                let mut prod = 0.0;
                for (r, a) in &col {
                    prod += a * y[*r as usize];
                }
                if prod.abs() > 1e-8 {
                    chosen = Some((j, prod));
                    break;
                }
            }
            if let Some((j, prod)) = chosen {
                // Pivot: replace artificial in basis position p with j.
                let d = self.dense_column(j);
                let d = {
                    let mut dd = d;
                    self.ftran(&mut dd);
                    dd
                };
                let dp = if d[p].abs() > 1e-12 { d[p] } else { prod };
                if self.eta_p.len() >= opts_refactor_interval(self) {
                    if !self.refactorize(clock, deadline) {
                        return false;
                    }
                }
                let old = self.basis[p];
                self.basis[p] = j;
                self.in_basis[old] = -1;
                self.in_basis[j] = p as i32;
                self.push_eta(p, &d, dp);
            }
        }
        self.recompute_basic();
        clock.now_secs() < deadline
    }

    /// Choose a finite bound to shift a nonbasic variable from, preferring the
    /// bound it currently sits at.
    fn shift_bound(&self, j: usize) -> Shift {
        let lo = self.lo[j];
        let hi = self.hi[j];
        let v = self.x[j];
        if hi.is_finite() && (hi - v).abs() <= 1e-7 * (1.0 + v.abs()) {
            return Shift::Upper(hi);
        }
        if lo.is_finite() {
            return Shift::Lower(lo);
        }
        if hi.is_finite() {
            return Shift::Upper(hi);
        }
        Shift::Invalid
    }

    /// Generate valid inequalities (Gomory mixed-integer cuts) violated by the
    /// optimal basis. The simplex works in equilibration-scaled variables, so an
    /// integer basic variable's tableau row is rescaled to original units before
    /// rounding; each returned cut is `lower <= Σ values[j]·x_j` over original
    /// structural columns and is valid for the mixed-integer feasible set.
    ///
    /// `is_int` must be the *true* model integrality: a variable treated as
    /// integer here must be integer at every feasible point, otherwise the cut is
    /// only valid on a subset and can exclude feasible points.
    fn gmi_cuts(&self, limit: usize) -> Vec<Cut> {
        let mut cuts: Vec<Cut> = Vec::new();
        if self.phase != 2 {
            return cuts;
        }
        let mut col: Vec<(u32, f64)> = Vec::new();
        // (column, D = tableau coefficient in original units, shift bound, sign).
        let mut terms: Vec<(u32, f64, f64, f64, bool)> = Vec::new();
        for p in 0..self.m {
            if cuts.len() >= limit {
                break;
            }
            let bvar = self.basis[p];
            if bvar >= self.n || !self.lpr.is_int[bvar] {
                continue;
            }
            let cs_i = self.lpr.col_scale[bvar];
            let mut w = vec![0.0f64; self.m];
            w[p] = 1.0;
            self.btran(&mut w);
            terms.clear();
            let mut valid = true;
            for j in 0..self.n {
                if self.in_basis[j] >= 0 {
                    continue;
                }
                let (c, s) = match self.shift_bound(j) {
                    Shift::Invalid => {
                        valid = false;
                        break;
                    }
                    Shift::Lower(b) => (b, 1.0),
                    Shift::Upper(b) => (b, -1.0),
                };
                self.column(j, &mut col);
                let mut dot = 0.0f64;
                for (r, v) in &col {
                    dot += w[*r as usize] * *v;
                }
                terms.push((j as u32, dot * cs_i, c, s, self.lpr.is_int[j]));
            }
            if !valid {
                continue;
            }
            for r in 0..self.m {
                let j = self.n + r;
                if self.in_basis[j] >= 0 {
                    continue;
                }
                let (c, s) = match self.shift_bound(j) {
                    Shift::Invalid => {
                        valid = false;
                        break;
                    }
                    Shift::Lower(b) => (b, 1.0),
                    Shift::Upper(b) => (b, -1.0),
                };
                terms.push((j as u32, -w[r] * cs_i, c, s, false));
            }
            if !valid {
                continue;
            }
            // x_i + Σ D_j z_j = 0  --shift-->  x_i + Σ A_j y_j = b.
            let mut b = 0.0f64;
            for &(_, d, c, _, _) in &terms {
                b -= d * c;
            }
            let f0 = b - b.floor();
            if !(1e-6..=1.0 - 1e-6).contains(&f0) {
                continue;
            }
            let mut rhs = 1.0f64;
            let mut margin = 0.0f64;
            let mut acc: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
            for &(j, d, c, s, isint) in &terms {
                let gamma = gmi_coeff(d * s, isint, f0);
                if gamma == 0.0 {
                    continue;
                }
                // y_j = s_j·(z_j - c_j): coefficient on z_j is γ_j·s_j.
                let gs = gamma * s;
                rhs += gs * c;
                margin += (gs * c).abs();
                if (j as usize) < self.n {
                    let cs_j = self.lpr.col_scale[j as usize];
                    *acc.entry(j).or_insert(0.0) += gs / cs_j;
                } else {
                    let r = j as usize - self.n;
                    for k in self.lpr.csr_start[r] as usize..self.lpr.csr_start[r + 1] as usize {
                        let jk = self.lpr.csr_index[k];
                        *acc.entry(jk).or_insert(0.0) +=
                            gs * self.lpr.csr_value[k] / self.lpr.col_scale[jk as usize];
                    }
                }
            }
            if !rhs.is_finite() {
                continue;
            }
            // Numerical guard: weaken the cut by a small, scale-relative margin so
            // roundoff in the tableau cannot make it exclude a feasible point.
            rhs -= 1e-9 * (1.0 + margin);
            let mut entries: Vec<(u32, f64)> = acc
                .into_iter()
                .filter(|(_, v)| v.abs() > 1e-12)
                .collect();
            if entries.is_empty() {
                continue;
            }
            entries.sort_by_key(|&(j, _)| j);
            cuts.push(Cut {
                indices: entries.iter().map(|&(j, _)| j).collect(),
                values: entries.iter().map(|&(_, v)| v).collect(),
                lower: rhs,
            });
        }
        cuts
    }

    fn timeout(&self, iterations: u64) -> LpSolution {
        LpSolution {
            status: LpStatus::TimeLimit,
            iterations,
            stats: LpStats {
                solves: 1,
                phase1_iterations: self.phase1_iterations,
                phase2_iterations: iterations.saturating_sub(self.phase1_iterations),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn finish(&self, status: LpStatus, iterations: u64) -> LpSolution {
        if status == LpStatus::TimeLimit {
            return self.timeout(iterations);
        }
        let mut col_value = vec![0.0f64; self.n];
        for j in 0..self.n {
            let v = self.x[j] * self.lpr.col_scale[j];
            col_value[j] = v.clamp(
                if self.lpr.col_lo[j].is_finite() {
                    self.lpr.col_lo[j]
                } else {
                    -INF
                },
                if self.lpr.col_hi[j].is_finite() {
                    self.lpr.col_hi[j]
                } else {
                    INF
                },
            );
        }
        // Objective must use the (column-scaled) cost with the (scaled) variable
        // `c'_j x'_j == c_j x_j`.
        let objective: f64 = self
            .lpr
            .cost_true
            .iter()
            .enumerate()
            .map(|(j, c)| c * self.x[j])
            .sum();
        // A primal LP objective is an upper bound on the relaxation, and may
        // carry residual/perturbation error. Use the Lagrangian minimum instead:
        // min_{lo<=z<=hi} (c - A^T y) z, with A=[structural, -I]. Any finite y
        // gives a lower bound, even when reduced costs are slightly nonzero.
        let mut y = vec![0.0; self.m];
        for (k, &j) in self.basis.iter().enumerate() {
            y[k] = if j < self.n {
                self.lpr.cost_true[j]
            } else {
                0.0
            };
        }
        self.btran(&mut y);
        // Evaluate the Lagrangian minimum term by term. At an optimal basis the
        // reduced cost of a basic variable is zero, so its box minimum is zero.
        // A nonbasic variable sits at one of its finite bounds, and the simplex
        // termination criterion fixes the sign of its true reduced cost, so the
        // minimising endpoint is that finite bound. This keeps the bound finite
        // on models with one-sided infinite bounds, where taking the minimum over
        // the whole box would otherwise pick the infinite endpoint of a reduced
        // cost that merely rounds to the wrong side of zero.
        let mut dual = 0.0;
        let mut magnitude = 0.0;
        for j in 0..self.n + self.m {
            let cost = if j < self.n {
                self.lpr.cost_true[j]
            } else {
                0.0
            };
            let mut rc = cost;
            let mut dot_magnitude = cost.abs();
            let mut operations = 1usize;
            if j < self.n {
                for k in self.lpr.csc_start[j] as usize..self.lpr.csc_start[j + 1] as usize {
                    let product = self.lpr.csc_value[k] * y[self.lpr.csc_index[k] as usize];
                    rc -= product;
                    dot_magnitude += product.abs();
                    operations += 2;
                }
            } else {
                rc += y[j - self.n];
                dot_magnitude += y[j - self.n].abs();
            }
            // Conservative interval for the reduced cost.
            let error = 8.0 * f64::EPSILON * operations as f64 * dot_magnitude;
            let (c_lo, c_hi) = (rc - error, rc + error);
            let lo = self.lo[j];
            let hi = self.hi[j];
            let term = if self.in_basis[j] >= 0 {
                // Basic: the basis dual makes this reduced cost zero, so the box
                // minimum is exactly zero, finite even for one-sided unbounded
                // columns.
                0.0
            } else if lo.is_finite() && hi.is_finite() {
                // Both endpoints finite: the classic exact box minimum. Taking the
                // worse sign for each candidate stays a valid lower bound.
                let mut t = f64::INFINITY;
                for coefficient in [c_lo, c_hi] {
                    let bound = if coefficient >= 0.0 { lo } else { hi };
                    let candidate = if coefficient == 0.0 {
                        0.0
                    } else {
                        coefficient * bound
                    };
                    t = t.min(candidate);
                }
                t
            } else if hi.is_finite() {
                // Unbounded below (lo = -inf): a nonbasic vertex sits at the finite
                // upper bound with a true reduced cost <= 0. Using the negative
                // side of the interval keeps the term finite and valid.
                let r = if hi >= 0.0 {
                    c_lo.min(0.0)
                } else {
                    c_hi.min(0.0)
                };
                r * hi
            } else if lo.is_finite() {
                // Unbounded above (hi = +inf): a nonbasic vertex sits at the finite
                // lower bound with a true reduced cost >= 0.
                let r = if lo >= 0.0 {
                    c_lo.max(0.0)
                } else {
                    c_hi.max(0.0)
                };
                r * lo
            } else {
                // Free nonbasic (only reachable through degeneracy): its reduced
                // cost must be zero at a vertex.
                0.0
            };
            dual += term;
            magnitude += term.abs();
        }
        // Guard the final sum against floating-point accumulation error. This
        // is a numerical solver bound, not an exact rational proof certificate.
        let dual_bound = if status == LpStatus::Optimal && dual.is_finite() {
            Some(dual - 64.0 * f64::EPSILON * (1.0 + magnitude) * (self.n + self.m + 1) as f64)
        } else {
            None
        };
        LpSolution {
            status,
            objective,
            dual_bound,
            col_value,
            col_dual: Vec::new(),
            row_dual: Vec::new(),
            iterations,
            basis: if status == LpStatus::Optimal && self.basis.iter().all(|&j| j < self.n + self.m)
            {
                Some(std::sync::Arc::new(LpBasis {
                    basis: self.basis.clone(),
                    x: self.x[..self.n + self.m].to_vec(),
                }))
            } else {
                None
            },
            stats: LpStats {
                solves: 1,
                phase1_iterations: self.phase1_iterations,
                phase2_iterations: iterations.saturating_sub(self.phase1_iterations),
                ..Default::default()
            },
            cuts: if status == LpStatus::Optimal && self.generate_cuts {
                self.gmi_cuts(MAX_CUTS)
            } else {
                Vec::new()
            },
        }
    }

    fn optimize(
        &mut self,
        opts: &LpOptions,
        clock: &dyn Clock,
        start: f64,
        iters: &mut u64,
    ) -> LpStatus {
        let ptol = opts.primal_tol;
        let dtol = opts.dual_tol;
        let mut degenerate = 0u64;
        #[cfg(not(target_arch = "wasm32"))]
        let dbg = std::env::var("OPTIO_LPDEBUG").is_ok();
        #[cfg(target_arch = "wasm32")]
        let dbg = false;
        let mut banned: Vec<usize> = Vec::new();
        let mut y = vec![0.0; self.m];
        let mut d = vec![0.0; self.m];

        loop {
            if *iters >= opts.max_iterations {
                return LpStatus::IterationLimit;
            }
            if clock.now_secs() - start >= opts.time_limit_secs {
                return LpStatus::TimeLimit;
            }
            if dbg && *iters > 0 && *iters % 1000 == 0 {
                let mut p1 = 0.0f64;
                for r in 0..self.m {
                    p1 += self.x[art_of(self.n, self.m, r)].max(0.0);
                }
                let mut objt = 0.0f64;
                for j in 0..self.n {
                    objt += self.lpr.cost_true[j] * self.x[j];
                }
                eprintln!(
                    "[lp] progress it={} phase={} degeneracy={} phase1_infeas={:.6} obj={:.6}",
                    *iters, self.phase, degenerate, p1, objt
                );
            }

            // Duals: B^T y = c_B.
            #[cfg(feature = "profiling")]
            let _t1 = std::time::Instant::now();
            y.fill(0.0);
            for k in 0..self.m {
                y[k] = self.cost[self.basis[k]];
            }
            self.btran(&mut y);
            #[cfg(feature = "profiling")]
            {
                self.prof[1] += _t1.elapsed().as_secs_f64();
            }

            // Pricing (Dantzig; Bland on prolonged degeneracy).
            #[cfg(feature = "profiling")]
            let _t2 = std::time::Instant::now();
            let bland = degenerate > 100;
            let (enter, dir) = match self.price(&y, ptol, dtol, bland, &banned) {
                Some(e) => e,
                None => {
                    // No acceptable improving column remains. If some were
                    // rejected as numerically dependent, the accumulated eta file
                    // may simply be inaccurate: refactor and re-price once before
                    // declaring a numerical failure. A fresh factorization of the
                    // same basis often recovers valid pivots that the stale LU
                    // (and its etas) hid.
                    if banned.is_empty() {
                        return LpStatus::Optimal;
                    }
                    banned.clear();
                    if !self.refactorize(clock, start + opts.time_limit_secs) {
                        return LpStatus::TimeLimit;
                    }
                    match self.price(&y, ptol, dtol, bland, &banned) {
                        Some(e) => e,
                        None => {
                            return if banned.is_empty() {
                                LpStatus::Optimal
                            } else {
                                LpStatus::Numerical
                            }
                        }
                    }
                }
            };
            #[cfg(feature = "profiling")]
            {
                self.prof[2] += _t2.elapsed().as_secs_f64();
            }

            // Direction of movement.
            #[cfg(feature = "profiling")]
            let _t3 = std::time::Instant::now();
            self.fill_column(enter, &mut d);
            self.ftran(&mut d);
            #[cfg(feature = "profiling")]
            {
                self.prof[3] += _t3.elapsed().as_secs_f64();
            }

            #[cfg(feature = "profiling")]
            let _t4 = std::time::Instant::now();
            // Harris two-pass ratio test: pass 1 finds the relaxed minimum step,
            // pass 2 picks the largest (most stable) pivot among near-ties.
            let mut t_bound = f64::INFINITY;
            if dir > 0.0 {
                if self.hi[enter].is_finite() {
                    t_bound = (self.hi[enter] - self.x[enter]).max(0.0);
                }
            } else if self.lo[enter].is_finite() {
                t_bound = (self.x[enter] - self.lo[enter]).max(0.0);
            }
            let dthr = 1e-10;
            let mut t_relaxed = t_bound;
            for i in 0..self.m {
                let coef = -d[i] * dir;
                let bi = self.basis[i];
                if coef > dthr {
                    if self.hi[bi].is_finite() {
                        let r = (self.hi[bi] + ptol - self.x[bi]) / coef;
                        if r < t_relaxed {
                            t_relaxed = r.max(0.0);
                        }
                    }
                } else if coef < -dthr && self.lo[bi].is_finite() {
                    let r = (self.x[bi] + ptol - self.lo[bi]) / (-coef);
                    if r < t_relaxed {
                        t_relaxed = r.max(0.0);
                    }
                }
            }
            // Step size is the former of the entering bound and the relaxed
            // blocking step; a leaving variable must actually reach it.
            let t = t_relaxed.min(t_bound);
            let mut leave: Option<usize> = None;
            let mut best_pivot = 0.0f64;
            // Bland's rule needs *both* halves to guarantee termination: the
            // smallest-index entering column (done in `price`) and the
            // smallest-index leaving variable among the minimum-ratio ties.
            // The stability heuristic below picks the largest pivot instead, which
            // lets a degenerate LP cycle forever (observed as ~400k no-progress
            // iterations in phase 2).
            let mut bland_leave_var = usize::MAX;
            for i in 0..self.m {
                let coef = -d[i] * dir;
                let bi = self.basis[i];
                let (room, mag) = if coef > 0.0 {
                    if !self.hi[bi].is_finite() {
                        continue;
                    }
                    (self.hi[bi] - self.x[bi], coef)
                } else if coef < 0.0 {
                    if !self.lo[bi].is_finite() {
                        continue;
                    }
                    (self.x[bi] - self.lo[bi], -coef)
                } else {
                    continue;
                };
                if mag < dthr {
                    continue;
                }
                let ratio = (room / mag).max(0.0);
                if ratio <= t + 1e-9 {
                    if bland {
                        if bi < bland_leave_var {
                            bland_leave_var = bi;
                            leave = Some(i);
                        }
                    } else if mag > best_pivot {
                        best_pivot = mag;
                        leave = Some(i);
                    }
                }
            }
            // Reject a numerically unusable pivot: the entering column is then
            // (near-)dependent on the basis, so ban it and re-price rather than
            // pivot into a singular basis.
            let dmax = d.iter().fold(0.0f64, |m, &v| m.max(v.abs()));
            if let Some(p) = leave {
                if d[p].abs() < 1e-10 * dmax.max(1e-12) {
                    banned.push(enter);
                    continue;
                }
            }

            if !t.is_finite() {
                return LpStatus::Unbounded;
            }
            if dbg {
                eprintln!(
                    "[lp] it={} enter={} dir={:+.0} t={:.4} leave={:?}",
                    *iters, enter, dir, t, leave
                );
            }

            // Apply the step (exact: do NOT clamp basics; that would break the
            // linear relation and hide infeasibility).
            self.x[enter] += dir * t;
            for i in 0..self.m {
                let bi = self.basis[i];
                self.x[bi] -= d[i] * dir * t;
            }

            match leave {
                None => {
                    self.x[enter] = clamp_bound(self.x[enter], self.lo[enter], self.hi[enter]);
                }
                Some(p) => {
                    let leaving = self.basis[p];
                    let dp = d[p];
                    self.basis[p] = enter;
                    self.in_basis[leaving] = -1;
                    self.in_basis[enter] = p as i32;
                    self.x[leaving] =
                        clamp_bound(self.x[leaving], self.lo[leaving], self.hi[leaving]);
                    self.push_eta(p, &d, dp);
                }
            }
            banned.clear();

            // Refactor on an eta-count cap or once the eta fill (total nonzero
            // updates) grows large enough to outweigh a fresh factorization. The
            // factor is cheap now (output-sensitive), so both caps are small:
            // short eta files keep `apply_etas` streaming.
            if self.eta_p.len() >= opts.refactor_interval {
                if !self.refactorize(clock, start + opts.time_limit_secs) {
                    return LpStatus::TimeLimit;
                }
            }
            #[cfg(feature = "profiling")]
            {
                self.prof[4] += _t4.elapsed().as_secs_f64();
            }

            if t <= ptol {
                degenerate += 1;
            } else {
                degenerate = 0;
            }
            *iters += 1;
        }
    }

    /// Choose an entering variable. Returns `(var, +1|-1)`.
    fn price(
        &mut self,
        y: &[f64],
        _ptol: f64,
        dtol: f64,
        bland: bool,
        banned: &[usize],
    ) -> Option<(usize, f64)> {
        let mut best: Option<(usize, f64)> = None;
        let mut best_mag = 0.0f64;

        let consider =
            |var: usize, rc: f64, best: &mut Option<(usize, f64)>, best_mag: &mut f64| {
                // Fixed variables (including phase-2 artificials) can never move.
                if self.hi[var] - self.lo[var] <= 1e-12 {
                    return;
                }
                if banned.contains(&var) {
                    return;
                }
                // A narrow box can be within tolerance of BOTH bounds. Price
                // only directions with actual room, otherwise a bound flip at
                // the upper bound repeatedly selects a zero-length upward step.
                let can_up = self.hi[var] - self.x[var] > 1e-12;
                let can_down = self.x[var] - self.lo[var] > 1e-12;
                if rc < -dtol && can_up && (bland || -rc > *best_mag) {
                    *best_mag = -rc;
                    *best = Some((var, 1.0));
                } else if rc > dtol && can_down && (bland || rc > *best_mag) {
                    *best_mag = rc;
                    *best = Some((var, -1.0));
                }
                if bland && best.is_some() {
                    // Bland: stop at the first (smallest index) candidate.
                }
            };

        // Structural columns: partial pricing over a window that advances each
        // time it finds no improving column, so per-iteration pricing cost is a
        // fraction of the full pass. Bland keeps a full index-ordered sweep.
        let n = self.n;
        let mut new_pos = self.price_pos;
        // Partial pricing only pays off on larger LPs, and more aggressively so
        // the larger they get; tiny LPs price fully (extra iterations there
        // outweigh the cheaper pass).
        let window = if bland || n < 2000 {
            n.max(1)
        } else if n < 6000 {
            (n / 2).max(256).min(n.max(1))
        } else {
            (n / 4).max(256).min(n.max(1))
        };
        let mut start = if bland { 0 } else { self.price_pos % n.max(1) };
        let mut covered = 0usize;
        while covered < n && !(bland && best.is_some()) {
            let hi = (start + window).min(n);
            for j in start..hi {
                if self.in_basis[j] >= 0 {
                    continue;
                }
                let mut rc = self.cost[j];
                for (r, a) in self.lpr.col(j) {
                    rc -= y[r] * a;
                }
                consider(j, rc, &mut best, &mut best_mag);
                if bland && best.is_some() {
                    break;
                }
            }
            covered += hi - start;
            if best.is_some() {
                new_pos = hi % n.max(1);
                break;
            }
            if hi >= n {
                start = 0;
            } else {
                start = hi;
            }
        }
        // Logical columns (cost 0, column -e_r -> rc = y[r]).
        for r in 0..self.m {
            let var = log_of(self.n, r);
            if self.in_basis[var] >= 0 {
                continue;
            }
            let rc = y[r];
            consider(var, rc, &mut best, &mut best_mag);
            if bland && best.is_some() {
                return best;
            }
        }
        // Artificial columns (phase 1 only, cost 1, column art_sign * e_r).
        for r in 0..self.m {
            if self.art_sign[r] == 0.0 {
                continue;
            }
            let var = art_of(self.n, self.m, r);
            if self.in_basis[var] >= 0 {
                continue;
            }
            let rc = self.cost[var] - y[r] * self.art_sign[r];
            consider(var, rc, &mut best, &mut best_mag);
            if bland && best.is_some() {
                return best;
            }
        }
        self.price_pos = new_pos % n.max(1);
        best
    }
}

fn opts_refactor_interval(_s: &Simplex) -> usize {
    128
}

#[inline]
fn clamp0(l: f64, u: f64) -> f64 {
    if l > 0.0 {
        l
    } else if u < 0.0 {
        u
    } else {
        0.0
    }
}

#[inline]
fn clamp_bound(v: f64, l: f64, u: f64) -> f64 {
    if l.is_finite() && v < l {
        l
    } else if u.is_finite() && v > u {
        u
    } else {
        v
    }
}

// keep the unused placeholder constant referenced (avoids dead-code warnings)
const _: usize = LOG_OFF;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::VarType;

    /// Dense reference solve of `A x = b` via Gauss-Jordan (for LU tests).
    fn dense_solve(a: &[Vec<f64>], b: &[f64]) -> Vec<f64> {
        let n = b.len();
        let mut m: Vec<Vec<f64>> = a.to_vec();
        let mut x = b.to_vec();
        for k in 0..n {
            let mut p = k;
            for i in k + 1..n {
                if m[i][k].abs() > m[p][k].abs() {
                    p = i;
                }
            }
            m.swap(k, p);
            x.swap(k, p);
            let d = m[k][k];
            for i in 0..n {
                m[k][i] /= d;
            }
            x[k] /= d;
            for i in 0..n {
                if i != k && m[i][k] != 0.0 {
                    let f = m[i][k];
                    for j in 0..n {
                        m[i][j] -= f * m[k][j];
                    }
                    x[i] -= f * x[k];
                }
            }
        }
        x
    }

    fn dense_cols(a: &[Vec<f64>]) -> Vec<Vec<(u32, f64)>> {
        let n = a.len();
        (0..n)
            .map(|j| {
                (0..n)
                    .filter(|&i| a[i][j] != 0.0)
                    .map(|i| (i as u32, a[i][j]))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn sparse_lu_ftran_btran_match_dense() {
        // A well-conditioned, non-tridiagonal invertible matrix.
        let a: Vec<Vec<f64>> = vec![
            vec![4.0, 1.0, 0.0, 2.0, 0.0, 0.0],
            vec![1.0, 5.0, 2.0, 0.0, 1.0, 0.0],
            vec![0.0, 2.0, 6.0, 1.0, 0.0, 1.0],
            vec![2.0, 0.0, 1.0, 7.0, 2.0, 0.0],
            vec![0.0, 1.0, 0.0, 2.0, 8.0, 1.0],
            vec![0.0, 0.0, 1.0, 0.0, 1.0, 9.0],
        ];
        let n = 6;
        let cols = dense_cols(&a);
        let lu = SparseLu::factor(&cols, n, true, None).unwrap();
        // Residual checks against the dense basis matrix B (not its inverse).
        for t in 0..n {
            let mut b = vec![0.0; n];
            b[t] = 1.0;
            let mut x = b.clone();
            lu.ftran(&mut x);
            for i in 0..n {
                let mut lhs = 0.0;
                for j in 0..n {
                    lhs += a[i][j] * x[j];
                }
                assert!((lhs - b[i]).abs() < 1e-9, "ftran resid row {i}: {lhs}");
            }
            // B^T y = c  <=>  for each column j: sum_i B[i][j] y[i] = c[j].
            let mut y = b.clone();
            lu.btran(&mut y);
            for j in 0..n {
                let mut lhs = 0.0;
                for i in 0..n {
                    lhs += a[i][j] * y[i];
                }
                assert!((lhs - b[j]).abs() < 1e-9, "btran resid col {j}: {lhs}");
            }
        }
    }

    fn solve_model(m: &Model) -> LpSolution {
        let mut lpr = LpRelaxation::build(m);
        lpr.solve(&LpOptions::default(), &crate::solve::StdClock::new())
    }

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn range(&mut self, lo: i64, hi: i64) -> i64 {
            lo + (self.next() % ((hi - lo + 1) as u64)) as i64
        }
    }

    /// Every generated cut must be satisfied by every *feasible* integer point.
    /// All-integer models, so feasibility can be enumerated exactly.
    #[test]
    fn gmi_cuts_are_valid_on_random_integer_models() {
        let clock = crate::solve::StdClock::new();
        let mut rng = Lcg(0xC0FFEE);
        let mut checked = 0usize;
        for _case in 0..400 {
            let n = rng.range(3, 5) as usize;
            let mut m = Model::new();
            let mut ranges = Vec::new();
            for _ in 0..n {
                let hi = rng.range(1, 3) as f64;
                let c = m.add_col(0.0, hi);
                m.set_integrality(c, crate::model::VarType::Integer);
                m.set_cost(c, rng.range(-3, 3) as f64);
                ranges.push(hi as i64);
            }
            for _ in 0..rng.range(2, 4) {
                let entries: Vec<(u32, f64)> = (0..n)
                    .map(|j| (j as u32, rng.range(-3, 3) as f64))
                    .collect();
                match rng.next() % 3 {
                    0 => {
                        let b = rng.range(0, 6) as f64;
                        m.add_row(b, b, &entries);
                    }
                    1 => {
                        m.add_row(-INF, rng.range(0, 8) as f64, &entries);
                    }
                    _ => {
                        m.add_row(rng.range(0, 8) as f64, INF, &entries);
                    }
                }
            }
            let opts = LpOptions {
                generate_cuts: true,
                ..LpOptions::default()
            };
            let mut lpr = LpRelaxation::build(&m);
            let sol = lpr.solve(&opts, &clock);
            if sol.status != LpStatus::Optimal || sol.cuts.is_empty() {
                continue;
            }
            // Enumerate every integer point in the box.
            let mut x = vec![0i64; n];
            loop {
                let xf: Vec<f64> = x.iter().map(|&v| v as f64).collect();
                if crate::model::check_solution(&m, &xf, 1e-9).feasible {
                    for cut in &sol.cuts {
                        let mut lhs = 0.0;
                        for (k, &j) in cut.indices.iter().enumerate() {
                            lhs += cut.values[k] * xf[j as usize];
                        }
                        assert!(
                            lhs >= cut.lower - 1e-7,
                            "cut {cut:?} violated by feasible integer point {xf:?} (lhs {lhs})",
                        );
                        checked += 1;
                    }
                }
                let mut k = 0;
                while k < n {
                    x[k] += 1;
                    if x[k] <= ranges[k] {
                        break;
                    }
                    x[k] = 0;
                    k += 1;
                }
                if k == n {
                    break;
                }
            }
        }
        assert!(checked > 0, "no cuts were exercised");
    }

    /// Same validity property with continuous columns mixed in, checked against a
    /// planted feasible point.
    #[test]
    fn gmi_cuts_are_valid_on_random_mixed_models() {
        let clock = crate::solve::StdClock::new();
        let mut rng = Lcg(0xBEEF);
        let mut checked = 0usize;
        for _case in 0..400 {
            let n_int = rng.range(2, 4) as usize;
            let n_cont = rng.range(1, 3) as usize;
            let n = n_int + n_cont;
            let mut m = Model::new();
            for j in 0..n {
                let (lo, hi) = if j < n_int { (0.0, 3.0) } else { (-10.0, 10.0) };
                let c = m.add_col(lo, hi);
                m.set_cost(c, rng.range(-3, 3) as f64);
                if j < n_int {
                    m.set_integrality(c, crate::model::VarType::Integer);
                }
            }
            // Planted feasible point.
            let xstar: Vec<f64> = (0..n)
                .map(|j| {
                    if j < n_int {
                        rng.range(0, 3) as f64
                    } else {
                        rng.range(-4, 4) as f64
                    }
                })
                .collect();
            for _ in 0..rng.range(2, 5) {
                let entries: Vec<(u32, f64)> = (0..n)
                    .map(|j| (j as u32, rng.range(-3, 3) as f64))
                    .collect();
                let act: f64 = entries.iter().map(|&(j, a)| a * xstar[j as usize]).sum();
                match rng.next() % 3 {
                    0 => {
                        m.add_row(act, act, &entries);
                    }
                    1 => {
                        m.add_row(-INF, act + rng.range(0, 4) as f64, &entries);
                    }
                    _ => {
                        m.add_row(act - rng.range(0, 4) as f64, INF, &entries);
                    }
                }
            }
            assert!(crate::model::check_solution(&m, &xstar, 1e-9).feasible);
            let opts = LpOptions {
                generate_cuts: true,
                ..LpOptions::default()
            };
            let mut lpr = LpRelaxation::build(&m);
            let sol = lpr.solve(&opts, &clock);
            if sol.status != LpStatus::Optimal {
                continue;
            }
            for cut in &sol.cuts {
                let mut lhs = 0.0;
                for (k, &j) in cut.indices.iter().enumerate() {
                    lhs += cut.values[k] * xstar[j as usize];
                }
                assert!(
                    lhs >= cut.lower - 1e-7,
                    "cut {cut:?} violated by planted feasible point {xstar:?} (lhs {lhs})",
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no cuts were exercised");
    }

    #[test]
    fn unbounded_columns_still_yield_a_finite_dual_bound() {
        // Both columns have no finite upper bound, so the naive box minimum would
        // take the unbounded endpoint of any reduced cost that rounds to the wrong
        // side of zero and report no bound at all. The basis-status evaluation must
        // keep it finite and valid for the relaxation.
        let mut m = Model::new();
        let x = m.add_col(0.0, INF);
        let y = m.add_col(0.0, INF);
        m.set_cost(x, 1.0);
        m.set_cost(y, 2.0);
        m.add_row(3.0, INF, &[(x.0, 1.0), (y.0, 1.0)]);
        let s = solve_model(&m);
        assert_eq!(s.status, LpStatus::Optimal);
        assert!((s.objective - 3.0).abs() < 1e-6);
        let bound = s
            .dual_bound
            .expect("a finite bound is required even with unbounded columns");
        assert!(
            bound <= s.objective + 1e-6,
            "dual bound {bound} exceeds objective {}",
            s.objective
        );
    }

    #[test]
    fn child_boxes_reoptimize_without_phase_one() {
        let mut m = Model::new();
        let x = m.add_col(0.0, 4.0);
        let y = m.add_col(0.0, 4.0);
        m.set_cost(x, 1.0);
        m.set_cost(y, 2.0);
        m.add_row(3.5, INF, &[(x.0, 1.0), (y.0, 1.0)]);
        let opts = LpOptions::default();
        let clock = crate::solve::StdClock::new();
        let mut lpr = LpRelaxation::build(&m);
        let parent = lpr.solve(&opts, &clock);
        let basis = parent.basis.as_ref().unwrap();
        for (lo, hi) in [
            (vec![0.0, 0.0], vec![3.0, 4.0]),
            (vec![4.0, 0.0], vec![4.0, 4.0]),
            (vec![0.0, 1.0], vec![4.0, 4.0]),
            (vec![0.0, 0.0], vec![1.0, 1.0]),
        ] {
            let warm = lpr.solve_with_basis(&lo, &hi, &opts, &clock, Some(basis));
            let cold = lpr.solve_with_bounds(&lo, &hi, &opts, &clock);
            assert_eq!(warm.status, cold.status);
            assert_eq!(warm.stats.warm_fallbacks, 0);
            assert_eq!(warm.stats.phase1_iterations, 0);
            if warm.status == LpStatus::Optimal {
                assert!(
                    (warm.objective - cold.objective).abs() < 1e-6,
                    "lo={lo:?} hi={hi:?} warm={warm:?} cold={cold:?}"
                );
                assert!(respects_bounds(&warm.col_value, &lo, &hi));
                assert!(warm.dual_bound.unwrap() <= warm.objective + 1e-7);
            }
        }
    }

    #[test]
    fn warm_repair_restores_nonbasic_endpoint() {
        let mut m = Model::new();
        let x = m.add_col(0.0, 1.0);
        let y = m.add_col(0.0, 1.0);
        m.set_cost(x, 1.0);
        m.set_cost(y, 2.0);
        m.add_row(1.0, 1.0, &[(x.0, 1.0), (y.0, 1.0)]);
        let opts = LpOptions::default();
        let clock = crate::solve::StdClock::new();
        let mut lpr = LpRelaxation::build(&m);
        let parent = lpr.solve(&opts, &clock);
        let mut saved = (**parent.basis.as_ref().unwrap()).clone();
        assert!(!saved.basis.contains(&(y.0 as usize)));
        // An endpoint inconsistent with reduced costs used to discard the
        // entire basis. The feasible endpoint is recoverable without Phase I.
        saved.x[y.0 as usize] = 1.0 / lpr.col_scale[y.0 as usize];
        let repaired = lpr.solve_with_basis(&[0.0, 0.0], &[1.0, 1.0], &opts, &clock, Some(&saved));
        assert_eq!(repaired.status, LpStatus::Optimal);
        assert!((repaired.objective - 1.0).abs() < 1e-7);
        assert_eq!(repaired.stats.warm_bound_flips, 1);
        assert_eq!(repaired.stats.warm_fallbacks, 0);
        assert_eq!(repaired.stats.phase1_iterations, 0);
    }

    #[test]
    fn cold_simplex_checks_deadline_after_each_pivot() {
        struct AdvancingClock(std::sync::atomic::AtomicU64);
        impl Clock for AdvancingClock {
            fn now_secs(&self) -> f64 {
                2.0 * self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as f64
            }
        }
        let mut m = Model::new();
        let x = m.add_col(0.0, 4.0);
        let y = m.add_col(0.0, 4.0);
        m.set_cost(x, 1.0);
        m.set_cost(y, 2.0);
        m.add_row(3.5, INF, &[(x.0, 1.0), (y.0, 1.0)]);
        let lpr = LpRelaxation::build(&m);
        let opts = LpOptions {
            time_limit_secs: 1.0,
            ..Default::default()
        };
        let mut simplex = Simplex::new(&lpr, &[0.0, 0.0], &[4.0, 4.0], &opts);
        assert!(simplex.initialize(&crate::solve::StdClock::new(), f64::INFINITY));
        simplex.set_phase2();
        let mut iterations = 0;
        let clock = AdvancingClock(std::sync::atomic::AtomicU64::new(0));
        assert_eq!(
            simplex.optimize(&opts, &clock, 0.0, &mut iterations),
            LpStatus::TimeLimit
        );
        assert_eq!(iterations, 1);
    }

    #[test]
    fn factorization_cancels_without_publishing_partial_lu() {
        struct AdvancingClock(std::sync::atomic::AtomicU64);
        impl Clock for AdvancingClock {
            fn now_secs(&self) -> f64 {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as f64
            }
        }
        let cols: Vec<Vec<(u32, f64)>> = (0..128).map(|i| vec![(i, 1.0)]).collect();
        for sparse in [false, true] {
            let clock = AdvancingClock(std::sync::atomic::AtomicU64::new(0));
            assert!(SparseLu::factor(&cols, 128, sparse, Some((&clock, 2.0))).is_none());
            assert!(clock.0.load(std::sync::atomic::Ordering::Relaxed) <= 3);
            let completed = SparseLu::factor(&cols, 128, sparse, None).unwrap();
            assert!(!completed.singular);
        }
    }

    #[test]
    fn cancelled_initialization_and_restore_have_no_certificate() {
        let mut m = Model::new();
        let x = m.add_col(0.0, 1.0);
        m.set_cost(x, 1.0);
        m.add_row(0.5, INF, &[(x.0, 1.0)]);
        let clock = crate::solve::StdClock::new();
        let mut lpr = LpRelaxation::build(&m);
        let parent = lpr.solve(&LpOptions::default(), &clock);
        let opts = LpOptions {
            time_limit_secs: 0.0,
            ..Default::default()
        };
        for basis in [None, parent.basis.as_deref()] {
            let cancelled = lpr.solve_with_basis(&[0.0], &[1.0], &opts, &clock, basis);
            assert_eq!(cancelled.status, LpStatus::TimeLimit);
            assert!(cancelled.dual_bound.is_none());
            assert!(cancelled.basis.is_none());
            assert!(cancelled.col_value.is_empty());
            assert_eq!(cancelled.stats.warm_fallbacks, 0);
        }
    }

    #[test]
    fn invalid_warm_basis_is_counted_and_safely_rebuilt() {
        let mut m = Model::new();
        let x = m.add_col(0.0, 1.0);
        m.set_cost(x, 1.0);
        m.add_row(0.5, INF, &[(x.0, 1.0)]);
        let opts = LpOptions::default();
        let clock = crate::solve::StdClock::new();
        let mut lpr = LpRelaxation::build(&m);
        let bad = LpBasis {
            basis: Vec::new(),
            x: Vec::new(),
        };
        let solved = lpr.solve_with_basis(&[0.0], &[1.0], &opts, &clock, Some(&bad));
        assert_eq!(solved.status, LpStatus::Optimal);
        assert_eq!(solved.stats.warm_fallbacks, 1);
        assert_eq!(solved.stats.warm_invalid_basis, 1);
    }

    #[test]
    fn sparse_lu_random_01() {
        // Random 40x40 0/1 matrix; dense reference residual for ftran/btran.
        let n = 40usize;
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut rnd = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut a = vec![vec![0.0f64; n]; n];
        for i in 0..n {
            for j in 0..n {
                a[i][j] = if rnd() < 0.15 { 1.0 } else { 0.0 };
            }
            a[i][i] += 1.0; // keep it nonsingular (diagonal dominance-ish)
        }
        let cols = dense_cols(&a);
        // Exercise the dense (pivot-scan) elimination here.
        let lu = SparseLu::factor(&cols, n, false, None).unwrap();
        assert!(!lu.singular, "unexpected singular");
        for t in 0..n {
            let mut b = vec![0.0; n];
            b[t] = 1.0;
            let mut x = b.clone();
            lu.ftran(&mut x);
            for i in 0..n {
                let mut lhs = 0.0;
                for j in 0..n {
                    lhs += a[i][j] * x[j];
                }
                assert!((lhs - b[i]).abs() < 1e-7, "ftran resid row {i}: {lhs}");
            }
            let mut y = b.clone();
            lu.btran(&mut y);
            for j in 0..n {
                let mut lhs = 0.0;
                for i in 0..n {
                    lhs += a[i][j] * y[i];
                }
                assert!((lhs - b[j]).abs() < 1e-7, "btran resid col {j}: {lhs}");
            }
        }
    }

    #[test]
    fn simple_lp_optimum() {
        // min -x - 10y  s.t. x + 7y <= 17.5, 0<=x<=3.5, y>=0.
        // LP optimum: (0, 2.5) -> -25.
        let mut m = Model::new();
        let x = m.add_col(0.0, 3.5);
        let y = m.add_col(0.0, INF);
        m.set_cost(x, -1.0);
        m.set_cost(y, -10.0);
        m.add_row(-INF, 17.5, &[(x.0, 1.0), (y.0, 7.0)]);
        let s = solve_model(&m);
        assert_eq!(s.status, LpStatus::Optimal, "{:?}", s.status);
        assert!((s.objective - -25.0).abs() < 1e-6, "obj {}", s.objective);
    }

    #[test]
    fn equality_and_bounds() {
        // min x + y  s.t. x + y = 4, 0<=x<=3, 0<=y<=3 -> 4.
        let mut m = Model::new();
        let x = m.add_col(0.0, 3.0);
        let y = m.add_col(0.0, 3.0);
        m.set_cost(x, 1.0);
        m.set_cost(y, 1.0);
        m.add_row(4.0, 4.0, &[(x.0, 1.0), (y.0, 1.0)]);
        let s = solve_model(&m);
        assert_eq!(s.status, LpStatus::Optimal);
        assert!((s.objective - 4.0).abs() < 1e-6);
    }

    #[test]
    fn detects_infeasible() {
        // x >= 5 and x <= 3.
        let mut m = Model::new();
        let x = m.add_col(0.0, 3.0);
        m.set_cost(x, 1.0);
        m.add_row(5.0, INF, &[(x.0, 1.0)]);
        let s = solve_model(&m);
        assert_eq!(s.status, LpStatus::Infeasible);
    }

    #[test]
    fn detects_unbounded() {
        // min -x, x >= 0, no upper bound.
        let mut m = Model::new();
        let x = m.add_col(0.0, INF);
        m.set_cost(x, -1.0);
        m.add_row(0.0, INF, &[(x.0, 1.0), (x.0, 1.0)]); // degenerate duplicate row
        let s = solve_model(&m);
        assert_eq!(s.status, LpStatus::Unbounded);
        let _ = VarType::Continuous;
    }

    #[test]
    fn two_var_transport_like() {
        // min 2a + 3b + 4c + d  s.t.
        //   a + b       = 10
        //        c + d  = 10
        //   a + c <= 12, b + d <= 12, bounds [0,10].
        // Optimum a=10,d=10 -> 2*10 + 1*10 = 30.
        let mut m = Model::new();
        let a = m.add_col(0.0, 10.0);
        let b = m.add_col(0.0, 10.0);
        let c = m.add_col(0.0, 10.0);
        let d = m.add_col(0.0, 10.0);
        m.set_cost(a, 2.0);
        m.set_cost(b, 3.0);
        m.set_cost(c, 4.0);
        m.set_cost(d, 1.0);
        m.add_row(10.0, 10.0, &[(a.0, 1.0), (b.0, 1.0)]);
        m.add_row(10.0, 10.0, &[(c.0, 1.0), (d.0, 1.0)]);
        m.add_row(-INF, 12.0, &[(a.0, 1.0), (c.0, 1.0)]);
        m.add_row(-INF, 12.0, &[(b.0, 1.0), (d.0, 1.0)]);
        let s = solve_model(&m);
        assert_eq!(s.status, LpStatus::Optimal, "{:?}", s.status);
        assert!((s.objective - 30.0).abs() < 1e-6, "obj {}", s.objective);
    }
}
