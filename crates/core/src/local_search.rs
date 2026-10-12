//! Structure-aware repair / local search for coupled planning MILPs.
//!
//! The generic min-conflicts heuristic (`heuristic.rs`) moves a single variable
//! at a time. On the planning model that stalls on the coupled
//! "choose one ring per unit/session" (SOS1-like) `onesplit` rows and on the
//! per-(session,ring) load coupling: satisfying the last rows needs a *transfer*
//! (move a whole match from one ring to another, updating the linked `z` and the
//! session load/peak/deviation auxiliaries together), not a single-variable move.
//!
//! This module is the primal first-aid that runs before/with the LP relaxation:
//! it takes an almost-feasible point and tries to close the remaining violation
//! with transfer + swap moves. It must remain allocation-light and wasm-safe.
//!
//! Algorithm (structure-aware local search with exact deltas):
//!   1. Detect the planning shape without names: `tot` sum-rows (unit totals),
//!      `link` rows (`x - M z <= 0`), the per-(session,ring) `load` definition
//!      equalities, and the peak/min/range/total/deviation auxiliaries. Sessions
//!      are the groups of loads sharing one `peak`.
//!   2. Keep a residual `lhs` per row plus a violation vector. Every candidate
//!      move (single-variable create/drop, and the coupled *transfer* `x[r1]-=d;
//!      x[r2]+=d` with its `z` flips and the derived load/aux deltas) is scored
//!      by the *true* change in total violation over the union of touched rows,
//!      then applied greedily.
//!   3. If no move improves, a seeded random transfer kicks the search out of the
//!      plateau; a bounded stall counter guarantees termination.
//!   4. `repair` returns `Some` only after `check_solution` agrees; the search's
//!      own bookkeeping is never trusted.

use crate::model::{Model, VarType, INF};
use crate::solve::Clock;

const TOL: f64 = 1e-6;
const INF_HALF: f64 = INF * 0.5;

/// A column that behaves like a binary (integral with bounds within [0,1]) —
/// `highs-js` may report these as `Integer` rather than `Binary`.
#[inline]
fn is_binary_like(model: &Model, c: usize) -> bool {
    model.col_type[c].is_integral()
        && model.col_lower[c] >= -1e-9
        && model.col_upper[c] <= 1.0 + 1e-9
}

/// Constraint matrix in CSC (columns) form, as in `heuristic.rs`.
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
    fn rows(&self, c: usize) -> impl Iterator<Item = (usize, f64)> + '_ {
        let lo = self.start[c] as usize;
        let hi = self.start[c + 1] as usize;
        (lo..hi).map(move |k| (self.index[k] as usize, self.value[k]))
    }
}

/// Small seeded xorshift so the whole pass is deterministic for a fixed `seed`.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Rng {
        let s = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0x1234_5678_9ABC_DEF0);
        Rng(if s == 0 { 0xDEAD_BEEF_CAFE_1234 } else { s })
    }
    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    #[inline]
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    #[inline]
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

#[inline]
fn row_viol(model: &Model, r: usize, lhs: f64) -> f64 {
    (model.row_lower[r] - lhs)
        .max(lhs - model.row_upper[r])
        .max(0.0)
}

/// Detected planning structure. All entries are column indices (`usize`), with
/// `-1` sentinels carried as `i32` where "absent" is possible.
struct Struct {
    /// `x[u,s,r] -> unit` (index into `units`), from the `tot` equality rows.
    x_unit: Vec<i32>,
    /// `x ->` the continuous load it defines (`load - sum dur*x = 0`), or -1.
    x_load: Vec<i32>,
    /// `d(load)/d(x)` (i.e. the unit duration) for the load definition.
    x_dur: Vec<f64>,
    /// `x ->` its fixed-charge binary `z`, or -1.
    x_z: Vec<i32>,
    /// `x` columns per unit.
    units: Vec<Vec<usize>>,
    /// RHS (total matches) per unit.
    unit_sum: Vec<f64>,
    /// `load -> session`.
    load_session: Vec<i32>,
    /// `load` columns per session, in encounter order.
    session_loads: Vec<Vec<usize>>,
    /// Per-load auxiliary columns (the first load of a session is canonical).
    total: Vec<i32>,
    peak: Vec<i32>,
    minv: Vec<i32>,
    range: Vec<i32>,
    /// Finite upper bound on a load column, if any.
    cap: Vec<f64>,
    /// Deviation-definition rows as `(dev column, row)`.
    dev_rows: Vec<(usize, usize)>,
    /// Rows defining each deviation column.
    dev_rows_of_col: Vec<Vec<usize>>,
    /// Deviation columns per session.
    dev_of_session: Vec<Vec<usize>>,
    /// `load ->` the `x` columns feeding it.
    feeders: Vec<Vec<usize>>,
    /// True when enough structure was recognised to run the coupled search.
    detected: bool,
}

fn detect(model: &Model, csc: &Csc) -> Struct {
    let n = model.num_col;
    let m = model.num_row;
    let mut st = Struct {
        x_unit: vec![-1; n],
        x_load: vec![-1; n],
        x_dur: vec![0.0; n],
        x_z: vec![-1; n],
        units: Vec::new(),
        unit_sum: Vec::new(),
        load_session: vec![-1; n],
        session_loads: Vec::new(),
        total: vec![-1; n],
        peak: vec![-1; n],
        minv: vec![-1; n],
        range: vec![-1; n],
        cap: vec![INF; n],
        dev_rows: Vec::new(),
        dev_rows_of_col: Vec::new(),
        dev_of_session: Vec::new(),
        feeders: vec![Vec::new(); n],
        detected: false,
    };

    // 1. `tot` rows: equality, all columns integral, all coefficients +1.
    for r in 0..m {
        if model.row_lower[r] != model.row_upper[r] {
            continue;
        }
        let mut all_int = true;
        let mut all_one = true;
        let mut cnt = 0usize;
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            let c = model.a_index[k] as usize;
            if !model.col_type[c].is_integral() {
                all_int = false;
                break;
            }
            if (model.a_value[k] - 1.0).abs() > 1e-9 {
                all_one = false;
            }
            cnt += 1;
        }
        if all_int && all_one && cnt > 0 {
            let uid = st.units.len() as i32;
            let mut cols = Vec::with_capacity(cnt);
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                let c = model.a_index[k] as usize;
                if st.x_unit[c] < 0 {
                    st.x_unit[c] = uid;
                }
                cols.push(c);
            }
            st.units.push(cols);
            st.unit_sum.push(model.row_lower[r]);
        }
    }

    // 2. `link` rows: exactly two nonzeros `(x:+a, z:-b)` with `z` binary.
    for c in 0..n {
        if !model.col_type[c].is_integral() {
            continue;
        }
        for (r, a) in csc.rows(c) {
            if (model.a_start[r + 1] - model.a_start[r]) != 2
                || model.row_upper[r] > 0.0
                || model.row_lower[r] > -INF * 0.5
            {
                continue;
            }
            let mut zc = -1i32;
            let mut b = 0.0;
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                let cc = model.a_index[k] as usize;
                if cc != c {
                    zc = cc as i32;
                    b = model.a_value[k];
                }
            }
            if zc >= 0 && model.col_type[zc as usize] == VarType::Binary && a > 0.0 && b < 0.0 {
                st.x_z[c] = zc;
            }
        }
    }

    // 3. Load definition: equality with exactly one continuous and >=1 integer.
    for r in 0..m {
        if model.row_lower[r] != model.row_upper[r] {
            continue;
        }
        let mut cont_col = -1i32;
        let mut cont_a = 0.0;
        let mut ncont = 0usize;
        let mut ints: Vec<(usize, f64)> = Vec::new();
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            let c = model.a_index[k] as usize;
            let a = model.a_value[k];
            if model.col_type[c].is_integral() {
                ints.push((c, a));
            } else {
                ncont += 1;
                cont_col = c as i32;
                cont_a = a;
            }
        }
        if ncont == 1 && !ints.is_empty() && cont_a.abs() > 1e-12 {
            for (c, a) in ints {
                if st.x_load[c] < 0 {
                    st.x_load[c] = cont_col;
                    st.x_dur[c] = -a / cont_a;
                }
            }
        }
    }

    // 4. peak (`load - peak <= 0`) and min (`load - min >= 0`) rows.
    for r in 0..m {
        if (model.a_start[r + 1] - model.a_start[r]) != 2 {
            continue;
        }
        let mut pos = -1i32;
        let mut neg = -1i32;
        let mut all_cont = true;
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            let c = model.a_index[k] as usize;
            let a = model.a_value[k];
            if model.col_type[c].is_integral() {
                all_cont = false;
                break;
            }
            if a > 0.0 {
                pos = c as i32;
            } else {
                neg = c as i32;
            }
        }
        if !all_cont || pos < 0 || neg < 0 {
            continue;
        }
        if model.row_upper[r] < INF_HALF && model.row_lower[r] < -INF_HALF {
            st.peak[pos as usize] = neg;
        } else if model.row_lower[r] > -INF_HALF && model.row_upper[r] > INF_HALF {
            st.minv[pos as usize] = neg;
        }
    }

    // 5. Sessions = loads sharing a `peak`.
    let mut peak_to_session: Vec<i32> = vec![-1; n];
    for l in 0..n {
        let pk = st.peak[l];
        if pk >= 0 {
            let pkc = pk as usize;
            if peak_to_session[pkc] < 0 {
                peak_to_session[pkc] = st.session_loads.len() as i32;
                st.session_loads.push(Vec::new());
            }
            let sid = peak_to_session[pkc] as usize;
            st.load_session[l] = sid as i32;
            st.session_loads[sid].push(l);
        }
    }

    // 6. `total = sum(loads)`: all-continuous equality with one non-load column.
    for r in 0..m {
        if model.row_lower[r] != model.row_upper[r] {
            continue;
        }
        let mut all_cont = true;
        let mut conts: Vec<usize> = Vec::new();
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            let c = model.a_index[k] as usize;
            if model.col_type[c].is_integral() {
                all_cont = false;
                break;
            }
            conts.push(c);
        }
        if !all_cont || conts.is_empty() {
            continue;
        }
        let mut load_cols = Vec::new();
        let mut others = Vec::new();
        for &c in &conts {
            if st.peak[c] >= 0 {
                load_cols.push(c);
            } else {
                others.push(c);
            }
        }
        if !load_cols.is_empty() && others.len() == 1 {
            for &lc in &load_cols {
                st.total[lc] = others[0] as i32;
            }
        }
    }

    let mut is_peak_col = vec![false; n];
    let mut is_min_col = vec![false; n];
    let mut is_total_col = vec![false; n];
    for l in 0..n {
        if st.peak[l] >= 0 {
            is_peak_col[st.peak[l] as usize] = true;
        }
        if st.minv[l] >= 0 {
            is_min_col[st.minv[l] as usize] = true;
        }
        if st.total[l] >= 0 {
            is_total_col[st.total[l] as usize] = true;
        }
    }

    // 7. range: 3 continuous columns, peak + min + the range itself.
    for r in 0..m {
        if (model.a_start[r + 1] - model.a_start[r]) != 3 {
            continue;
        }
        let mut all_cont = true;
        let mut conts: Vec<usize> = Vec::new();
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            let c = model.a_index[k] as usize;
            if model.col_type[c].is_integral() {
                all_cont = false;
                break;
            }
            conts.push(c);
        }
        if !all_cont {
            continue;
        }
        let mut pk = -1i32;
        let mut mn = -1i32;
        let mut th = -1i32;
        for &c in &conts {
            if is_peak_col[c] {
                pk = c as i32;
            } else if is_min_col[c] {
                mn = c as i32;
            } else {
                th = c as i32;
            }
        }
        if pk >= 0 && mn >= 0 && th >= 0 {
            for l in 0..n {
                if st.peak[l] == pk {
                    st.range[l] = th;
                }
            }
        }
    }
    let mut is_range_col = vec![false; n];
    for l in 0..n {
        if st.range[l] >= 0 {
            is_range_col[st.range[l] as usize] = true;
        }
    }

    // 8. Deviation rows: a continuous `<=` row carrying an unclassified `-1`
    //    coefficient (the deviation is the only remaining auxiliary).
    st.dev_rows_of_col = vec![Vec::new(); n];
    for r in 0..m {
        if model.row_upper[r] >= INF_HALF {
            continue;
        }
        let mut all_cont = true;
        let mut cand = -1i32;
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            let c = model.a_index[k] as usize;
            let a = model.a_value[k];
            if model.col_type[c].is_integral() {
                all_cont = false;
                break;
            }
            if (a + 1.0).abs() < 1e-9 {
                let cls = st.peak[c] >= 0
                    || is_peak_col[c]
                    || is_min_col[c]
                    || is_total_col[c]
                    || is_range_col[c];
                if !cls {
                    cand = c as i32;
                }
            }
        }
        if all_cont && cand >= 0 {
            st.dev_rows.push((cand as usize, r));
            st.dev_rows_of_col[cand as usize].push(r);
        }
    }

    st.dev_of_session = vec![Vec::new(); st.session_loads.len()];
    for &(dc, r) in &st.dev_rows {
        let mut sid = -1i32;
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            let c = model.a_index[k] as usize;
            if st.peak[c] >= 0 {
                sid = st.load_session[c];
                break;
            }
        }
        if sid >= 0 {
            let s = sid as usize;
            if !st.dev_of_session[s].contains(&dc) {
                st.dev_of_session[s].push(dc);
            }
        }
    }

    // 9. Finite caps directly on a load column.
    for r in 0..m {
        if (model.a_start[r + 1] - model.a_start[r]) != 1 {
            continue;
        }
        let c = model.a_index[model.a_start[r] as usize] as usize;
        if !model.col_type[c].is_integral() && model.row_upper[r] < INF_HALF {
            if model.row_upper[r] < st.cap[c] {
                st.cap[c] = model.row_upper[r];
            }
        }
    }

    // Feeders: load column -> x columns feeding it.
    for c in 0..n {
        if st.x_load[c] >= 0 {
            st.feeders[st.x_load[c] as usize].push(c);
        }
    }

    st.detected = !st.units.is_empty() && !st.session_loads.is_empty();
    st
}

