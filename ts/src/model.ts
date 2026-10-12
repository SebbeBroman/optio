// Data-oriented LP/MIP model (port of `crates/core/src/model.rs`).
//
// Everything lives in flat, contiguous arrays (struct-of-arrays) so hot loops
// stay cache friendly. There are no per-variable objects and no owning pointers,
// only indices into these arrays.

/** Value used for "no bound". Matches HiGHS' `kHighsInf`. */
export const INF: number = 1e30;

/** Variable kind, stored as a byte-valued code (matches the Rust `#[repr(u8)]`). */
export type VarType = 0 | 1 | 2;
export const VAR_CONTINUOUS: VarType = 0;
export const VAR_INTEGER: VarType = 1;
export const VAR_BINARY: VarType = 2;

/** True for integer and binary variables. */
export function isIntegral(t: VarType): boolean {
  return t !== VAR_CONTINUOUS;
}

/** Rust `f64::max`: ignores NaN (returns the other operand). */
export function fmax(a: number, b: number): number {
  if (a !== a) return b;
  if (b !== b) return a;
  return a < b ? b : a;
}

/** Rust `f64::min`: ignores NaN (returns the other operand). */
export function fmin(a: number, b: number): number {
  if (a !== a) return b;
  if (b !== b) return a;
  return a > b ? b : a;
}

/**
 * A fixed-length dense array of `n` copies of `v` (Rust `vec![v; n]`).
 */
export function filled<T>(n: number, v: T): T[] {
  const a: T[] = [];
  for (let i = 0; i < n; i++) a.push(v);
  return a;
}

/**
 * Data-oriented LP/MIP model. The constraint matrix is stored CSR by row.
 * The matrix, costs and bounds are plain double arrays; `colType` is a byte
 * code per column.
 */
export class Model {
  numCol: number = 0;
  numRow: number = 0;

  // Column-major data (length = numCol).
  colCost: number[] = [];
  colLower: number[] = [];
  colUpper: number[] = [];
  colType: VarType[] = [];

  // Row bounds (length = numRow).
  rowLower: number[] = [];
  rowUpper: number[] = [];

  // Constraint matrix, CSR by row (lengths = numRow + 1, nnz, nnz).
  aStart: number[] = [0];
  aIndex: number[] = [];
  aValue: number[] = [];

  numNz(): number {
    return this.aValue.length;
  }

  /** Append a column and return its id (index). */
  addCol(lower: number, upper: number): number {
    const id = this.numCol;
    this.colCost.push(0);
    this.colLower.push(lower);
    this.colUpper.push(upper);
    this.colType.push(VAR_CONTINUOUS);
    this.numCol += 1;
    return id;
  }

  /** Append a row given `(column index, coefficient)` entries. */
  addRow(lower: number, upper: number, entries: readonly (readonly [number, number])[]): number {
    const id = this.numRow;
    this.rowLower.push(lower);
    this.rowUpper.push(upper);
    for (let i = 0; i < entries.length; i++) {
      this.aIndex.push(entries[i][0]);
      this.aValue.push(entries[i][1]);
    }
    this.aStart.push(this.aIndex.length);
    this.numRow += 1;
    return id;
  }

