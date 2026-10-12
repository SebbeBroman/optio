//! Data-oriented LP/MIP model.
//!
//! Everything is stored in flat, contiguous arrays (struct-of-arrays) so hot
//! loops are cache-friendly and the model can live in one WebAssembly
//! linear-memory region that JS can view zero-copy. There are no per-variable
//! objects and no pointers: only indices into these arrays.

/// Value used for "no bound". Matches HiGHS' `kHighsInf`.
pub const INF: f64 = 1e30;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct VarId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct RowId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[repr(u8)]
pub enum VarType {
    #[default]
    Continuous = 0,
    Integer = 1,
    Binary = 2,
}

impl VarType {
    #[inline]
    pub fn is_integral(self) -> bool {
        !matches!(self, VarType::Continuous)
    }
}

/// Data-oriented LP/MIP model. The constraint matrix is stored CSR by row.
#[derive(Clone, Debug)]
pub struct Model {
    pub num_col: usize,
    pub num_row: usize,

    // Column-major data (length = num_col).
    pub col_cost: Vec<f64>,
    pub col_lower: Vec<f64>,
    pub col_upper: Vec<f64>,
    pub col_type: Vec<VarType>,

    // Row bounds (length = num_row).
    pub row_lower: Vec<f64>,
    pub row_upper: Vec<f64>,

    // Constraint matrix, CSR by row (lengths = num_row + 1, nnz, nnz).
    pub a_start: Vec<u32>,
    pub a_index: Vec<u32>,
    pub a_value: Vec<f64>,
}

impl Model {
    pub fn new() -> Self {
        Model {
            num_col: 0,
            num_row: 0,
            col_cost: Vec::new(),
            col_lower: Vec::new(),
            col_upper: Vec::new(),
            col_type: Vec::new(),
            row_lower: Vec::new(),
            row_upper: Vec::new(),
            a_start: vec![0],
            a_index: Vec::new(),
            a_value: Vec::new(),
        }
    }

    #[inline]
    pub fn num_nz(&self) -> usize {
        self.a_value.len()
    }

    /// Append a column and return its id.
    #[inline]
    pub fn add_col(&mut self, lower: f64, upper: f64) -> VarId {
        let id = VarId(self.num_col as u32);
        self.col_cost.push(0.0);
        self.col_lower.push(lower);
        self.col_upper.push(upper);
        self.col_type.push(VarType::Continuous);
        self.num_col += 1;
        id
    }

    /// Append a row given `(column index, coefficient)` entries.
    #[inline]
    pub fn add_row(&mut self, lower: f64, upper: f64, entries: &[(u32, f64)]) -> RowId {
        let id = RowId(self.num_row as u32);
        self.row_lower.push(lower);
        self.row_upper.push(upper);
        for &(col, val) in entries {
            self.a_index.push(col);
            self.a_value.push(val);
        }
        self.a_start.push(self.a_index.len() as u32);
        self.num_row += 1;
        id
    }

    /// Bulk-append a block of columns and rows from flat CSR arrays, in one
    /// pass with no per-row temporaries. `starts` has `row_lower.len() + 1`
    /// entries; `indices`/`values` are the CSR payload and `indices` are
    /// relative to the start of the block (offset by the current `num_col`).
    /// This is the entry point the wasm binding uses for a whole model.
    #[allow(clippy::too_many_arguments)]
    pub fn add_csr(
        &mut self,
        num_cols: usize,
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
        let base_col = self.num_col as u32;
        self.col_cost.extend_from_slice(&col_cost[..num_cols]);
        self.col_lower.extend_from_slice(&col_lower[..num_cols]);
        self.col_upper.extend_from_slice(&col_upper[..num_cols]);
        for c in 0..num_cols {
            let ty = match integrality.get(c).copied().unwrap_or(0) {
                0 => VarType::Continuous,
                2 => VarType::Binary,
                _ => VarType::Integer,
            };
            self.col_type.push(ty);
        }
        self.num_col += num_cols;

        let nrows = row_lower.len();
        self.row_lower.extend_from_slice(row_lower);
        self.row_upper.extend_from_slice(row_upper);
        let nnz_base = self.a_index.len() as u32;
        self.a_start
            .extend((0..nrows).map(|r| nnz_base + starts[r + 1]));
        self.a_index.extend(indices.iter().map(|&i| i + base_col));
        self.a_value.extend_from_slice(values);
        self.num_row += nrows;
    }

    #[inline]
    pub fn set_cost(&mut self, col: VarId, cost: f64) {
        self.col_cost[col.0 as usize] = cost;
    }

    #[inline]
    pub fn set_integrality(&mut self, col: VarId, ty: VarType) {
        self.col_type[col.0 as usize] = ty;
    }

    /// Row `r` as `(index, value)` pairs.
    #[inline]
    pub fn row(&self, r: RowId) -> impl Iterator<Item = (u32, f64)> + '_ {
        let lo = self.a_start[r.0 as usize] as usize;
        let hi = self.a_start[r.0 as usize + 1] as usize;
        (lo..hi).map(move |k| (self.a_index[k], self.a_value[k]))
    }

    /// Evaluate the linear objective `c'x`.
    pub fn objective(&self, x: &[f64]) -> f64 {
        self.col_cost.iter().zip(x).map(|(c, xi)| c * xi).sum()
    }
}