/// Residuals: row activity, per-row violation, total violation.
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
        .map(|r| row_viol(model, r, lhs[r]))
        .collect();
    let total = viol.iter().sum();
    (lhs, viol, total)
}

/// Recompute each load column from the `x` columns feeding it (keeps the load
/// definition equalities satisfied for an arbitrary start point).
fn sync_loads(st: &Struct, x: &mut [f64]) {
    for l in 0..st.feeders.len() {
        if st.peak[l] < 0 {
            continue;
        }
        let mut s = 0.0;
        for &c in &st.feeders[l] {
            s += st.x_dur[c] * x[c];
        }
        x[l] = s;
    }
}

/// Snap every auxiliary (`total`, `peak`, `min`, `range`, `dev`) to its exact
/// implied value given the current loads, so aux rows are always satisfied.
fn sync_all(model: &Model, st: &Struct, x: &mut [f64]) {
    for s in 0..st.session_loads.len() {
        let loads = &st.session_loads[s];
        if loads.is_empty() {
            continue;
        }
        let vals: Vec<f64> = loads.iter().map(|&l| x[l]).collect();
        let total: f64 = vals.iter().sum();
        let peak = vals.iter().cloned().fold(f64::MIN, f64::max);
        let min = vals.iter().cloned().fold(f64::MAX, f64::min);
        let lc0 = loads[0];
        if st.total[lc0] >= 0 {
            x[st.total[lc0] as usize] = total;
        }
        if st.peak[lc0] >= 0 {
            x[st.peak[lc0] as usize] = peak;
        }
        if st.minv[lc0] >= 0 {
            x[st.minv[lc0] as usize] = min;
        }
        if st.range[lc0] >= 0 {
            x[st.range[lc0] as usize] = peak - min;
        }
    }
    for dc in 0..st.dev_rows_of_col.len() {
        if st.dev_rows_of_col[dc].is_empty() {
            continue;
        }
        let mut req = 0.0f64;
        for &r in &st.dev_rows_of_col[dc] {
            let mut ssum = 0.0;
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                let c = model.a_index[k] as usize;
                if c == dc {
                    continue;
                }
                ssum += model.a_value[k] * x[c];
            }
            if ssum > req {
                req = ssum;
            }
        }
        x[dc] = req.max(0.0);
    }
}

/// Required value of deviation column `dc` under `newval`.
fn dev_required(model: &Model, st: &Struct, dc: usize, newval: &dyn Fn(usize) -> f64) -> f64 {
    let mut req = 0.0f64;
    for &r in &st.dev_rows_of_col[dc] {
        let mut ssum = 0.0;
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            let c = model.a_index[k] as usize;
            if c == dc {
                continue;
            }
            ssum += model.a_value[k] * newval(c);
        }
        if ssum > req {
            req = ssum;
        }
    }
    req.max(0.0)
}

/// Deltas for the auxiliaries of session `s` implied by a set of load changes,
/// mirroring `sync_all`.
fn aux_deltas(
    model: &Model,
    st: &Struct,
    x: &[f64],
    s: usize,
    ld: &[(usize, f64)],
) -> Vec<(usize, f64)> {
    let loads = &st.session_loads[s];
    if loads.is_empty() {
        return Vec::new();
    }
    let getl = |lc: usize| -> f64 {
        let mut v = x[lc];
        for &(c, d) in ld {
            if c == lc {
                v += d;
            }
        }
        v
    };
    let newload: Vec<f64> = loads.iter().map(|&l| getl(l)).collect();
    let new_total: f64 = newload.iter().sum();
    let new_peak = newload.iter().cloned().fold(f64::MIN, f64::max);
    let new_min = newload.iter().cloned().fold(f64::MAX, f64::min);
    let new_range = new_peak - new_min;
    let lc0 = loads[0];
    let mut out = Vec::new();

    let tc = st.total[lc0];
    let tc_col = if tc >= 0 { Some(tc as usize) } else { None };
    if let Some(c) = tc_col {
        if (x[c] - new_total).abs() > 1e-12 {
            out.push((c, new_total - x[c]));
        }
    }
    let pc = st.peak[lc0];
    if pc >= 0 {
        let c = pc as usize;
        if (x[c] - new_peak).abs() > 1e-12 {
            out.push((c, new_peak - x[c]));
        }
    }
    let mc = st.minv[lc0];
    if mc >= 0 {
        let c = mc as usize;
        if (x[c] - new_min).abs() > 1e-12 {
            out.push((c, new_min - x[c]));
        }
    }
    let rc = st.range[lc0];
    if rc >= 0 {
        let c = rc as usize;
        if (x[c] - new_range).abs() > 1e-12 {
            out.push((c, new_range - x[c]));
        }
    }

    for &dc in &st.dev_of_session[s] {
        let newv = |c: usize| -> f64 {
            if Some(c) == tc_col {
                new_total
            } else if loads.contains(&c) {
                getl(c)
            } else {
                x[c]
            }
        };
        let req = dev_required(model, st, dc, &newv);
        if (x[dc] - req).abs() > 1e-12 {
            out.push((dc, req - x[dc]));
        }
    }
    out
}

/// Full column-change list for a move: the decision columns plus the implied
/// load and auxiliary deltas that keep the definition rows satisfied.
fn build_full(
    model: &Model,
    st: &Struct,
    x: &[f64],
    decision: &[(usize, f64)],
) -> Vec<(usize, f64)> {
    let mut ch: Vec<(usize, f64)> = Vec::with_capacity(decision.len() + 8);
    let mut ldel: Vec<(usize, f64)> = Vec::new();
    for &(c, d) in decision {
        ch.push((c, d));
        if c < st.x_load.len() {
            let lc = st.x_load[c];
            if lc >= 0 {
                ldel.push((lc as usize, st.x_dur[c] * d));
            }
        }
    }
    // Combine repeated load deltas.
    let mut combined: Vec<(usize, f64)> = Vec::new();
    for &(lc, d) in &ldel {
        if let Some(e) = combined.iter_mut().find(|e| e.0 == lc) {
            e.1 += d;
        } else {
            combined.push((lc, d));
        }
    }
    let mut affected: Vec<usize> = Vec::new();
    for &(lc, _) in &combined {
        let s = st.load_session[lc];
        if s >= 0 && !affected.contains(&(s as usize)) {
            affected.push(s as usize);
        }
    }
    for &(lc, d) in &combined {
        if d != 0.0 {
            ch.push((lc, d));
        }
    }
    for &s in &affected {
        let ld: Vec<(usize, f64)> = combined
            .iter()
            .filter(|(lc, _)| st.load_session[*lc] == s as i32)
            .cloned()
            .collect();
        for a in aux_deltas(model, st, x, s, &ld) {
            ch.push(a);
        }
    }
    ch
}

/// Exact change in total row violation for a set of column changes.
fn eval_delta(
    model: &Model,
    csc: &Csc,
    lhs: &[f64],
    viol: &[f64],
    changes: &[(usize, f64)],
    scratch: &mut Vec<(usize, f64)>,
) -> f64 {
    scratch.clear();
    for &(c, d) in changes {
        if d == 0.0 {
            continue;
        }
        for (r, a) in csc.rows(c) {
            let mut found = false;
            for e in scratch.iter_mut() {
                if e.0 == r {
                    e.1 += a * d;
                    found = true;
                    break;
                }
            }
            if !found {
                scratch.push((r, a * d));
            }
        }
    }
    let mut delta = 0.0;
    for &(r, ch) in scratch.iter() {
        delta += row_viol(model, r, lhs[r] + ch) - viol[r];
    }
    delta
}

fn apply_changes(
    model: &Model,
    csc: &Csc,
    x: &mut [f64],
    lhs: &mut [f64],
    viol: &mut [f64],
    changes: &[(usize, f64)],
) {
    for &(c, d) in changes {
        if d == 0.0 {
            continue;
        }
        x[c] += d;
        for (r, a) in csc.rows(c) {
            viol[r] -= row_viol(model, r, lhs[r]);
            lhs[r] += a * d;
            viol[r] += row_viol(model, r, lhs[r]);
        }
    }
}