  /**
   * Bulk-append a block of columns and rows from flat CSR arrays, in one pass
   * with no per-row temporaries. `starts` has `rowLower.length + 1` entries;
   * `indices`/`values` are the CSR payload and `indices` are relative to the
   * start of the block (offset by the current `numCol`).
   */
  addCsr(
    numCols: number,
    colCost: readonly number[],
    colLower: readonly number[],
    colUpper: readonly number[],
    integrality: readonly number[] | null,
    rowLower: readonly number[],
    rowUpper: readonly number[],
    starts: readonly number[],
    indices: readonly number[],
    values: readonly number[],
  ): void {
    const baseCol = this.numCol;
    for (let c = 0; c < numCols; c++) {
      this.colCost.push(colCost[c]);
      this.colLower.push(colLower[c]);
      this.colUpper.push(colUpper[c]);
      const raw = integrality !== null && c < integrality.length ? integrality[c] : 0;
      const ty: VarType = raw === 0 ? VAR_CONTINUOUS : raw === 2 ? VAR_BINARY : VAR_INTEGER;
      this.colType.push(ty);
    }
    this.numCol += numCols;

    const nrows = rowLower.length;
    const nnzBase = this.aIndex.length;
    for (let r = 0; r < nrows; r++) {
      this.rowLower.push(rowLower[r]);
      this.rowUpper.push(rowUpper[r]);
      this.aStart.push(nnzBase + starts[r + 1]);
    }
    for (let k = 0; k < indices.length; k++) {
      this.aIndex.push(indices[k] + baseCol);
      this.aValue.push(values[k]);
    }
    this.numRow += nrows;
  }

  setCost(col: number, cost: number): void {
    this.colCost[col] = cost;
  }

  setIntegrality(col: number, ty: VarType): void {
    this.colType[col] = ty;
  }

  /** Evaluate the linear objective `c'x`. */
  objective(x: readonly number[]): number {
    let s = 0;
    for (let j = 0; j < this.numCol; j++) {
      s += this.colCost[j] * x[j];
    }
    return s;
  }

  /** Deep clone: the pipeline builds reduced/strengthened copies. */
  clone(): Model {
    const m = new Model();
    m.numCol = this.numCol;
    m.numRow = this.numRow;
    m.colCost = this.colCost.slice();
    m.colLower = this.colLower.slice();
    m.colUpper = this.colUpper.slice();
    m.colType = this.colType.slice();
    m.rowLower = this.rowLower.slice();
    m.rowUpper = this.rowUpper.slice();
    m.aStart = this.aStart.slice();
    m.aIndex = this.aIndex.slice();
    m.aValue = this.aValue.slice();
    return m;
  }
}

/** Result of validating a candidate solution against the model. */
export interface SolutionCheck {
  feasible: boolean;
  maxRowViolation: number;
  maxBoundViolation: number;
  maxIntegralityViolation: number;
  objective: number;
}

export function madeSolutionCheck(objective: number): SolutionCheck {
  return {
    feasible: false,
    maxRowViolation: 0,
    maxBoundViolation: 0,
    maxIntegralityViolation: 0,
    objective,
  };
}

export function isCheckFeasible(c: SolutionCheck, tol: number): boolean {
  return c.maxRowViolation <= tol && c.maxBoundViolation <= tol && c.maxIntegralityViolation <= tol;
}

/**
 * Validate `x` against the model. This is the oracle used by both unit tests
 * and the differential harness: it never trusts the solver's own report.
 */
export function checkSolution(model: Model, x: readonly number[], tol: number): SolutionCheck {
  const out = madeSolutionCheck(model.objective(x));

  const n = model.numCol;
  for (let c = 0; c < n; c++) {
    const v = x[c]!;
    // Non-finite candidates must never look feasible: `fmax` would ignore a
    // NaN violation and zero it out.
    if (!isFinite(v)) {
      out.maxBoundViolation = Infinity;
      return out;
    }
    const lo = model.colLower[c];
    const hi = model.colUpper[c];
    out.maxBoundViolation = fmax(out.maxBoundViolation, fmax(lo - v, v - hi));
    if (isIntegral(model.colType[c])) {
      const frac = Math.abs(v - Math.round(v));
      out.maxIntegralityViolation = fmax(out.maxIntegralityViolation, frac);
    }
  }

  const m = model.numRow;
  for (let r = 0; r < m; r++) {
    let lhs = 0;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      lhs += model.aValue[k] * x[model.aIndex[k]];
    }
    const lo = model.rowLower[r];
    const hi = model.rowUpper[r];
    out.maxRowViolation = fmax(out.maxRowViolation, fmax(lo - lhs, lhs - hi));
  }

  out.feasible = isCheckFeasible(out, tol);
  return out;
}