impl Default for Model {
    fn default() -> Self {
        Model::new()
    }
}

/// Result of validating a candidate solution against the model.
#[derive(Clone, Copy, Debug, Default)]
pub struct SolutionCheck {
    pub feasible: bool,
    pub max_row_violation: f64,
    pub max_bound_violation: f64,
    pub max_integrality_violation: f64,
    pub objective: f64,
}

impl SolutionCheck {
    pub fn is_feasible(&self, tol: f64) -> bool {
        self.max_row_violation <= tol
            && self.max_bound_violation <= tol
            && self.max_integrality_violation <= tol
    }
}

/// Validate `x` against the model. This is the oracle used by both unit tests
/// and the differential harness: it never trusts the solver's own report.
pub fn check_solution(model: &Model, x: &[f64], tol: f64) -> SolutionCheck {
    assert_eq!(x.len(), model.num_col, "solution length mismatch");
    let mut out = SolutionCheck {
        feasible: false,
        objective: model.objective(x),
        ..Default::default()
    };

    for (c, &v) in x.iter().enumerate() {
        // f64::max ignores NaNs, so without this guard a non-finite candidate
        // could appear to have zero violations and be accepted as an incumbent.
        if !v.is_finite() {
            out.max_bound_violation = f64::INFINITY;
            return out;
        }
        let lo = model.col_lower[c];
        let hi = model.col_upper[c];
        out.max_bound_violation = out.max_bound_violation.max(lo - v).max(v - hi);
        if model.col_type[c].is_integral() {
            let frac = (v - v.round()).abs();
            out.max_integrality_violation = out.max_integrality_violation.max(frac);
        }
    }

    for r in 0..model.num_row {
        let mut lhs = 0.0;
        for (col, val) in model.row(RowId(r as u32)) {
            lhs += val * x[col as usize];
        }
        let lo = model.row_lower[r];
        let hi = model.row_upper[r];
        out.max_row_violation = out.max_row_violation.max(lo - lhs).max(lhs - hi);
    }

    out.feasible = out.is_feasible(tol);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_build_and_check() {
        // min -x - 10y  s.t. x + 7y <= 17.5, 0<=x<=3.5, y>=0 integer.
        let mut m = Model::new();
        let x = m.add_col(0.0, 3.5);
        let y = m.add_col(0.0, INF);
        m.set_cost(x, -1.0);
        m.set_cost(y, -10.0);
        m.set_integrality(y, VarType::Integer);
        m.add_row(-INF, 17.5, &[(x.0, 1.0), (y.0, 7.0)]);

        assert_eq!(m.num_col, 2);
        assert_eq!(m.num_row, 1);
        assert_eq!(m.num_nz(), 2);

        let ok = check_solution(&m, &[3.5, 2.0], 1e-9);
        assert!(ok.feasible);
        assert!((ok.objective - -23.5).abs() < 1e-9);

        let bad = check_solution(&m, &[3.5, 3.0], 1e-9);
        assert!(!bad.feasible); // row violated
    }

    #[test]
    fn add_csr_matches_incremental_build() {
        // Same model built incrementally and via the bulk CSR path.
        let mut a = Model::new();
        let x = a.add_col(0.0, 3.5);
        let y = a.add_col(0.0, INF);
        a.set_cost(x, -1.0);
        a.set_cost(y, -10.0);
        a.set_integrality(y, VarType::Integer);
        a.add_row(-INF, 17.5, &[(x.0, 1.0), (y.0, 7.0)]);
        a.add_row(1.0, 1.0, &[(x.0, 1.0), (y.0, 1.0)]);

        let mut b = Model::new();
        b.add_csr(
            2,
            &[-1.0, -10.0],
            &[0.0, 0.0],
            &[3.5, INF],
            &[0, 1],
            &[-INF, 1.0],
            &[17.5, 1.0],
            &[0, 2, 4],
            &[0, 1, 0, 1],
            &[1.0, 7.0, 1.0, 1.0],
        );

        assert_eq!(
            (b.num_col, b.num_row, b.num_nz()),
            (a.num_col, a.num_row, a.num_nz())
        );
        assert_eq!(b.a_start, a.a_start);
        assert_eq!(b.a_index, a.a_index);
        assert_eq!(b.a_value, a.a_value);
        assert_eq!(b.col_cost, a.col_cost);
        assert_eq!(b.col_lower, a.col_lower);
        assert_eq!(b.col_upper, a.col_upper);
        assert_eq!(b.col_type, a.col_type);
        // The oracle agrees on a shared point.
        let chk_a = check_solution(&a, &[2.0, 1.0], 1e-9);
        let chk_b = check_solution(&b, &[2.0, 1.0], 1e-9);
        assert_eq!(chk_a.feasible, chk_b.feasible);
        assert!((chk_a.objective - chk_b.objective).abs() < 1e-12);
    }
}