fn transfer_decision(st: &Struct, x: &[f64], c1: usize, c2: usize, d: f64) -> Vec<(usize, f64)> {
    let mut dec = Vec::with_capacity(4);
    dec.push((c1, -d));
    let z1 = st.x_z[c1];
    if z1 >= 0 && x[c1] - d <= 1e-9 {
        dec.push((z1 as usize, -1.0));
    }
    dec.push((c2, d));
    let z2 = st.x_z[c2];
    if z2 >= 0 && x[z2 as usize] < 0.5 {
        dec.push((z2 as usize, 1.0));
    }
    dec
}

struct Best {
    delta: f64,
    changes: Vec<(usize, f64)>,
}

fn consider(
    model: &Model,
    csc: &Csc,
    st: &Struct,
    x: &[f64],
    lhs: &[f64],
    viol: &[f64],
    dec: &[(usize, f64)],
    scratch: &mut Vec<(usize, f64)>,
    best: &mut Option<Best>,
    rng: &mut Rng,
) {
    let full = build_full(model, st, x, dec);
    let d = eval_delta(model, csc, lhs, viol, &full, scratch);
    let better = match best {
        None => true,
        Some(b) => d < b.delta - 1e-12 || (d <= b.delta + 1e-12 && rng.next_f64() < 0.15),
    };
    if better {
        *best = Some(Best {
            delta: d,
            changes: full,
        });
    }
}

/// Generate the best improving (or least-worsening) move for the current state.
fn find_best(
    model: &Model,
    csc: &Csc,
    st: &Struct,
    x: &[f64],
    lhs: &[f64],
    viol: &[f64],
    feeders: &[Vec<usize>],
    rng: &mut Rng,
    scratch: &mut Vec<(usize, f64)>,
) -> Option<Best> {
    let n = model.num_col;
    let mut best: Option<Best> = None;

    // Per-unit deficit (positive => unplaced matches).
    let mut need = vec![0.0f64; st.units.len()];
    for (u, cols) in st.units.iter().enumerate() {
        let mut s = 0.0;
        for &c in cols {
            s += x[c];
        }
        need[u] = st.unit_sum[u] - s;
    }

    // A) Single-variable create / drop (the generic move, now structure-aware:
    //    it also sets the fixed-charge `z` and the implied load/aux deltas).
    for (u, cols) in st.units.iter().enumerate() {
        if need[u] > 0.5 {
            for &c in cols {
                let mut d = need[u].min(model.col_upper[c] - x[c]);
                if st.x_load[c] >= 0 {
                    let lc = st.x_load[c] as usize;
                    if st.cap[lc] < INF_HALF && st.x_dur[c] > 1e-12 {
                        let room = (st.cap[lc] - x[lc]).max(0.0) / st.x_dur[c];
                        d = d.min(room);
                    }
                }
                d = d.floor();
                if d < 1.0 {
                    continue;
                }
                let mut dec = vec![(c, d)];
                let z = st.x_z[c];
                if z >= 0 && x[z as usize] < 0.5 {
                    dec.push((z as usize, 1.0));
                }
                consider(model, csc, st, x, lhs, viol, &dec, scratch, &mut best, rng);
            }
        } else if need[u] < -0.5 {
            for &c in cols {
                if x[c] < 0.5 {
                    continue;
                }
                let d = (-need[u]).min(x[c]).floor();
                if d < 1.0 {
                    continue;
                }
                let mut dec = vec![(c, -d)];
                let z = st.x_z[c];
                if z >= 0 && x[c] - d <= 1e-9 && x[z as usize] > 0.5 {
                    dec.push((z as usize, -1.0));
                }
                consider(model, csc, st, x, lhs, viol, &dec, scratch, &mut best, rng);
            }
        }
    }

    // B) Coupled transfers, only once every unit's total is placed (a transfer
    //    preserves `tot`, so it cannot help while matches are still missing).
    let tot_ok = need.iter().all(|&d| d.abs() < 0.5);
    if tot_ok {
        let x_sess = |c: usize| -> i32 {
            let l = st.x_load[c];
            if l >= 0 {
                st.load_session[l as usize]
            } else {
                -1
            }
        };

        let mut sources: Vec<usize> = Vec::new();
        // Overloaded (s,r) loads: push everything feeding them.
        for l in 0..n {
            if st.peak[l] >= 0 && st.cap[l] < INF_HALF && x[l] > st.cap[l] + TOL {
                for &c in &feeders[l] {
                    if x[c] >= 1.0 {
                        sources.push(c);
                    }
                }
            }
        }
        // `onesplit` conflicts: a unit/session with more than one positive ring.
        for cols in st.units.iter() {
            let mut sessions: Vec<i32> = Vec::new();
            for &c in cols {
                let s = x_sess(c);
                if x[c] >= 1.0 && !sessions.contains(&s) {
                    sessions.push(s);
                }
            }
            for &s in &sessions {
                let pos: Vec<usize> = cols
                    .iter()
                    .cloned()
                    .filter(|&c| x[c] >= 1.0 && x_sess(c) == s)
                    .collect();
                if pos.len() > 1 {
                    sources.extend(pos);
                }
            }
        }
        sources.sort_unstable();
        sources.dedup();

        for &c1 in &sources {
            let u = st.x_unit[c1];
            if u < 0 {
                continue;
            }
            for &c2 in &st.units[u as usize] {
                if c2 == c1 {
                    continue;
                }
                let full = x[c1];
                if full >= 1.0 {
                    let dec = transfer_decision(st, x, c1, c2, full);
                    consider(model, csc, st, x, lhs, viol, &dec, scratch, &mut best, rng);
                }
                if x[c1] > 1.5 {
                    let dec = transfer_decision(st, x, c1, c2, 1.0);
                    consider(model, csc, st, x, lhs, viol, &dec, scratch, &mut best, rng);
                }
            }
        }
    }

    best
}

/// Random transfer kick to escape a plateau. Returns false when nothing can move.
fn kick(
    model: &Model,
    csc: &Csc,
    st: &Struct,
    x: &mut [f64],
    lhs: &mut [f64],
    viol: &mut [f64],
    rng: &mut Rng,
) -> bool {
    let nu = st.units.len();
    if nu == 0 {
        return false;
    }
    for _ in 0..64 {
        let u = rng.below(nu);
        let cols = &st.units[u];
        if cols.is_empty() {
            continue;
        }
        let c1 = cols[rng.below(cols.len())];
        if x[c1] < 1.0 {
            continue;
        }
        let c2 = cols[rng.below(cols.len())];
        if c2 == c1 {
            continue;
        }
        let maxd = x[c1] as usize;
        if maxd == 0 {
            continue;
        }
        let d = 1.0 + rng.below(maxd) as f64;
        let dec = transfer_decision(st, x, c1, c2, d);
        let full = build_full(model, st, x, &dec);
        apply_changes(model, csc, x, lhs, viol, &full);
        return true;
    }
    false
}

fn repair_structured(
    model: &Model,
    csc: &Csc,
    st: &Struct,
    mut x: Vec<f64>,
    max_moves: u64,
    rng: &mut Rng,
) -> Option<Repair> {
    let n = model.num_col;
    // Clean the fixed-charge bits to match the current `x`.
    for c in 0..n {
        let z = st.x_z[c];
        if z >= 0 {
            if x[c] <= 1e-9 {
                x[z as usize] = 0.0;
            } else if x[z as usize] < 0.5 {
                x[z as usize] = 1.0;
            }
        }
    }
    sync_loads(st, &mut x);
    sync_all(model, st, &mut x);
    let (mut lhs, mut viol, mut total) = compute(model, &x);
    let mut moves = 0u64;
    let mut best_total = total;
    let mut stall = 0u64;
    let stall_limit = 20_000u64;
    let mut scratch: Vec<(usize, f64)> = Vec::new();

    loop {
        if total <= TOL {
            let chk = crate::model::check_solution(model, &x, TOL);
            if chk.feasible {
                return Some(Repair { x, moves });
            }
            // Bookkeeping drifted; resynchronise from scratch.
            sync_all(model, st, &mut x);
            let (l, v, t) = compute(model, &x);
            lhs = l;
            viol = v;
            total = t;
            if total <= TOL {
                // `compute` and `check_solution` disagree and cannot be
                // reconciled; give up rather than spin.
                break;
            }
        }
        if moves >= max_moves || stall > stall_limit {
            break;
        }

        let best = find_best(
            model,
            csc,
            st,
            &x,
            &lhs,
            &viol,
            &st.feeders,
            rng,
            &mut scratch,
        );
        match best {
            Some(b) if b.delta < -1e-12 => {
                apply_changes(model, csc, &mut x, &mut lhs, &mut viol, &b.changes);
                moves += 1;
                let (l, v, t) = compute(model, &x);
                lhs = l;
                viol = v;
                total = t;
            }
            _ => {
                if !kick(model, csc, st, &mut x, &mut lhs, &mut viol, rng) {
                    break;
                }
                moves += 1;
                let (l, v, t) = compute(model, &x);
                lhs = l;
                viol = v;
                total = t;
            }
        }

        if total < best_total - 1e-9 {
            best_total = total;
            stall = 0;
        } else {
            stall += 1;
        }
    }

    if crate::model::check_solution(model, &x, TOL).feasible {
        Some(Repair { x, moves })
    } else {
        None
    }
}

/// Result of a repair pass.
#[derive(Clone, Debug)]
pub struct Repair {
    pub x: Vec<f64>,
    pub moves: u64,
}

/// Try to turn `x0` into a fully feasible point using structure-aware moves.
///
/// Returns `Some(Repair)` only when `crate::check_solution(model, &x, tol)`
/// reports feasibility; never trust the search's own belief.
pub fn repair(model: &Model, x0: &[f64], max_moves: u64, seed: u64) -> Option<Repair> {
    if model.num_col == 0 {
        return Some(Repair {
            x: Vec::new(),
            moves: 0,
        });
    }
    if x0.len() != model.num_col {
        return None;
    }
    let csc = Csc::build(model);
    let mut x = x0.to_vec();
    for c in 0..model.num_col {
        x[c] = x[c].clamp(model.col_lower[c], model.col_upper[c]);
        if model.col_type[c].is_integral() {
            x[c] = x[c].round();
        }
        x[c] = x[c].clamp(model.col_lower[c], model.col_upper[c]);
    }
    if crate::model::check_solution(model, &x, TOL).feasible {
        return Some(Repair { x, moves: 0 });
    }

    let mut rng = Rng::new(seed);
    let st = detect(model, &csc);
    if st.detected {
        if let Some(r) = repair_structured(model, &csc, &st, x, max_moves, &mut rng) {
            return Some(r);
        }
    }
    None
}

/// Construct a feasible plan for the **inlined** planning shape
/// (`planning-*.lp`: `sum dur*x` appears directly in the cap/peak/dev rows and
/// there is no explicit `load` column). Greedy, cap-aware placement of each
/// unit's matches plus an exact projection of the continuous auxiliaries.
/// Returns a `check_solution`-verified point, or `None` if the shape is not
/// recognised or the greedy cannot fit.
fn construct_with(model: &Model, use_lpt: bool) -> Option<Vec<f64>> {
    let n = model.num_col;
    let m = model.num_row;
    let csc = Csc::build(model);

    // 1. Units: `tot` equality rows, all-integer, all coefficients +1.
    let mut x_unit = vec![-1i32; n];
    let mut units: Vec<(Vec<usize>, f64)> = Vec::new();
    for r in 0..m {
        if model.row_lower[r] != model.row_upper[r] {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        if ks == ke {
            continue;
        }
        let mut all_int = true;
        let mut all_one = true;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            if !model.col_type[c].is_integral() {
                all_int = false;
                break;
            }
            if (model.a_value[k] - 1.0).abs() > 1e-9 {
                all_one = false;
            }
        }
        if all_int && all_one {
            let uid = units.len() as i32;
            let mut cols = Vec::new();
            for k in ks..ke {
                let c = model.a_index[k] as usize;
                if x_unit[c] < 0 {
                    x_unit[c] = uid;
                }
                cols.push(c);
            }
            units.push((cols, model.row_lower[r]));
        }
    }
    if units.is_empty() {
        return None;
    }

    // 2. Sessions: equality with exactly one positive continuous and integer
    //    columns with negative coefficients. Gives `total_p` and `dur_u`.
    let mut x_dur = vec![0.0f64; n];
    let mut x_sess = vec![-1i32; n];
    let mut nsess = 0i32;
    for r in 0..m {
        if model.row_lower[r] != model.row_upper[r] {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        let mut ncont = 0usize;
        let mut cont_a = 0.0f64;
        let mut ok = true;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            if model.col_type[c].is_integral() {
                if model.a_value[k] > 0.0 {
                    ok = false;
                }
            } else {
                ncont += 1;
                cont_a = model.a_value[k];
            }
        }
        if ncont == 1 && ok && cont_a > 1e-12 {
            for k in ks..ke {
                let c = model.a_index[k] as usize;
                if model.col_type[c].is_integral() {
                    x_dur[c] = -model.a_value[k] / cont_a;
                    x_sess[c] = nsess;
                }
            }
            nsess += 1;
        }
    }
    if nsess == 0 {
        return None;
    }

    // 3. Load groups: finite-upper all-integer rows (the `cap_p_r` rows). Each
    //    is one (session, ring); its columns are the feeders.
    let mut x_group = vec![-1i32; n];
    let mut group_sess: Vec<i32> = Vec::new();
    let mut group_ring: Vec<i32> = Vec::new();
    let mut group_cap: Vec<f64> = Vec::new();
    let mut ring_count = vec![0i32; nsess as usize];
    for r in 0..m {
        if model.row_lower[r] == model.row_upper[r] {
            continue;
        }
        if !model.row_upper[r].is_finite() || model.row_upper[r] <= 0.0 {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        if ks == ke {
            continue;
        }
        let mut all_int = true;
        let mut all_bin = true;
        let mut all_one = true;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            if !model.col_type[c].is_integral() {
                all_int = false;
                break;
            }
            if model.col_type[c] != VarType::Binary && !is_binary_like(model, c) {
                all_bin = false;
            }
            if (model.a_value[k] - 1.0).abs() > 1e-9 {
                all_one = false;
            }
        }
        if !all_int || (all_bin && all_one) {
            continue; // skip `onesplit` and non-integer rows
        }
        // Require a single consistent session.
        let mut sess = -1i32;
        let mut good = true;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            if x_sess[c] < 0 {
                good = false;
                break;
            }
            if sess < 0 {
                sess = x_sess[c];
            } else if sess != x_sess[c] {
                good = false;
                break;
            }
        }
        if !good {
            continue;
        }
        let gid = group_cap.len() as i32;
        let ring = ring_count[sess as usize];
        ring_count[sess as usize] += 1;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            if x_group[c] < 0 {
                x_group[c] = gid;
            }
        }
        group_sess.push(sess);
        group_ring.push(ring);
        group_cap.push(model.row_upper[r]);
    }
    let ng = group_cap.len();
    if ng == 0 {
        return None;
    }

    // 4. Fixed-charge links `x - M z <= 0`.
    let mut x_z = vec![-1i32; n];
    for c in 0..n {
        if !model.col_type[c].is_integral() {
            continue;
        }
        for (r, a) in csc.rows(c) {
            if (model.a_start[r + 1] - model.a_start[r]) != 2
                || model.row_upper[r] > 0.0
                || model.row_lower[r] > -INF * 0.5
            {
                continue;
            }
            let mut zc = -1i32;
            let mut b = 0.0;
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                let cc = model.a_index[k] as usize;
                if cc != c {
                    zc = cc as i32;
                    b = model.a_value[k];
                }
            }
            if zc >= 0 && is_binary_like(model, zc as usize) && a > 0.0 && b < 0.0 {
                x_z[c] = zc;
            }
        }
    }

    // 5. For each unit and group, the (unique) x column.
    let mut unit_group_col: Vec<Vec<i32>> = vec![vec![-1; ng]; units.len()];
    for (u, (cols, _)) in units.iter().enumerate() {
        for &c in cols {
            let g = x_group[c];
            if g >= 0 {
                unit_group_col[u][g as usize] = c as i32;
            }
        }
    }

    // 6. Greedy cap-aware placement: place each match in the (session, ring)
    //    with the most remaining capacity, keeping one ring per (unit, session).
    let mut x = vec![0.0f64; n];
    let mut load = vec![0.0f64; ng];
    let dur_u: Vec<f64> = units.iter().map(|(cols, _)| x_dur[cols[0]]).collect();
    if use_lpt {
        // LPT: even session split, then longest block first onto the least-loaded ring.
        let mut session_blocks: Vec<Vec<(usize, i64)>> = vec![Vec::new(); nsess as usize];
        for (u, (_, matches)) in units.iter().enumerate() {
            let m = matches.round() as i64;
            if m <= 0 || nsess == 0 {
                continue;
            }
            let base = m / nsess as i64;
            let mut extra = m % nsess as i64;
            for s in 0..nsess as usize {
                let mut c = base;
                if extra > 0 {
                    c += 1;
                    extra -= 1;
                }
                if c > 0 {
                    session_blocks[s].push((u, c));
                }
            }
        }
        for s in 0..nsess as usize {
            let mut bl = std::mem::take(&mut session_blocks[s]);
            bl.sort_by(|a, b| {
                let da = dur_u[a.0] * a.1 as f64;
                let db = dur_u[b.0] * b.1 as f64;
                db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
            });
            for (u, c) in bl {
                let blk = dur_u[u] * c as f64;
                let mut best = -1i32;
                let mut bestl = f64::INFINITY;
                let mut fallback = -1i32;
                let mut fallback_rem = f64::NEG_INFINITY;
                for g in 0..ng {
                    if group_sess[g] as usize != s || unit_group_col[u][g] < 0 {
                        continue;
                    }
                    if load[g] + blk <= group_cap[g] + 1e-9 && load[g] < bestl {
                        bestl = load[g];
                        best = g as i32;
                    }
                    let rem = group_cap[g] - load[g];
                    if rem > fallback_rem {
                        fallback_rem = rem;
                        fallback = g as i32;
                    }
                }
                if best < 0 {
                    best = fallback;
                }
                if best < 0 {
                    continue;
                }
                let g = best as usize;
                let col = unit_group_col[u][g] as usize;
                x[col] = c as f64;
                load[g] += dur_u[u] * c as f64;
            }
        }
    } else {
        // Cap-greedy: place each match in the ring with the most remaining cap,
        // keeping one ring per (unit, session).
        for (u, (cols, matches)) in units.iter().enumerate() {
            let need = matches.round() as i64;
            if need <= 0 {
                continue;
            }
            let mut lock = vec![-1i32; nsess as usize];
            let mut count = vec![0i64; ng];
            for _ in 0..need {
                let mut best = -1i32;
                let mut best_rem = f64::NEG_INFINITY;
                for g in 0..ng {
                    let col = unit_group_col[u][g];
                    if col < 0 {
                        continue;
                    }
                    let p = group_sess[g] as usize;
                    let r = group_ring[g];
                    if lock[p] != -1 && lock[p] != r {
                        continue;
                    }
                    let rem = group_cap[g] - load[g];
                    if rem > best_rem {
                        best_rem = rem;
                        best = g as i32;
                    }
                }
                if best < 0 {
                    return None;
                }
                let g = best as usize;
                lock[group_sess[g] as usize] = group_ring[g];
                let col = unit_group_col[u][g] as usize;
                load[g] += x_dur[col];
                count[g] += 1;
            }
            for &c in cols {
                let g = x_group[c];
                if g >= 0 {
                    x[c] = count[g as usize] as f64;
                }
            }
        }
    }

    // 7. Set the fixed-charge binaries and project the continuous auxiliaries.
    for c in 0..n {
        if x[c] > 0.5 && x_z[c] >= 0 {
            x[x_z[c] as usize] = 1.0;
        }
    }
    project_continuous(model, &csc, &mut x);

    // Balance ring loads per session: repeatedly move a unit's matches from the
    // heaviest ring to the lightest (respecting caps), which lowers peak/range.
    for _round in 0..60 {
        let mut loads = vec![0.0f64; ng];
        for c in 0..n {
            let g = x_group[c];
            if g >= 0 {
                loads[g as usize] += x_dur[c] * x[c];
            }
        }
        let mut moved = false;
        for p in 0..nsess as usize {
            let gs: Vec<usize> = (0..ng).filter(|&g| group_sess[g] as usize == p).collect();
            if gs.len() < 2 {
                continue;
            }
            let mut gmax = gs[0];
            let mut gmin = gs[0];
            for &g in &gs {
                if loads[g] > loads[gmax] {
                    gmax = g;
                }
                if loads[g] < loads[gmin] {
                    gmin = g;
                }
            }
            let spread = loads[gmax] - loads[gmin];
            if spread <= 1e-6 {
                continue;
            }
            for u in 0..units.len() {
                let cu = unit_group_col[u][gmax];
                let cm = unit_group_col[u][gmin];
                if cu < 0 || cm < 0 {
                    continue;
                }
                let (cu, cm) = (cu as usize, cm as usize);
                let m = x[cu];
                if m <= 0.0 {
                    continue;
                }
                let d = x_dur[cu] * m;
                if 2.0 * d <= spread + 1e-9 && loads[gmin] + d <= group_cap[gmin] + 1e-9 {
                    x[cu] = 0.0;
                    x[cm] += m;
                    if x_z[cu] >= 0 {
                        x[x_z[cu] as usize] = 0.0;
                    }
                    if x_z[cm] >= 0 && x[cm] > 0.0 {
                        x[x_z[cm] as usize] = 1.0;
                    }
                    loads[gmax] -= d;
                    loads[gmin] += d;
                    moved = true;
                    break;
                }
            }
        }
        if !moved {
            break;
        }
    }
    project_continuous(model, &csc, &mut x);

    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var("OPTIO_LPDEBUG").is_ok() {
        let chk = crate::model::check_solution(model, &x, TOL);
        eprintln!(
            "[plan] units={} sessions={} groups={} feasible={} rowViol={:.3e} intViol={:.3e}",
            units.len(),
            nsess,
            ng,
            chk.feasible,
            chk.max_row_violation,
            chk.max_integrality_violation
        );
        // worst row
        let mut wr = 0usize;
        let mut wv = 0.0f64;
        for r in 0..m {
            let mut lhs = 0.0;
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                lhs += model.a_value[k] * x[model.a_index[k] as usize];
            }
            let v = (model.row_lower[r] - lhs)
                .max(lhs - model.row_upper[r])
                .max(0.0);
            if v > wv {
                wv = v;
                wr = r;
            }
        }
        let mut lhs = 0.0;
        for k in model.a_start[wr] as usize..model.a_start[wr + 1] as usize {
            lhs += model.a_value[k] * x[model.a_index[k] as usize];
        }
        eprintln!(
            "[plan] worst row {} lhs={:.1} bounds=[{:.1},{:.1}]",
            wr, lhs, model.row_lower[wr], model.row_upper[wr]
        );
        let mut loads = vec![0.0f64; ng];
        for c in 0..n {
            let g = x_group[c];
            if g >= 0 {
                loads[g as usize] += x_dur[c] * x[c];
            }
        }
        eprintln!(
            "[plan]   group caps={:?}",
            group_cap
                .iter()
                .map(|v| v.round() as i64)
                .collect::<Vec<_>>()
        );
        eprintln!(
            "[plan]   group loads={:?}",
            loads.iter().map(|v| v.round() as i64).collect::<Vec<_>>()
        );
        eprintln!("[plan]   constructed objective={:.1}", model.objective(&x));
    }

    if crate::model::check_solution(model, &x, TOL).feasible {
        Some(x)
    } else {
        None
    }
}

/// Construct a feasible plan for the inlined planning shape. Tries two
/// assignments (LPT load-balancing and a cap-greedy) and returns the better
/// feasible one. Returns `None` if the shape is not recognised or both fail.
pub fn construct_inlined_planning(model: &Model) -> Option<Vec<f64>> {
    let a = construct_with(model, true);
    let b = construct_with(model, false);
    match (a, b) {
        (Some(x), Some(y)) => Some(if model.objective(&x) <= model.objective(&y) {
            x
        } else {
            y
        }),
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------------------
// Objective local search for the planning shape
//
// The constructive planner (`construct_with`) balances ring loads with a greedy
// heaviest->lightest sweep, but it is not objective-optimal and stops early.
// This module detects the planning structure *generically* (from the LP matrix,
// without names) and runs a transfer local search that minimises the true
// objective (peak / range / deviation + finals-day preference). It is the
// primal-quality counterpart to the feasibility repair, and is what closes the
// gap to native HiGHS on the planning suite.
// ---------------------------------------------------------------------------

use std::collections::HashMap;

/// Detected planning structure, in terms of the *inlined* shape where the load
/// of a `(session, ring)` group is `sum_u dur_u * x[u,s,r]` (no explicit load
/// column) and the auxiliaries (`total`/`peak`/`min`/`range`/`dev`) are linear
/// functions of the group loads.
struct PShape {
    nsess: usize,
    /// Feeder `(x column, duration)` per group `(session, ring)`.
    g_feeder: Vec<Vec<(usize, f64)>>,
    g_sess: Vec<usize>,
    g_cap: Vec<f64>,
    /// Groups per session, in ring order.
    groups: Vec<Vec<usize>>,
    u_dur: Vec<f64>,
    /// `xcol[u][g]` / `zcol[u][g]` / `ycol[u][s]` column indices (or -1).
    xcol: Vec<Vec<i32>>,
    zcol: Vec<Vec<i32>>,
    ycol: Vec<Vec<i32>>,
    xr_of: Vec<i32>,
    a_total: Vec<i32>,
    a_peak: Vec<i32>,
    a_min: Vec<i32>,
    a_range: Vec<i32>,
    /// `a_dev[s][ring]`, with `dev = |dev_coef * load - total|`.
    a_dev: Vec<Vec<i32>>,
    dev_coef: Vec<Vec<f64>>,
    cost: Vec<f64>,
}

/// Objective contribution of the auxiliaries of session `s` under `load`.
fn p_sess_cost(sh: &PShape, s: usize, load: impl Fn(usize) -> f64) -> f64 {
    let gs = &sh.groups[s];
    if gs.is_empty() {
        return 0.0;
    }
    let mut total = 0.0;
    let mut peak = f64::MIN;
    let mut mn = f64::MAX;
    for &g in gs {
        let v = load(g);
        total += v;
        peak = peak.max(v);
        mn = mn.min(v);
    }
    let range = peak - mn;
    let mut c = 0.0;
    if sh.a_total[s] >= 0 {
        c += sh.cost[sh.a_total[s] as usize] * total;
    }
    if sh.a_peak[s] >= 0 {
        c += sh.cost[sh.a_peak[s] as usize] * peak;
    }
    if sh.a_min[s] >= 0 {
        c += sh.cost[sh.a_min[s] as usize] * mn;
    }
    if sh.a_range[s] >= 0 {
        c += sh.cost[sh.a_range[s] as usize] * range;
    }
    for ring in 0..gs.len() {
        let dc = sh.a_dev[s][ring];
        if dc >= 0 {
            let v = load(gs[ring]);
            c += sh.cost[dc as usize] * (sh.dev_coef[s][ring] * v - total).abs();
        }
    }
    c
}

/// Detect the planning structure from the constraint matrix. Returns `None`
/// when the shape is not recognised.
fn detect_plan(model: &Model, csc: &Csc) -> Option<PShape> {
    let n = model.num_col;
    let m = model.num_row;
    let neg_inf = -INF * 0.5;

    // x columns are integral and have a fixed-charge link row `x - M z <= 0`.
    let mut is_x = vec![false; n];
    for c in 0..n {
        if !model.col_type[c].is_integral() {
            continue;
        }
        for (r, a) in csc.rows(c) {
            if (model.a_start[r + 1] - model.a_start[r]) != 2
                || model.row_upper[r] > 0.0
                || model.row_lower[r] > -INF * 0.5
            {
                continue;
            }
            let mut zc = -1i32;
            let mut b = 0.0;
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                let cc = model.a_index[k] as usize;
                if cc != c {
                    zc = cc as i32;
                    b = model.a_value[k];
                }
            }
            if zc >= 0 && is_binary_like(model, zc as usize) && a > 0.0 && b < 0.0 {
                is_x[c] = true;
            }
        }
    }
    if !is_x.iter().any(|&b| b) {
        return None;
    }

    // Units: equality rows whose columns are all `x` with coefficient +1.
    let mut x_unit = vec![-1i32; n];
    let mut units: Vec<Vec<usize>> = Vec::new();
    for r in 0..m {
        if model.row_lower[r] != model.row_upper[r] {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        if ks == ke {
            continue;
        }
        let mut ok = true;
        let mut cols = Vec::new();
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            if !is_x[c] || (model.a_value[k] - 1.0).abs() > 1e-9 {
                ok = false;
                break;
            }
            cols.push(c);
        }
        if ok {
            let uid = units.len() as i32;
            for &c in &cols {
                if x_unit[c] < 0 {
                    x_unit[c] = uid;
                }
            }
            units.push(cols);
        }
    }
    if units.is_empty() {
        return None;
    }
    let nu = units.len();

    // Groups `(session, ring)` from the `peakge` rows (`sum dur x - peak <= 0`).
    let mut x_group = vec![-1i32; n];
    let mut u_dur = vec![0.0f64; nu];
    let mut g_feeder: Vec<Vec<(usize, f64)>> = Vec::new();
    let mut g_sess: Vec<usize> = Vec::new();
    let mut g_ring: Vec<usize> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut sess_peak: Vec<i32> = Vec::new();
    let mut peak_index: HashMap<i32, usize> = HashMap::new();
    for r in 0..m {
        if !model.row_upper[r].is_finite() || model.row_upper[r] > 0.0 {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        let mut cont = -1i32;
        let mut ncont = 0usize;
        let mut colx: Vec<(usize, f64)> = Vec::new();
        let mut ok = true;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            let a = model.a_value[k];
            if model.col_type[c].is_integral() {
                if !is_x[c] {
                    ok = false;
                    break;
                }
                colx.push((c, a));
            } else {
                ncont += 1;
                cont = c as i32;
                if a >= 0.0 {
                    ok = false;
                }
            }
        }
        if !ok || ncont != 1 || colx.is_empty() {
            continue;
        }
        let sid = match peak_index.get(&cont) {
            Some(&s) => s,
            None => {
                let s = sess_peak.len();
                sess_peak.push(cont);
                groups.push(Vec::new());
                peak_index.insert(cont, s);
                s
            }
        };
        let gid = g_feeder.len();
        for &(c, a) in &colx {
            if x_group[c] < 0 {
                x_group[c] = gid as i32;
            }
            let u = x_unit[c];
            if u >= 0 && u_dur[u as usize] == 0.0 {
                u_dur[u as usize] = a;
            }
        }
        g_ring.push(groups[sid].len());
        groups[sid].push(gid);
        g_sess.push(sid);
        g_feeder.push(colx);
    }
    let ng = g_feeder.len();
    let nsess = groups.len();
    if ng == 0 || nsess == 0 {
        return None;
    }

    // Caps: rows of all-`x` columns with a finite positive upper bound.
    let mut g_cap = vec![f64::INFINITY; ng];
    for r in 0..m {
        if !model.row_upper[r].is_finite() || model.row_upper[r] <= 0.0 {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        let mut g = -1i32;
        let mut ok = true;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            if !is_x[c] {
                ok = false;
                break;
            }
            let cg = x_group[c];
            if g < 0 {
                g = cg;
            } else if cg != g {
                ok = false;
                break;
            }
        }
        if ok && g >= 0 {
            g_cap[g as usize] = g_cap[g as usize].min(model.row_upper[r]);
        }
    }

    // x/z/y/xr column maps.
    let mut zcol = vec![vec![-1i32; ng]; nu];
    let mut xcol = vec![vec![-1i32; ng]; nu];
    for u in 0..nu {
        for &c in &units[u] {
            let g = x_group[c];
            if g >= 0 {
                xcol[u][g as usize] = c as i32;
            }
        }
    }
    // z from the link rows.
    for u in 0..nu {
        for g in 0..ng {
            let c = xcol[u][g];
            if c < 0 {
                continue;
            }
            let c = c as usize;
            for (r, a) in csc.rows(c) {
                if (model.a_start[r + 1] - model.a_start[r]) != 2
                    || model.row_upper[r] > 0.0
                    || model.row_lower[r] > -INF * 0.5
                {
                    continue;
                }
                let mut zc = -1i32;
                let mut b = 0.0;
                for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                    let cc = model.a_index[k] as usize;
                    if cc != c {
                        zc = cc as i32;
                        b = model.a_value[k];
                    }
                }
                if zc >= 0 && is_binary_like(model, zc as usize) && a > 0.0 && b < 0.0 {
                    zcol[u][g] = zc;
                }
            }
        }
    }
    let mut ycol = vec![vec![-1i32; nsess]; nu];
    for r in 0..m {
        if model.row_lower[r] != model.row_upper[r] {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        let mut yc = -1i32;
        let mut xs: Vec<usize> = Vec::new();
        let mut ok = true;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            if is_x[c] {
                xs.push(c);
            } else if model.col_type[c].is_integral() {
                if yc >= 0 {
                    ok = false;
                    break;
                }
                yc = c as i32;
            } else {
                ok = false;
                break;
            }
        }
        if ok && yc >= 0 && !xs.is_empty() {
            let u = x_unit[xs[0]];
            let g = x_group[xs[0]];
            if u >= 0 && g >= 0 {
                ycol[u as usize][g_sess[g as usize]] = yc;
            }
        }
    }
    let mut xr_of = vec![-1i32; n];
    for r in 0..m {
        if model.row_lower[r] != model.row_upper[r] {
            continue;
        }
        if (model.a_start[r + 1] - model.a_start[r]) != 2 {
            continue;
        }
        let c0 = model.a_index[model.a_start[r] as usize] as usize;
        let c1 = model.a_index[model.a_start[r] as usize + 1] as usize;
        if is_x[c0] && !is_x[c1] && model.col_type[c1].is_integral() {
            xr_of[c0] = c1 as i32;
        } else if is_x[c1] && !is_x[c0] && model.col_type[c0].is_integral() {
            xr_of[c1] = c0 as i32;
        }
    }

    // Auxiliaries per session.
    let mut a_total = vec![-1i32; nsess];
    let mut a_min = vec![-1i32; nsess];
    let mut a_range = vec![-1i32; nsess];
    // total = sum loads (equality: one continuous `+`, the rest `x` with `-`).
    for r in 0..m {
        if model.row_lower[r] != model.row_upper[r] {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        let mut cont = -1i32;
        let mut ncont = 0usize;
        let mut sid = usize::MAX;
        let mut ok = true;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            let a = model.a_value[k];
            if model.col_type[c].is_integral() {
                if !is_x[c] || a >= 0.0 {
                    ok = false;
                    break;
                }
                let g = x_group[c];
                if g >= 0 {
                    sid = g_sess[g as usize];
                }
            } else {
                ncont += 1;
                cont = c as i32;
                if a <= 0.0 {
                    ok = false;
                }
            }
        }
        if ok && ncont == 1 && sid != usize::MAX {
            a_total[sid] = cont;
        }
    }
    // min (finite lower, one continuous `-`, the rest `x` with `+`).
    for r in 0..m {
        if !model.row_lower[r].is_finite() || model.row_lower[r] <= neg_inf {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        let mut cont = -1i32;
        let mut ncont = 0usize;
        let mut sid = usize::MAX;
        let mut ok = true;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            let a = model.a_value[k];
            if model.col_type[c].is_integral() {
                if !is_x[c] || a <= 0.0 {
                    ok = false;
                    break;
                }
                let g = x_group[c];
                if g >= 0 {
                    sid = g_sess[g as usize];
                }
            } else {
                ncont += 1;
                cont = c as i32;
                if a >= 0.0 {
                    ok = false;
                }
            }
        }
        if ok && ncont == 1 && sid != usize::MAX {
            a_min[sid] = cont;
        }
    }
    // range (finite lower, three continuous: peak, min, range).
    for r in 0..m {
        if !model.row_lower[r].is_finite() || model.row_lower[r] <= neg_inf {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        let mut conts: Vec<usize> = Vec::new();
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            if !model.col_type[c].is_integral() {
                conts.push(c);
            }
        }
        if conts.len() != 3 {
            continue;
        }
        // Identify peak (has a `peakge` row) and min; the third is range.
        let mut sid = usize::MAX;
        for &c in &conts {
            if let Some(&s) = peak_index.get(&(c as i32)) {
                sid = s;
                break;
            }
        }
        if sid == usize::MAX {
            continue;
        }
        let peak = sess_peak[sid];
        let mn = a_min[sid];
        for &c in &conts {
            if c as i32 != peak && c as i32 != mn {
                a_range[sid] = c as i32;
            }
        }
    }
    // dev (finite upper, continuous `dev` (`-`) + `total`, the rest `x`).
    let mut a_dev: Vec<Vec<i32>> = groups.iter().map(|gs| vec![-1i32; gs.len()]).collect();
    let mut dev_coef: Vec<Vec<f64>> = groups.iter().map(|gs| vec![0.0f64; gs.len()]).collect();
    for r in 0..m {
        if !model.row_upper[r].is_finite() || model.row_upper[r] > 0.0 + 1e-9 {
            continue;
        }
        let ks = model.a_start[r] as usize;
        let ke = model.a_start[r + 1] as usize;
        let mut dv = -1i32;
        let mut tc = -1i32;
        let mut ncont = 0usize;
        let mut sid = usize::MAX;
        let mut ring = usize::MAX;
        let mut coef = 0.0f64;
        let mut ok = true;
        for k in ks..ke {
            let c = model.a_index[k] as usize;
            let a = model.a_value[k];
            if model.col_type[c].is_integral() {
                if !is_x[c] {
                    ok = false;
                    break;
                }
                let u = x_unit[c];
                if u < 0 {
                    ok = false;
                    break;
                }
                let g = x_group[c];
                if g >= 0 {
                    let gg = g as usize;
                    sid = g_sess[gg];
                    ring = g_ring[gg];
                    if u_dur[u as usize].abs() > 1e-12 {
                        coef = (a / u_dur[u as usize]).abs();
                    }
                }
            } else {
                ncont += 1;
                if a < 0.0 {
                    // the one with `-` is `dev` unless it is the total column
                    let is_total = a_total.iter().any(|&t| t == c as i32);
                    if !is_total {
                        dv = c as i32;
                    } else {
                        tc = c as i32;
                    }
                } else {
                    tc = c as i32;
                }
            }
        }
        if !ok || ncont != 2 || dv < 0 || sid == usize::MAX || ring == usize::MAX {
            continue;
        }
        let _ = tc;
        a_dev[sid][ring] = dv;
        dev_coef[sid][ring] = coef;
    }

    Some(PShape {
        nsess,
        g_feeder,
        g_sess,
        g_cap,
        groups,
        u_dur,
        xcol,
        zcol,
        ycol,
        xr_of,
        a_total,
        a_peak: sess_peak,
        a_min,
        a_range,
        a_dev,
        dev_coef,
        cost: model.col_cost.clone(),
    })
}

/// Build a full `x` vector from an assignment (x/z/y/xr), then project the
/// continuous auxiliaries. The caller validates with `check_solution`.
fn p_finalize(model: &Model, sh: &PShape, assign: &[Vec<f64>]) -> Vec<f64> {
    let mut x = vec![0.0f64; model.num_col];
    for u in 0..assign.len() {
        for g in 0..sh.g_feeder.len() {
            let a = assign[u][g];
            let xc = sh.xcol[u][g];
            if xc < 0 {
                continue;
            }
            x[xc as usize] = a;
            let zc = sh.zcol[u][g];
            if zc >= 0 {
                x[zc as usize] = if a > 0.5 { 1.0 } else { 0.0 };
            }
            let r = sh.xr_of[xc as usize];
            if r >= 0 {
                x[r as usize] = a;
            }
        }
        for s in 0..sh.nsess {
            let yc = sh.ycol[u][s];
            if yc >= 0 {
                let mut yv = 0.0;
                for &g in &sh.groups[s] {
                    yv += assign[u][g];
                }
                x[yc as usize] = yv;
            }
        }
    }
    let csc = Csc::build(model);
    project_continuous(model, &csc, &mut x);
    x
}

/// Objective local search over the planning assignment: relocate a unit's
/// matches between `(session, ring)` groups to reduce the true objective, with
/// random kicks to escape local optima. Returns a validated `(objective, x)`.
pub fn improve_planning(
    model: &Model,
    x0: &[f64],
    clock: &dyn Clock,
    deadline_secs: f64,
    seed: u64,
) -> Option<(f64, Vec<f64>)> {
    improve_planning_adaptive(model, x0, clock, deadline_secs, seed, true)
}

pub fn improve_planning_adaptive(
    model: &Model,
    x0: &[f64],
    clock: &dyn Clock,
    deadline_secs: f64,
    seed: u64,
    adaptive: bool,
) -> Option<(f64, Vec<f64>)> {
    if x0.len() != model.num_col {
        return None;
    }
    let block_started = clock.now_secs();
    if let Some(x) = crate::block_planner::improve(model, Some(x0), clock, deadline_secs) {
        return Some((model.objective(&x), x));
    }
    let deadline_secs = deadline_secs - (clock.now_secs() - block_started);
    if deadline_secs <= 0. {
        return None;
    }
    let csc = Csc::build(model);
    let sh = match detect_plan(model, &csc) {
        Some(s) => s,
        None => {
            #[cfg(not(target_arch = "wasm32"))]
            if std::env::var("OPTIO_PLANDEBUG").is_ok() {
                eprintln!("[plans] detect_plan failed");
            }
            return None;
        }
    };
    let nu = sh.xcol.len();
    let ng = sh.g_feeder.len();
    #[cfg(not(target_arch = "wasm32"))]
    let pd = std::env::var("OPTIO_PLANDEBUG").is_ok();
    #[cfg(target_arch = "wasm32")]
    let pd = false;
    if pd {
        eprintln!("[plans] units={} sessions={} groups={}", nu, sh.nsess, ng);
    }

    // Seed the assignment from x0 (integers).
    let mut assign = vec![vec![0.0f64; ng]; nu];
    let mut load = vec![0.0f64; ng];
    for u in 0..nu {
        for g in 0..ng {
            let xc = sh.xcol[u][g];
            if xc >= 0 {
                let v = x0[xc as usize].round().max(0.0);
                assign[u][g] = v;
            }
        }
    }
    for g in 0..ng {
        for &(c, d) in &sh.g_feeder[g] {
            load[g] += d * x0[c].round().max(0.0);
        }
    }

    // Objective of an assignment (x/z/y cost + per-session auxiliary cost).
    let assignment_obj = |assign: &[Vec<f64>], load: &[f64]| -> f64 {
        let mut obj = 0.0;
        for u in 0..nu {
            for g in 0..ng {
                let a = assign[u][g];
                if a == 0.0 {
                    continue;
                }
                if sh.xcol[u][g] >= 0 {
                    obj += sh.cost[sh.xcol[u][g] as usize] * a;
                }
                if sh.zcol[u][g] >= 0 && a > 0.5 {
                    obj += sh.cost[sh.zcol[u][g] as usize];
                }
            }
            for s in 0..sh.nsess {
                let y = sh.ycol[u][s];
                if y >= 0 {
                    let mut yv = 0.0;
                    for &g in &sh.groups[s] {
                        yv += assign[u][g];
                    }
                    obj += sh.cost[y as usize] * yv;
                }
            }
        }
        for s in 0..sh.nsess {
            obj += p_sess_cost(&sh, s, |g| load[g]);
        }
        obj
    };

    // Delta of moving `d` matches of unit `u` from group `g1` to `g2`.
    let move_delta = |assign: &[Vec<f64>],
                      load: &[f64],
                      u: usize,
                      g1: usize,
                      g2: usize,
                      d: f64|
     -> Option<f64> {
        if d <= 0.0 || g1 == g2 {
            return None;
        }
        let du = sh.u_dur[u];
        if load[g2] + du * d > sh.g_cap[g2] + 1e-6 {
            return None;
        }
        // One ring per (unit, session): a within-session move must drain the
        // source, and a cross-session move may not land where the unit already
        // uses another ring of the destination session.
        let s1 = sh.g_sess[g1];
        let s2 = sh.g_sess[g2];
        let a1 = assign[u][g1];
        if s1 == s2 {
            if d < a1 - 1e-9 {
                return None;
            }
        } else {
            for &gg in &sh.groups[s2] {
                if gg != g2 && assign[u][gg] > 1e-9 {
                    return None;
                }
            }
        }
        let x1 = sh.xcol[u][g1];
        let x2 = sh.xcol[u][g2];
        if x1 < 0 || x2 < 0 {
            return None;
        }
        let mut delta = sh.cost[x1 as usize] * (-d) + sh.cost[x2 as usize] * d;
        let a1 = assign[u][g1];
        let a2 = assign[u][g2];
        let z1 = sh.zcol[u][g1];
        let z2 = sh.zcol[u][g2];
        if z1 >= 0 && a1 - d <= 1e-9 && a1 > 1e-9 {
            delta -= sh.cost[z1 as usize];
        }
        if z2 >= 0 && a2 <= 1e-9 {
            delta += sh.cost[z2 as usize];
        }
        let s1 = sh.g_sess[g1];
        let s2 = sh.g_sess[g2];
        if s1 != s2 {
            let y1 = sh.ycol[u][s1];
            let y2 = sh.ycol[u][s2];
            if y1 >= 0 {
                delta -= sh.cost[y1 as usize] * d;
            }
            if y2 >= 0 {
                delta += sh.cost[y2 as usize] * d;
            }
        }
        let old1 = p_sess_cost(&sh, s1, |g| load[g]);
        let old2 = if s1 != s2 {
            p_sess_cost(&sh, s2, |g| load[g])
        } else {
            0.0
        };
        let nl = |g: usize| -> f64 {
            if g == g1 {
                load[g1] - du * d
            } else if g == g2 {
                load[g2] + du * d
            } else {
                load[g]
            }
        };
        let new1 = p_sess_cost(&sh, s1, nl);
        let new2 = if s1 != s2 {
            p_sess_cost(&sh, s2, nl)
        } else {
            0.0
        };
        delta += (new1 - old1) + (new2 - old2);
        Some(delta)
    };

    let mut obj = assignment_obj(&assign, &load);
    let obj0 = obj;
    let mut best_obj = obj;
    let mut best_assign = assign.clone();
    let mut rng = Rng::new(seed);
    let start = clock.now_secs();
    // Time to first feasible/validated improvement is bounded by the caller.
    let mut last_improvement = start;
    let stall_seconds = (deadline_secs * 0.15).clamp(0.05, 1.0);
    let mut stall = 0u32;
    let stall_limit = 400u32;

    let mut order: Vec<usize> = (0..nu).collect();
    while clock.now_secs() - start < deadline_secs {
        if adaptive && clock.now_secs() - last_improvement > stall_seconds {
            break;
        }
        // Fisher-Yates shuffle for a randomized first-improvement scan.
        for i in (1..order.len()).rev() {
            let j = (rng.next_f64() * (i as f64 + 1.0)) as usize % (i + 1);
            order.swap(i, j);
        }
        let mut improved = false;
        for &u in &order {
            let du = sh.u_dur[u];
            // Candidate source groups (occupied) and destinations.
            let mut src: Vec<usize> = Vec::new();
            for g in 0..ng {
                if assign[u][g] > 1e-9 {
                    src.push(g);
                }
            }
            if src.is_empty() {
                continue;
            }
            let mut best: Option<(usize, usize, f64, f64)> = None; // (g1,g2,d,delta)
            for &g1 in &src {
                let a1 = assign[u][g1];
                for g2 in 0..ng {
                    if g2 == g1 {
                        continue;
                    }
                    if load[g2] + du * a1 > sh.g_cap[g2] + 1e-6 && a1 > 0.0 {
                        // full move may not fit; still try d=1 below
                    }
                    for &d in &[a1, 1.0f64] {
                        if d <= 0.0 || d > a1 + 1e-9 {
                            continue;
                        }
                        if let Some(delta) = move_delta(&assign, &load, u, g1, g2, d) {
                            if delta < -1e-9 && best.map_or(true, |(_, _, _, bd)| delta < bd) {
                                best = Some((g1, g2, d, delta));
                            }
                        }
                    }
                }
            }
            if let Some((g1, g2, d, delta)) = best {
                let du2 = sh.u_dur[u];
                assign[u][g1] -= d;
                assign[u][g2] += d;
                load[g1] -= du2 * d;
                load[g2] += du2 * d;
                obj += delta;
                improved = true;
            }
        }
        if improved {
            if obj < best_obj - 1e-9 {
                best_obj = obj;
                best_assign = assign.clone();
                last_improvement = clock.now_secs();
            }
            stall = 0;
        } else {
            // Kick: move a random unit with some matches to a random feasible group.
            let mut moved = false;
            for _ in 0..32 {
                let u = (rng.next_f64() * nu as f64) as usize % nu.max(1);
                let mut src: Vec<usize> = Vec::new();
                for g in 0..ng {
                    if assign[u][g] > 1e-9 {
                        src.push(g);
                    }
                }
                if src.is_empty() {
                    continue;
                }
                let g1 = src[(rng.next_f64() * src.len() as f64) as usize % src.len()];
                let g2 = (rng.next_f64() * ng as f64) as usize % ng.max(1);
                if g2 == g1 {
                    continue;
                }
                let d = assign[u][g1];
                if let Some(delta) = move_delta(&assign, &load, u, g1, g2, d) {
                    let du2 = sh.u_dur[u];
                    assign[u][g1] -= d;
                    assign[u][g2] += d;
                    load[g1] -= du2 * d;
                    load[g2] += du2 * d;
                    obj += delta;
                    moved = true;
                    break;
                }
            }
            if moved {
                if obj < best_obj - 1e-9 {
                    best_obj = obj;
                    best_assign = assign.clone();
                    last_improvement = clock.now_secs();
                }
            } else {
                break;
            }
            stall += 1;
            if stall > stall_limit {
                break;
            }
        }
    }

    let x = p_finalize(model, &sh, &best_assign);
    let chk = crate::model::check_solution(model, &x, TOL);
    if pd {
        eprintln!(
            "[plans] start_est={:.2} best_est={:.2} feasible={} objective={:.2}",
            obj0, best_obj, chk.feasible, chk.objective
        );
    }
    if chk.feasible {
        Some((chk.objective, x))
    } else {
        None
    }
}

/// Set each continuous variable to the smallest value satisfying its rows.
/// Phase A projects using only rows with no *other* continuous column (the
/// "definition" rows: `total`, `peak`, `min`), so e.g. `peak` gets `max load`.
/// Phase B then projects `range`/`dev` using all rows, with the phase-A values
/// already fixed.
fn project_continuous(model: &Model, csc: &Csc, x: &mut [f64]) {
    let n = model.num_col;
    // `has_pure[c]`: c appears in some row that contains no other continuous col.
    let mut has_pure = vec![false; n];
    for c in 0..n {
        if model.col_type[c].is_integral() {
            continue;
        }
        for (r, _) in csc.rows(c) {
            let mut other_cont = false;
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                let cc = model.a_index[k] as usize;
                if cc != c && !model.col_type[cc].is_integral() {
                    other_cont = true;
                }
            }
            if !other_cont {
                has_pure[c] = true;
            }
        }
    }
    // Phase A: definition aux (total/peak/min) from their pure rows.
    for _pass in 0..3 {
        for c in 0..n {
            if !model.col_type[c].is_integral() && has_pure[c] {
                project_one(model, csc, x, c, true);
            }
        }
    }
    // Phase B: dependent aux (range/dev) from all rows, once the above are set.
    for _pass in 0..3 {
        for c in 0..n {
            if !model.col_type[c].is_integral() && !has_pure[c] {
                project_one(model, csc, x, c, false);
            }
        }
    }
}

fn project_one(model: &Model, csc: &Csc, x: &mut [f64], c: usize, only_pure: bool) {
    let mut lo = model.col_lower[c];
    let mut hi = model.col_upper[c];
    if lo <= -INF_HALF {
        lo = f64::NEG_INFINITY;
    }
    if hi >= INF_HALF {
        hi = f64::INFINITY;
    }
    let mut need_lo = lo;
    let mut need_hi = hi;
    for (r, a) in csc.rows(c) {
        if a == 0.0 {
            continue;
        }
        let mut other = 0.0;
        let mut has_other_cont = false;
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            let cc = model.a_index[k] as usize;
            if cc != c {
                other += model.a_value[k] * x[cc];
                if !model.col_type[cc].is_integral() {
                    has_other_cont = true;
                }
            }
        }
        if only_pure && has_other_cont {
            continue;
        }
        // Row bounds carry the same +/-`INF` sentinel as column bounds: convert
        // them, or a one-sided row's open side looks finite and clamps the
        // variable to `INF` (e.g. a perfectly balanced `range` = 0).
        let rl = if model.row_lower[r] <= -INF_HALF {
            f64::NEG_INFINITY
        } else {
            model.row_lower[r]
        };
        let ru = if model.row_upper[r] >= INF_HALF {
            f64::INFINITY
        } else {
            model.row_upper[r]
        };
        let (l, h) = if a > 0.0 {
            ((rl - other) / a, (ru - other) / a)
        } else {
            ((ru - other) / a, (rl - other) / a)
        };
        if l.is_finite() {
            need_lo = need_lo.max(l);
        }
        if h.is_finite() {
            need_hi = need_hi.min(h);
        }
    }
    // `need_hi` starts at the column's own upper bound; it is only a "row" upper
    // bound when a row actually tightened it below that. This distinction matters:
    // `min` is upper-bounded by its rows (`min <= load`), but `dev`/`range` are
    // lower-bounded by theirs and merely carry a large finite column upper bound.
    let row_upper_only =
        need_hi.is_finite() && (hi.is_infinite() || need_hi < hi - 1e-9 * (1.0 + hi.abs()));
    if need_lo.is_finite() {
        // Normally the smallest feasible value; but if the variable is only
        // *upper*-bounded by its rows (e.g. `min <= load`), take the largest so
        // `range = peak - min` is minimised.
        let target = if row_upper_only && need_lo <= lo + 1e-9 {
            need_hi
        } else {
            need_lo
        };
        let mut v = target.max(lo).min(hi);
        if need_hi.is_finite() {
            v = v.min(need_hi);
        }
        x[c] = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{check_solution, VarType, INF};

    /// Build a planning-shaped model natively, mirroring tools/gen-planning.mjs
    /// and the `planning_like` builder in `heuristic.rs`: integer x[u,p,r],
    /// binary z[u,p,r], per-(p,r) load = sum dur*x, cap, and per-session
    /// peak/min/range/deviation auxiliaries. Cap is 1.5x a witness load so the
    /// instance is feasible (unless `cap_override` forces otherwise).
    /// Build a planning-shaped model and return it together with the `x`/`z`
    /// index maps and unit match counts, so tests can forge start points.
    fn planning_build(
        units: usize,
        sessions: usize,
        rings: usize,
        cap_override: Option<f64>,
    ) -> (Model, Vec<Vec<Vec<u32>>>, Vec<Vec<Vec<u32>>>, Vec<f64>) {
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

        let mut wload = vec![vec![0.0f64; rings]; sessions];
        for u in 0..units {
            wload[u % sessions][u % rings] += matches[u] * dur[u];
        }
        let witness_max = wload.iter().flatten().cloned().fold(0.0, f64::max);
        let cap = cap_override.unwrap_or_else(|| (witness_max * 1.5).ceil());

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
        (m, x, z, matches)
    }

    fn planning_like_cap(
        units: usize,
        sessions: usize,
        rings: usize,
        cap_override: Option<f64>,
    ) -> Model {
        planning_build(units, sessions, rings, cap_override).0
    }

    fn planning_like(units: usize, sessions: usize, rings: usize) -> Model {
        planning_like_cap(units, sessions, rings, None)
    }

    #[test]
    fn repairs_small_planning_like() {
        let m = planning_like(8, 3, 5);
        let x0 = crate::initial_point(&m);
        let r = repair(&m, &x0, 200_000, 42).expect("repair should find a feasible plan");
        let chk = check_solution(&m, &r.x, 1e-6);
        assert!(chk.feasible, "check_solution: {:?}", chk);
        assert!(r.moves < 200_000);
    }

    #[test]
    fn repairs_large_planning_like() {
        let m = planning_like(40, 3, 5);
        let x0 = crate::initial_point(&m);
        let r = repair(&m, &x0, 500_000, 7).expect("repair should find a feasible plan");
        let chk = check_solution(&m, &r.x, 1e-6);
        assert!(chk.feasible, "check_solution: {:?}", chk);
    }

    #[test]
    fn repairs_from_near_feasible_heuristic_point() {
        // The intended production path: `find_feasible` stalls near-feasible,
        // then `repair` closes the coupled `onesplit`/load rows.
        let m = planning_like(8, 3, 5);
        let opts = crate::options::Options {
            seed: 11,
            ..Default::default()
        };
        // Bounded so the heuristic is guaranteed to leave work for `repair`.
        let near = crate::heuristic::find_feasible(&m, &opts, &crate::StdClock::new(), 4_000);
        let x0 = near
            .map(|f| f.x)
            .unwrap_or_else(|| crate::initial_point(&m));
        let r = repair(&m, &x0, 200_000, 11).expect("repair should finish the plan");
        assert!(check_solution(&m, &r.x, 1e-6).feasible);
    }

    #[test]
    fn infeasible_instance_returns_none() {
        // cap = 0 while every unit must place >= 1 match: no feasible point.
        let m = planning_like_cap(6, 2, 3, Some(0.0));
        let x0 = crate::initial_point(&m);
        let r = repair(&m, &x0, 50_000, 3);
        assert!(r.is_none(), "infeasible instance must not report a repair");
    }

    #[test]
    fn returned_solution_passes_check_solution() {
        for (units, seed) in [(8usize, 1u64), (16usize, 2), (24usize, 5)] {
            let m = planning_like(units, 3, 5);
            let x0 = crate::initial_point(&m);
            let r = repair(&m, &x0, 300_000, seed)
                .unwrap_or_else(|| panic!("units={units}: repair returned None"));
            let chk = check_solution(&m, &r.x, 1e-6);
            assert!(chk.feasible, "units={units}: {:?}", chk);
        }
    }

    #[test]
    fn repair_is_deterministic() {
        let m = planning_like(10, 3, 4);
        let x0 = crate::initial_point(&m);
        let a = repair(&m, &x0, 200_000, 99).expect("feasible");
        let b = repair(&m, &x0, 200_000, 99).expect("feasible");
        assert_eq!(a.x, b.x);
        assert_eq!(a.moves, b.moves);
    }

    #[test]
    fn repairs_transfers_from_cap_congested_start() {
        // Forge a start where `tot` is already met but every unit is stacked on
        // session 0 / ring 0, blowing the cap. Only the coupled transfer move
        // (move a whole match to another ring, updating z + load + aux) can fix
        // this; single-variable moves cannot because they break `tot`/`onesplit`.
        for units in [8usize, 40] {
            let (m, x, z, matches) = planning_build(units, 3, 5, None);
            let mut x0 = vec![0.0f64; m.num_col];
            for u in 0..units {
                x0[x[u][0][0] as usize] = matches[u];
                x0[z[u][0][0] as usize] = 1.0;
            }
            let r = repair(&m, &x0, 2_000_000, units as u64 + 1)
                .unwrap_or_else(|| panic!("units={units}: transfer repair failed"));
            let chk = check_solution(&m, &r.x, 1e-6);
            assert!(chk.feasible, "units={units}: {:?}", chk);
        }
    }

    /// Inlined planning model (`sum dur*x` directly in cap/peak/dev rows, no
    /// `load` column) — the shape of the real `planning-*.lp` fixtures.
    fn planning_inlined(units: usize, sessions: usize, rings: usize) -> Model {
        use crate::model::VarId;
        let mut m = Model::new();
        let matches: Vec<f64> = (0..units).map(|u| (1 + (u * 37) % 4) as f64).collect();
        let dur: Vec<f64> = (0..units)
            .map(|u| (100 + (u * 53) % 9 * 100) as f64)
            .collect();
        let mut x = vec![vec![vec![0u32; rings]; sessions]; units];
        let mut z = vec![vec![vec![0u32; rings]; sessions]; units];
        for u in 0..units {
            for p in 0..sessions {
                for r in 0..rings {
                    let xi = m.add_col(0.0, matches[u]).0;
                    m.set_integrality(VarId(xi), VarType::Integer);
                    let zi = m.add_col(0.0, 1.0).0;
                    m.set_integrality(VarId(zi), VarType::Integer);
                    x[u][p][r] = xi;
                    z[u][p][r] = zi;
                }
            }
        }
        for u in 0..units {
            let mut terms = Vec::new();
            for p in 0..sessions {
                for r in 0..rings {
                    terms.push((x[u][p][r], 1.0));
                }
            }
            m.add_row(matches[u], matches[u], &terms);
            for p in 0..sessions {
                for r in 0..rings {
                    m.add_row(-INF, 0.0, &[(x[u][p][r], 1.0), (z[u][p][r], -matches[u])]);
                }
                let zt: Vec<(u32, f64)> = (0..rings).map(|r| (z[u][p][r], 1.0)).collect();
                m.add_row(-INF, 1.0, &zt);
            }
        }
        let mut wload = vec![vec![0.0f64; rings]; sessions];
        for u in 0..units {
            wload[u % sessions][u % rings] += matches[u] * dur[u];
        }
        let cap = (wload.iter().flatten().cloned().fold(0.0, f64::max) * 1.2).ceil() + 1.0;
        for p in 0..sessions {
            let total = m.add_col(0.0, INF).0;
            let mut tt = vec![(total, 1.0)];
            for u in 0..units {
                for r in 0..rings {
                    tt.push((x[u][p][r], -dur[u]));
                }
            }
            m.add_row(0.0, 0.0, &tt);
            let peak = m.add_col(0.0, INF).0;
            let minv = m.add_col(0.0, INF).0;
            m.set_cost(crate::model::VarId(peak), 0.05);
            for r in 0..rings {
                let mut loadr: Vec<(u32, f64)> = (0..units).map(|u| (x[u][p][r], dur[u])).collect();
                m.add_row(-INF, cap, &loadr);
                let mut pk = loadr.clone();
                pk.push((peak, -1.0));
                m.add_row(-INF, 0.0, &pk);
                let mut mn = loadr.clone();
                mn.push((minv, -1.0));
                m.add_row(0.0, INF, &mn);
                let dev = m.add_col(0.0, INF).0;
                m.set_cost(crate::model::VarId(dev), 0.01);
                let mut hi = vec![(dev, -1.0), (total, -1.0)];
                let mut lo = vec![(dev, -1.0), (total, 1.0)];
                for u in 0..units {
                    hi.push((x[u][p][r], rings as f64 * dur[u]));
                    lo.push((x[u][p][r], -(rings as f64) * dur[u]));
                }
                m.add_row(-INF, 0.0, &hi);
                m.add_row(-INF, 0.0, &lo);
                loadr.clear();
            }
            let range = m.add_col(0.0, INF).0;
            m.set_cost(crate::model::VarId(range), 1.0);
            m.add_row(0.0, INF, &[(range, 1.0), (peak, -1.0), (minv, 1.0)]);
        }
        m
    }

    #[test]
    fn construct_inlined_planning_is_feasible() {
        for (units, sessions, rings) in [(8usize, 2, 3), (24, 3, 5), (60, 3, 5)] {
            let m = planning_inlined(units, sessions, rings);
            let x = construct_inlined_planning(&m)
                .unwrap_or_else(|| panic!("units={units}: construct returned None"));
            let chk = check_solution(&m, &x, 1e-6);
            assert!(chk.feasible, "units={units}: {:?}", chk);
            // The generic single-variable heuristic stalls here, so a feasible
            // point from the constructor is the whole point.
            assert!(m.objective(&x) > 0.0);
        }
    }

    #[test]
    fn improve_planning_returns_feasible_no_worse_objective() {
        use crate::solve::StdClock;
        for (units, sessions, rings) in [(24usize, 3, 5), (60, 3, 5)] {
            let m = planning_inlined(units, sessions, rings);
            let x0 = construct_inlined_planning(&m)
                .unwrap_or_else(|| panic!("units={units}: construct returned None"));
            let o0 = m.objective(&x0);
            let (o1, x1) = improve_planning(&m, &x0, &StdClock::new(), 2.0, 1)
                .unwrap_or_else(|| panic!("units={units}: improve returned None"));
            let chk = check_solution(&m, &x1, 1e-6);
            assert!(chk.feasible, "units={units}: {:?}", chk);
            // The objective local search must never make a feasible point worse.
            assert!(
                o1 <= o0 + 1e-6,
                "units={units}: objective {o1} worse than start {o0}"
            );
        }
    }
}
