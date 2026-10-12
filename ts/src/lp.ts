// LP relaxation: a bounded-variable, two-phase revised primal simplex.
// (Port of `crates/core/src/lp.rs`.)
//
// Form solved (the model's own bounds, integrality ignored):
//
//   minimise   c'x
//   subject to  L_r <= a_r' x <= U_r      (rows)
//               l_j <=    x_j   <= u_j    (columns)
//
// Internals: each row gets a *logical* variable `s_r` with `a_r'x - s_r = 0`
// and bounds `[L_r, U_r]`. Rows whose origin is infeasible get an *artificial*
// variable for Phase 1. The basis is `m x m`; `B^{-1}` is a sparse LU (partial
// pivoting, dense-accumulator left-looking factorisation) refined by a product
// of eta matrices and refactorised every `refactorInterval` pivots.

import { Model, INF, filled } from "./model.ts";
import type { Clock } from "./clock.ts";

/** Outcome of an LP solve. */
export const LpStatus = {
  Optimal: 0,
  Infeasible: 1,
  Unbounded: 2,
  IterationLimit: 3,
  TimeLimit: 4,
  Numerical: 5,
} as const;
export type LpStatus = (typeof LpStatus)[keyof typeof LpStatus];

/** Tolerances and limits for the simplex. */
export interface LpOptions {
  primalTol: number;
  dualTol: number;
  maxIterations: number;
  refactorInterval: number;
  timeLimitSecs: number;
}

export function defaultLpOptions(): LpOptions {
  return {
    primalTol: 1e-7,
    dualTol: 1e-7,
    maxIterations: 200000,
    refactorInterval: 128,
    timeLimitSecs: 3600.0,
  };
}

/** Aggregate LP work. Seconds sum across workers, rather than wall-clock time. */
export class LpStats {
  solves = 0;
  seconds = 0;
  phase1Iterations = 0;
  phase2Iterations = 0;
  warmSolves = 0;
  warmFallbacks = 0;

  /** Mutating `+=` equivalent. */
  add(rhs: LpStats): void {
    this.solves += rhs.solves;
    this.seconds += rhs.seconds;
    this.phase1Iterations += rhs.phase1Iterations;
    this.phase2Iterations += rhs.phase2Iterations;
    this.warmSolves += rhs.warmSolves;
    this.warmFallbacks += rhs.warmFallbacks;
  }
}

export function makeLpStats(): LpStats {
  return new LpStats();
}

/**
 * Snapshot in scaled LP coordinates; shared by siblings. A fresh numerical
 * factorization is built on restore, while simplex iterations reuse the basis.
 */
export class LpBasis {
  basis: number[];
  x: number[];
  constructor(basis: number[], x: number[]) {
    this.basis = basis;
    this.x = x;
  }
}

/** LP result in the original column space. */
export class LpSolution {
  status: LpStatus = LpStatus.Numerical;
  objective = 0;
  /** Lagrangian lower bound using true costs and all variable bounds. */
  dualBound: number | null = null;
  colValue: number[] = [];
  colDual: number[] = [];
  rowDual: number[] = [];
  iterations = 0;
  stats: LpStats = new LpStats();
  basis: LpBasis | null = null;
}

const MAXUSIZE = 0x3fffffff; // usize::MAX sentinel (indices are far smaller)

/** A sparse column, stored as parallel index/value arrays (reused in place). */
class SparseCol {
  idx: number[] = [];
  val: number[] = [];
  clear(): void {
    this.idx.length = 0;
    this.val.length = 0;
  }
  push(i: number, v: number): void {
    this.idx.push(i);
    this.val.push(v);
  }
}

/**
 * Detect equality rows implied by other equality rows (rank-deficient rows).
 * Returns `drop[r] = true` for redundant rows. Rows whose coefficients reduce
 * to zero but whose right-hand side is nonzero are kept (phase 1 flags the
 * resulting infeasibility).
 */
function redundantEqualities(model: Model): boolean[] {
  const n = model.numCol;
  const m = model.numRow;
  const drop: boolean[] = filled<boolean>(m, false);
  // `piv[c]` = (normalized reduced row, reduced rhs) keyed by its pivot column.
  const pivCols: number[][] = new Array(n);
  const pivVals: number[][] = new Array(n);
  const pivRhs: number[] = filled<number>(n, 0);
  const pivSet: boolean[] = filled<boolean>(n, false);
  const w: number[] = filled<number>(n, 0);
  const list: number[] = [];

  for (let r = 0; r < m; r++) {
    const lo = model.rowLower[r];
    const hi = model.rowUpper[r];
    const scale = 1 + Math.max(Math.abs(hi), Math.abs(lo));
    if (Math.abs(hi - lo) > 1e-9 * scale) {
      continue; // only equalities participate
    }
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    if (ks === ke) {
      continue; // empty row: leave for phase 1
    }
    let maxc = 0;
    for (let k = ks; k < ke; k++) {
      maxc = Math.max(maxc, Math.abs(model.aValue[k]));
    }
    if (maxc === 0) {
      continue;
    }
    const inv = 1 / maxc;
    let rhs = lo * inv;
    list.length = 0;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      if (w[c] === 0) {
        list.push(c);
      }
      w[c!] += model.aValue[k] * inv;
    }
    let head = 0;
    while (head < list.length) {
      const c = list[head];
      head += 1;
      const v = w[c];
      if (v === 0) {
        continue;
      }
      if (pivSet[c]) {
        const prow = pivCols[c];
        const pval = pivVals[c];
        for (let t = 0; t < prow.length; t++) {
          const cc = prow[t];
          if (w[cc] === 0) {
            list.push(cc);
          }
          w[cc!] -= v * pval[t];
        }
        rhs -= v * pivRhs[c];
        w[c] = 0;
      }
    }
    let pivotCol = -1;
    for (let t = 0; t < list.length; t++) {
      if (Math.abs(w[list[t]]) > 1e-10) {
        pivotCol = list[t];
        break;
      }
    }
    if (pivotCol < 0) {
      if (Math.abs(rhs) <= 1e-8) {
        drop[r] = true;
      }
    } else {
      const pivval = w[pivotCol];
      const prow: number[] = [];
      const pval: number[] = [];
      for (let t = 0; t < list.length; t++) {
        const c = list[t];
        if (Math.abs(w[c]) > 1e-10) {
          prow.push(c);
          pval.push(w[c] / pivval);
        }
      }
      pivCols[pivotCol] = prow;
      pivVals[pivotCol] = pval;
      pivRhs[pivotCol] = rhs / pivval;
      pivSet[pivotCol] = true;
    }
    for (let t = 0; t < list.length; t++) {
      w[list[t]] = 0;
    }
  }
  return drop;
}

/** `INF` (1e30) -> true infinity for the solver. */
function toLo(v: number): number {
  return v <= -INF / 2 ? -Infinity : v;
}
function toHi(v: number): number {
  return v >= INF / 2 ? Infinity : v;
}

/** Branching uses an absolute integer tolerance; never branch from a point
 * materially outside a child's box, even if scaled simplex tolerances allow it. */
function respectsBounds(x: readonly number[], lo: readonly number[], hi: readonly number[]): boolean {
  for (let j = 0; j < x.length; j++) {
    const v = x[j];
    const l = lo[j];
    const u = hi[j];
    if (!(isFinite(v!) && v >= l - 1e-6 && v <= u + 1e-6)) {
      return false;
    }
  }
  return true;
}

// ---------------------------------------------------------------------------
// Sparse LU (partial pivoting, dense-accumulator left-looking factorisation)
// ---------------------------------------------------------------------------

/**
 * Data-oriented sparse LU: L and U are stored as flat, compressed columns
 * (`start`/`idx`/`val`) rather than nested arrays, so the forward/back
 * substitutions stream contiguous memory instead of chasing pointers.
 */
class SparseLu {
  n: number;
  work: Float64Array;
  /** `perm[k]` = original row at elimination position `k`. */
  perm: number[];
  /** `pinv[r]` = position of original row `r`. */
  pinv: number[];
  lStart: number[];
  lIdx: number[];
  lVal: number[];
  uStart: number[];
  uIdx: number[];
  uVal: number[];
  diag: number[];
  singular: boolean;

  constructor(
    n: number,
    work: Float64Array,
    perm: number[],
    pinv: number[],
    lStart: number[],
    lIdx: number[],
    lVal: number[],
    uStart: number[],
    uIdx: number[],
    uVal: number[],
    diag: number[],
    singular: boolean,
  ) {
    this.n = n;
    this.work = work;
    this.perm = perm;
    this.pinv = pinv;
    this.lStart = lStart;
    this.lIdx = lIdx;
    this.lVal = lVal;
    this.uStart = uStart;
    this.uIdx = uIdx;
    this.uVal = uVal;
    this.diag = diag;
    this.singular = singular;
  }

  static identity(n: number): SparseLu {
    const perm: number[] = new Array(n);
    const pinv: number[] = new Array(n);
    for (let i = 0; i < n; i++) {
      perm[i] = i;
      pinv[i] = i;
    }
    return new SparseLu(
      n,
      new Float64Array(n),
      perm,
      pinv,
      filled<number>(n + 1, 0),
      [],
      [],
      filled<number>(n + 1, 0),
      [],
      [],
      filled<number>(n, 1),
      false,
    );
  }

  /** Factor the basis whose columns are given sparsely (`cols[k]` = column k). */
  static factor(cols: SparseCol[], n: number, sparse: boolean): SparseLu {
    const TINY = 1e-12;
    const perm: number[] = filled<number>(n, 0);
    const pinv: number[] = filled<number>(n, MAXUSIZE);
    const used: boolean[] = filled<boolean>(n, false);
    const diag: number[] = filled<number>(n, 1);
    const x: Float64Array = new Float64Array(n); // indexed by ORIGINAL row
    const mark: boolean[] = filled<boolean>(n, false);
    const active: number[] = [];
    let singular = false;
    const lStart: number[] = [];
    const lIdx: number[] = [];
    const lVal: number[] = [];
    const uStart: number[] = [];
    const uIdx: number[] = [];
    const uVal: number[] = [];
    // Min-heap over (pivot index j, row r), encoded as j*n + r.
    const heap: number[] = [];

    for (let k = 0; k < n; k++) {
      lStart.push(lIdx.length);
      uStart.push(uIdx.length);
      active.length = 0;
      const col = cols[k];
      const cidx = col.idx;
      const cval = col.val;
      for (let t = 0; t < cidx.length; t++) {
        const ri = cidx[t];
        if (!mark[ri]) {
          mark[ri] = true;
          active.push(ri);
        }
        x[ri!] += cval[t];
      }
      if (sparse) {
        // Sparse basis: walk only the spike's pivot rows via a min-heap
        // (output-sensitive), touching `lStart[j]` columns in order.
        heap.length = 0;
        for (let a = 0; a < active.length; a++) {
          const r = active[a];
          const j = pinv[r];
          if (j < k) {
            heapPush(heap, j * n + r);
          }
        }
        while (heap.length > 0) {
          const key = heapPop(heap);
          const j = Math.floor(key / n);
          const r = key - j * n;
          if (x[r] === 0) {
            continue;
          }
          const u = x[r];
          x[r] = 0;
          uIdx.push(j);
          uVal.push(u);
          const lo = lStart[j];
          const hi = lStart[j + 1];
          for (let t = lo; t < hi; t++) {
            const ii = lIdx[t];
            if (x[ii] === 0 && !mark[ii]) {
              mark[ii] = true;
              active.push(ii);
            }
            x[ii!] -= lVal[t] * u;
          }
          for (let t = lo; t < hi; t++) {
            const ii = lIdx[t];
            const pj = pinv[ii];
            if (pj < k && x[ii] !== 0) {
              heapPush(heap, pj * n + ii);
            }
          }
        }
      } else {
        // Dense basis: a simple pivot-order scan is cheaper than the heap.
        for (let j = 0; j < k; j++) {
          const pj = perm[j];
          const u = x[pj];
          if (u !== 0) {
            x[pj!] = 0;
            uIdx.push(j);
            uVal.push(u);
            const lo = lStart[j];
            const hi = lStart[j + 1];
            for (let t = lo; t < hi; t++) {
              const ii = lIdx[t];
              if (x[ii] === 0 && !mark[ii]) {
                mark[ii] = true;
                active.push(ii);
              }
              x[ii!] -= lVal[t] * u;
            }
          }
        }
      }
      // Partial pivot among the support rows that are not yet pivots.
      let pr = -1;
      let best = 0;
      for (let a = 0; a < active.length; a++) {
        const ri = active[a];
        if (used[ri]) {
          continue;
        }
        const v = Math.abs(x[ri]);
        // Largest magnitude; ties broken by smallest row index (matches a
        // row-order scan), so the factorization is deterministic and stable.
        if (v > best || (v === best && v > 0 && (pr < 0 || ri < pr))) {
          best = v;
          pr = ri;
        }
      }
      if (pr < 0) {
        singular = true;
        for (let r = 0; r < n; r++) {
          if (!used[r]) {
            pr = r;
            break;
          }
        }
        if (pr < 0) pr = 0;
      }
      if (best < TINY) {
        singular = true;
      }
      used[pr] = true;
      perm[k] = pr;
      pinv[pr] = k;
      diag[k] = Math.abs(x[pr]) < TINY ? 1.0 : x[pr];
      const inv = 1.0 / diag[k];
      for (let a = 0; a < active.length; a++) {
        const ri = active[a];
        if (!used[ri] && x[ri] !== 0) {
          lIdx.push(ri);
          lVal.push(x[ri] * inv);
        }
      }
      for (let a = 0; a < active.length; a++) {
        const ri = active[a];
        x[ri!] = 0;
        mark[ri] = false;
      }
    }
    lStart.push(lIdx.length);
    uStart.push(uIdx.length);

    return new SparseLu(n, new Float64Array(n), perm, pinv, lStart, lIdx, lVal, uStart, uIdx, uVal, diag, singular);
  }

  ftran(b: Float64Array): void {
    const n = this.n;
    const z = this.work;
    const perm = this.perm;
    const lStart = this.lStart;
    const lIdx = this.lIdx;
    const lVal = this.lVal;
    const uStart = this.uStart;
    const uIdx = this.uIdx;
    const uVal = this.uVal;
    const pinv = this.pinv;
    for (let k = 0; k < n; k++) {
      z[k] = b[perm[k]];
    }
    // L z = pb (forward; L entries are stored by original row).
    for (let k = 0; k < n; k++) {
      const zk = z[k];
      if (zk !== 0) {
        const lo = lStart[k];
        const hi = lStart[k + 1];
        for (let t = lo; t < hi; t++) {
          z[pinv[lIdx[t]]!] -= lVal[t] * zk;
        }
      }
    }
    // U x = z (back substitution). z is the solution B^{-1}b.
    for (let k = n - 1; k >= 0; k--) {
      const xk = z[k] / this.diag[k];
      const lo = uStart[k];
      const hi = uStart[k + 1];
      for (let t = lo; t < hi; t++) {
        z[uIdx[t]!] -= uVal[t] * xk;
      }
      z[k] = xk;
    }
    for (let k = 0; k < n; k++) {
      b[k] = z[k];
    }
  }

  btran(c: Float64Array): void {
    const n = this.n;
    const perm = this.perm;
    const uStart = this.uStart;
    const uIdx = this.uIdx;
    const uVal = this.uVal;
    const lStart = this.lStart;
    const lIdx = this.lIdx;
    const lVal = this.lVal;
    const pinv = this.pinv;
    const u = this.work;
    for (let k = 0; k < n; k++) {
      u[k] = c[k];
    }
    // U^T u = c (forward).
    for (let k = 0; k < n; k++) {
      let s = u[k];
      const lo = uStart[k];
      const hi = uStart[k + 1];
      for (let t = lo; t < hi; t++) {
        s -= uVal[t] * u[uIdx[t]];
      }
      u[k] = s / this.diag[k];
    }
    // L^T w = u (back substitution; column k of L, rows below the diagonal).
    for (let k = n - 1; k >= 0; k--) {
      let s = u[k];
      const lo = lStart[k];
      const hi = lStart[k + 1];
      for (let t = lo; t < hi; t++) {
        s -= lVal[t] * u[pinv[lIdx[t]]];
      }
      u[k] = s;
    }
    // y = P^T w.
    for (let k = 0; k < n; k++) {
      c[perm[k]!] = u[k];
    }
  }
}

// Small binary min-heap over plain numbers (used by the sparse factorization).
function heapPush(h: number[], v: number): void {
  h.push(v);
  let i = h.length - 1;
  while (i > 0) {
    const p = (i - 1) >> 1;
    if (h[i] < h[p]) {
      const t = h[i];
      h[i] = h[p];
      h[p] = t;
      i = p;
    } else {
      break;
    }
  }
}
function heapPop(h: number[]): number {
  const top = h[0];
  const last = h.pop() as number;
  if (h.length > 0) {
    h[0] = last;
    let i = 0;
    const n = h.length;
    for (;;) {
      const l = 2 * i + 1;
      const r = l + 1;
      let m = i;
      if (l < n && h[l] < h[m]) m = l;
      if (r < n && h[r] < h[m]) m = r;
      if (m === i) break;
      const t = h[i];
      h[i] = h[m];
      h[m] = t;
      i = m;
    }
  }
  return top;
}

// ---------------------------------------------------------------------------
// Simplex
// ---------------------------------------------------------------------------

const REFACTOR_INTERVAL = 128;

interface ConsiderState {
  v: number;
  dir: number;
  mag: number;
  has: boolean;
}

/** A reusable LP relaxation of a `Model` (structure is fixed; bounds change). */
export class LpRelaxation {
  n: number;
  m: number;
  /** Column costs (structural, column-scaled, solver copy; may be perturbed). */
  cost: number[];
  /** Unperturbed column costs, for reporting the true objective. */
  costTrue: number[];
  colLo: number[];
  colHi: number[];
  rowLo: number[];
  rowHi: number[];
  /** Sparse matrix, CSC by column. */
  cscStart: Int32Array;
  cscIndex: Int32Array;
  cscValue: Float64Array;
  /** Sparse matrix, CSR by row (same scaled values as the CSC). */
  csrStart: Int32Array;
  csrIndex: Int32Array;
  csrValue: Float64Array;
  /** Column equilibration factors: solver variable `x'_j = x_j / colScale[j]`. */
  colScale: Float64Array;

  private constructor(
    n: number,
    m: number,
    cost: number[],
    costTrue: number[],
    colLo: number[],
    colHi: number[],
    rowLo: number[],
    rowHi: number[],
    cscStart: Int32Array,
    cscIndex: Int32Array,
    cscValue: Float64Array,
    csrStart: Int32Array,
    csrIndex: Int32Array,
    csrValue: Float64Array,
    colScale: Float64Array,
  ) {
    this.n = n;
    this.m = m;
    this.cost = cost;
    this.costTrue = costTrue;
    this.colLo = colLo;
    this.colHi = colHi;
    this.rowLo = rowLo;
    this.rowHi = rowHi;
    this.cscStart = cscStart;
    this.cscIndex = cscIndex;
    this.cscValue = cscValue;
    this.csrStart = csrStart;
    this.csrIndex = csrIndex;
    this.csrValue = csrValue;
    this.colScale = colScale;
  }

  /** Build the relaxation from a model (builds CSC once) and equilibrate it. */
  static build(model: Model): LpRelaxation {
    const n = model.numCol;
    const m0 = model.numRow;

    // Drop equality rows that are implied by other equality rows.
    const drop = redundantEqualities(model);
    const rowNew: Int32Array = new Int32Array(m0).fill(MAXUSIZE);
    {
      let i = 0;
      for (let r = 0; r < m0; r++) {
        if (!drop[r]) {
          rowNew[r] = i;
          i += 1;
        }
      }
    }
    let m = 0;
    for (let r = 0; r < m0; r++) {
      if (rowNew[r] !== MAXUSIZE) m += 1;
    }

    const start = new Int32Array(n + 1).fill(0);
    for (let r = 0; r < m0; r++) {
      if (drop[r]) continue;
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        start[model.aIndex[k] + 1] += 1;
      }
    }
    for (let i = 0; i < n; i++) {
      start[i + 1] += start[i];
    }
    const nnz = start[n];
    const index = new Int32Array(nnz);
    const value = new Float64Array(nnz);
    const cursor = Int32Array.from(start);
    for (let r = 0; r < m0; r++) {
      if (drop[r]) continue;
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const c = model.aIndex[k];
        const p = cursor[c];
        index[p!] = rowNew[r];
        value[p!] = model.aValue[k];
        cursor[c!] += 1;
      }
    }

    // --- Equilibration scaling (inf-norm, clamped). ---
    const colScale = new Float64Array(n).fill(1);
    for (let j = 0; j < n; j++) {
      let mx = 0;
      for (let k = start[j]; k < start[j + 1]; k++) {
        mx = Math.max(mx, Math.abs(value[k]));
      }
      if (mx > 1e-12) {
        colScale[j] = clampNum(1.0 / mx, 1e-3, 1e3);
      }
    }
    for (let j = 0; j < n; j++) {
      const s = colScale[j];
      for (let k = start[j]; k < start[j + 1]; k++) {
        value[k!] *= s;
      }
    }
    const rowMax = new Float64Array(m).fill(0);
    for (let k = 0; k < nnz; k++) {
      const r = index[k];
      rowMax[r!] = Math.max(rowMax[r], Math.abs(value[k]));
    }
    const rowScale = new Float64Array(m).fill(1);
    for (let r = 0; r < m; r++) {
      if (rowMax[r] > 1e-12) {
        rowScale[r] = clampNum(1.0 / rowMax[r], 1e-3, 1e3);
      }
    }
    for (let k = 0; k < nnz; k++) {
      value[k] *= rowScale[index[k]];
    }

    const colLo = new Array<number>(n);
    const colHi = new Array<number>(n);
    for (let j = 0; j < n; j++) {
      colLo[j] = toLo(model.colLower[j]);
      colHi[j] = toHi(model.colUpper[j]);
    }
    const rowLo = filled<number>(m, 0);
    const rowHi = filled<number>(m, 0);
    for (let r = 0; r < m0; r++) {
      const i = rowNew[r];
      if (i === MAXUSIZE) continue;
      rowLo[i] = toLo(model.rowLower[r]) * rowScale[i];
      rowHi[i] = toHi(model.rowUpper[r]) * rowScale[i];
    }
    // Row-major copy (CSR) of the same scaled CSC, for the crash.
    const csrStart = new Int32Array(m + 1).fill(0);
    for (let k = 0; k < nnz; k++) {
      csrStart[index[k] + 1] += 1;
    }
    for (let r = 0; r < m; r++) {
      csrStart[r + 1] += csrStart[r];
    }
    const csrIndex = new Int32Array(nnz);
    const csrValue = new Float64Array(nnz);
    const csrCursor = Int32Array.from(csrStart);
    for (let j = 0; j < n; j++) {
      for (let k = start[j]; k < start[j + 1]; k++) {
        const r = index[k];
        const p = csrCursor[r];
        csrIndex[p!] = j;
        csrValue[p!] = value[k];
        csrCursor[r!] += 1;
      }
    }

    const costTrue: number[] = new Array(n);
    const cost: number[] = new Array(n);
    for (let j = 0; j < n; j++) {
      cost[j] = model.colCost[j] * colScale[j];
      costTrue[j] = cost[j];
    }
    // A tiny deterministic objective perturbation breaks the dual degeneracy
    // that makes Dantzig pricing cycle on large degenerate LPs.
    for (let j = 0; j < n; j++) {
      const r = mixHash53(j) - 0.5;
      cost[j] += 1e-9 * (1.0 + Math.abs(cost[j])) * r;
    }

    return new LpRelaxation(
      n,
      m,
      cost,
      costTrue,
      colLo,
      colHi,
      rowLo,
      rowHi,
      start,
      index,
      value,
      csrStart,
      csrIndex,
      csrValue,
      colScale,
    );
  }

  numCol(): number {
    return this.n;
  }
  numRow(): number {
    return this.m;
  }

  /** Solve with the model's own bounds. */
  solve(opts: LpOptions, clock: Clock): LpSolution {
    return this.solveWithBounds(this.colLo, this.colHi, opts, clock);
  }

  /** Solve with overridden column bounds (used by branching). Bounds are in
   * the model's original units. */
  solveWithBounds(colLo: readonly number[], colHi: readonly number[], opts: LpOptions, clock: Clock): LpSolution {
    return this.solveWithBasis(colLo, colHi, opts, clock, null);
  }

  solveWithBasis(
    colLo: readonly number[],
    colHi: readonly number[],
    opts: LpOptions,
    clock: Clock,
    basis: LpBasis | null,
  ): LpSolution {
    const started = clock.nowSecs();
    if (colLo.length !== this.n || colHi.length !== this.n) {
      return new LpSolution();
    }
    for (let j = 0; j < this.n; j++) {
      if (colLo[j] !== colLo[j] || colHi[j] !== colHi[j]) {
        return new LpSolution();
      }
      if (colLo[j] > colHi[j]) {
        const r = new LpSolution();
        r.status = LpStatus.Infeasible;
        return r;
      }
    }
    const loS: number[] = new Array(this.n);
    const hiS: number[] = new Array(this.n);
    for (let j = 0; j < this.n; j++) {
      loS[j] = toLo(colLo[j]) / this.colScale[j];
      hiS[j] = toHi(colHi[j]) / this.colScale[j];
    }
    let solver = new Simplex(this, loS, hiS, opts);
    const attempted = new LpStats();
    if (basis !== null) {
      const warm = solver.reoptimize(basis, opts, clock);
      if (warm !== null) {
        if (warm.status !== LpStatus.Optimal || respectsBounds(warm.colValue, colLo, colHi)) {
          warm.stats.seconds = clock.nowSecs() - started;
          warm.stats.warmSolves = 1;
          return warm;
        }
      }
      attempted.phase2Iterations = solver.warmIterations;
      attempted.warmFallbacks = 1;
      solver = new Simplex(this, loS, hiS, opts);
    }
    const remaining: LpOptions = {
      primalTol: opts.primalTol,
      dualTol: opts.dualTol,
      maxIterations: opts.maxIterations,
      refactorInterval: opts.refactorInterval,
      timeLimitSecs: Math.max(opts.timeLimitSecs - (clock.nowSecs() - started), 0.0),
    };
    const result = solver.run(remaining, clock);
    result.iterations += attempted.phase2Iterations;
    result.stats.add(attempted);
    result.stats.seconds = clock.nowSecs() - started;
    if (result.status === LpStatus.Optimal && !respectsBounds(result.colValue, colLo, colHi)) {
      result.status = LpStatus.Numerical;
      result.dualBound = null;
      result.basis = null;
    }
    return result;
  }
}

/** Mixing for the deterministic objective perturbation; returns a value in [0,1). */
function mixHash53(j: number): number {
  const h1 = mixHash32(j) >>> 0;
  const h2 = mixHash32((h1 ^ 0x9e3779b9) >>> 0) >>> 0;
  const mant = (h1 >>> 6) * 67108864 + (h2 >>> 5); // 26 + 27 = 53 bits
  return mant / 9007199254740992;
}
function mixHash32(x: number): number {
  let v = (x + 0x9e3779b9) >>> 0;
  v = Math.imul(v ^ (v >>> 16), 0x45d9f3b) >>> 0;
  v = Math.imul(v ^ (v >>> 16), 0x45d9f3b) >>> 0;
  v = (v ^ (v >>> 16)) >>> 0;
  return v;
}

function clampNum(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

function clamp0(l: number, u: number): number {
  if (l > 0) return l;
  if (u < 0) return u;
  return 0;
}
function clampBound(v: number, l: number, u: number): number {
  if (isFinite(l!) && v < l) return l;
  if (isFinite(u!) && v > u) return u;
  return v;
}

class Simplex {
  lpr: LpRelaxation;
  n: number;
  m: number;
  v: number; // n + 2m

  lo: Float64Array;
  hi: Float64Array;
  cost: Float64Array;
  x: Float64Array;

  basis: number[];
  inBasis: Int32Array;

  /** Artificial sign per row (0.0 = unused). */
  artSign: number[];
  phase = 0;
  phase1Iterations = 0;
  warmIterations = 0;

  lu: SparseLu;
  /** Eta file, flattened to parallel arrays. */
  etaP: number[] = [];
  etaStart: number[] = [0];
  etaIdx: number[] = [];
  etaVal: number[] = [];
  pricePos = 0;
  factorNnz = 0;
  sparseFactor = true;
  baseCols: SparseCol[];
  private scratch = new SparseCol();

  constructor(lpr: LpRelaxation, colLo: readonly number[], colHi: readonly number[], _opts: LpOptions) {
    const n = lpr.n;
    const m = lpr.m;
    const v = n + 2 * m;
    this.lpr = lpr;
    this.n = n;
    this.m = m;
    this.v = v;
    this.lo = new Float64Array(v);
    this.hi = new Float64Array(v);
    for (let j = 0; j < n; j++) {
      this.lo[j] = colLo[j];
      this.hi[j] = colHi[j];
    }
    for (let r = 0; r < m; r++) {
      this.lo[n + r] = lpr.rowLo[r];
      this.hi[n + r] = lpr.rowHi[r];
      this.lo[n + m + r] = 0;
      this.hi[n + m + r] = Infinity;
    }
    this.cost = new Float64Array(v);
    this.x = new Float64Array(v);
    this.basis = filled<number>(m, 0);
    this.inBasis = new Int32Array(v).fill(-1);
    this.artSign = filled<number>(m, 0);
    this.lu = SparseLu.identity(m);
    this.baseCols = new Array(m);
    for (let k = 0; k < m; k++) this.baseCols[k] = new SparseCol();
  }

  /** Sparse column of an extended variable. */
  column(varIdx: number, out: SparseCol): void {
    out.clear();
    const { n, m, lpr } = this;
    if (varIdx < n) {
      for (let k = lpr.cscStart[varIdx]; k < lpr.cscStart[varIdx + 1]; k++) {
        out.push(lpr.cscIndex[k], lpr.cscValue[k]);
      }
    } else if (varIdx < n + m) {
      out.push(varIdx - n, -1.0);
    } else {
      const r = varIdx - n - m;
      out.push(r, this.artSign[r]);
    }
  }

  fillColumn(varIdx: number, d: Float64Array): void {
    d.fill(0);
    const { n, m, lpr } = this;
    if (varIdx < n) {
      for (let k = lpr.cscStart[varIdx]; k < lpr.cscStart[varIdx + 1]; k++) {
        d[lpr.cscIndex[k]!] += lpr.cscValue[k];
      }
    } else if (varIdx < n + m) {
      d[varIdx - n] = -1.0;
    } else {
      const r = varIdx - n - m;
      d[r] = this.artSign[r];
    }
  }

  denseColumn(varIdx: number): Float64Array {
    const d = new Float64Array(this.m);
    this.fillColumn(varIdx, d);
    return d;
  }

  applyEtasFtran(z: Float64Array): void {
    const { etaP, etaStart, etaIdx, etaVal } = this;
    for (let e = 0; e < etaP.length; e++) {
      const p = etaP[e];
      const zp = z[p];
      if (zp !== 0) {
        let newp = zp;
        const lo = etaStart[e];
        const hi = etaStart[e + 1];
        for (let t = lo; t < hi; t++) {
          const i = etaIdx[t];
          const wi = etaVal[t];
          if (i === p) {
            newp = wi * zp;
          } else {
            z[i!] += wi * zp;
          }
        }
        z[p!] = newp;
      }
    }
  }

  applyEtasBtran(z: Float64Array): void {
    const { etaP, etaStart, etaIdx, etaVal } = this;
    for (let e = etaP.length - 1; e >= 0; e--) {
      const p = etaP[e];
      let dot = 0;
      const lo = etaStart[e];
      const hi = etaStart[e + 1];
      for (let t = lo; t < hi; t++) {
        dot += etaVal[t] * z[etaIdx[t]];
      }
      z[p!] = dot;
    }
  }

  pushEta(p: number, d: Float64Array, dp: number): void {
    this.etaP.push(p);
    this.etaIdx.push(p);
    this.etaVal.push(1.0 / dp);
    const inv = -1.0 / dp;
    for (let i = 0; i < d.length; i++) {
      const di = d[i];
      if (i !== p && di !== 0) {
        this.etaIdx.push(i);
        this.etaVal.push(di * inv);
      }
    }
    this.etaStart.push(this.etaIdx.length);
  }

  clearEtas(): void {
    this.etaP.length = 0;
    this.etaStart.length = 0;
    this.etaStart.push(0);
    this.etaIdx.length = 0;
    this.etaVal.length = 0;
  }

  ftran(b: Float64Array): void {
    this.lu.ftran(b);
    this.applyEtasFtran(b);
  }
  btran(c: Float64Array): void {
    this.applyEtasBtran(c);
    this.lu.btran(c);
  }

  /** Max residual of the basis equations `A'x' - s + sigma*alpha = 0`. */
  equationResidual(): number {
    const { n, m } = this;
    const act = new Float64Array(m);
    for (let j = 0; j < n; j++) {
      const xj = this.x[j];
      if (xj !== 0) {
        for (let k = this.lpr.cscStart[j]; k < this.lpr.cscStart[j + 1]; k++) {
          act[this.lpr.cscIndex[k]!] += this.lpr.cscValue[k] * xj;
        }
      }
    }
    let mx = 0;
    for (let r = 0; r < m; r++) {
      const res = act[r] - this.x[n + r] + this.artSign[r] * this.x[n + m + r];
      const a = Math.abs(res);
      if (a > mx) mx = a;
    }
    return mx;
  }

  /** Rebuild the basis matrix and factor it freshly (drops all etas). */
  refactorize(): void {
    for (let k = 0; k < this.m; k++) {
      const varIdx = this.basis[k];
      this.column(varIdx, this.baseCols[k]);
    }
    this.lu = SparseLu.factor(this.baseCols, this.m, this.sparseFactor);
    this.factorNnz = this.lu.lIdx.length + this.lu.uIdx.length;
    this.sparseFactor = this.factorNnz <= 8 * this.m;
    this.clearEtas();
    this.recomputeBasic();
  }

  /** Recompute basic values from the nonbasic point: `B x_B = -(N x_N)`. */
  recomputeBasic(): void {
    const { n, m } = this;
    const rhs = new Float64Array(m);
    for (let varIdx = 0; varIdx < this.v; varIdx++) {
      if (this.inBasis[varIdx] >= 0) continue;
      const xv = this.x[varIdx];
      if (xv !== 0) {
        this.column(varIdx, this.scratch);
        const idx = this.scratch.idx;
        const val = this.scratch.val;
        for (let t = 0; t < idx.length; t++) {
          rhs[idx[t]!] += val[t] * xv;
        }
      }
    }
    for (let r = 0; r < m; r++) {
      rhs[r] = -rhs[r];
    }
    this.lu.ftran(rhs);
    this.applyEtasFtran(rhs);
    for (let k = 0; k < m; k++) {
      this.x[this.basis[k]!] = rhs[k];
    }
  }

  /** Reset `x` from scratch, choose the initial basis, recompute basics. */
  initialize(): void {
    const { n, m } = this;
    for (let varIdx = 0; varIdx < this.v; varIdx++) {
      this.inBasis[varIdx] = -1;
      this.x[varIdx] = 0;
    }
    for (let j = 0; j < n; j++) {
      this.x[j] = clamp0(this.lo[j], this.hi[j]);
    }
    const act = new Float64Array(m);
    for (let j = 0; j < n; j++) {
      const xj = this.x[j];
      if (xj !== 0) {
        for (let k = this.lpr.cscStart[j]; k < this.lpr.cscStart[j + 1]; k++) {
          act[this.lpr.cscIndex[k]!] += this.lpr.cscValue[k] * xj;
        }
      }
    }
    this.crash(act);
    for (let r = 0; r < m; r++) {
      const s = n + r;
      const aVar = n + m + r;
      const loS = this.lo[s];
      const hiS = this.hi[s];
      if (act[r] >= loS - 1e-9 && act[r] <= hiS + 1e-9) {
        this.artSign[r] = 0;
        this.basis[r] = s;
        this.inBasis[s] = r;
      } else {
        const b = clampBound(act[r], loS, hiS);
        this.x[s] = b;
        const diff = b - act[r];
        this.artSign[r] = diff >= 0 ? 1.0 : -1.0;
        this.basis[r] = aVar;
        this.inBasis[aVar] = r;
      }
    }
    this.refactorizeNoRecompute();
    this.clearEtas();
    this.recomputeBasic();
  }

  /** Best-effort primal crash on the nonbasic structural point. */
  crash(act: Float64Array): void {
    const SWEEPS = 8;
    const n = this.n;
    const lpr = this.lpr;
    for (let sweep = 0; sweep < SWEEPS; sweep++) {
      let changed = false;
      for (let r = 0; r < this.m; r++) {
        const lo = this.lo[n + r];
        const hi = this.hi[n + r];
        let guard = 0;
        for (;;) {
          const ar = act[r];
          let dir: number;
          let need: number;
          if (ar < lo - 1e-9) {
            dir = 1.0;
            need = lo - ar;
          } else if (ar > hi + 1e-9) {
            dir = -1.0;
            need = ar - hi;
          } else {
            break;
          }
          let bj = -1;
          let ba = 0;
          for (let k = lpr.csrStart[r]; k < lpr.csrStart[r + 1]; k++) {
            const j = lpr.csrIndex[k];
            if (j >= n) continue;
            const a = lpr.csrValue[k];
            if (a === 0) continue;
            const mdir = a * dir > 0 ? 1.0 : -1.0;
            const room = mdir > 0 ? this.hi[j] - this.x[j] : this.x[j] - this.lo[j];
            if (room <= 1e-12) continue;
            if (Math.abs(a) > ba) {
              ba = Math.abs(a);
              bj = j;
            }
          }
          if (bj < 0) break;
          let coef = 0;
          for (let k = lpr.csrStart[r]; k < lpr.csrStart[r + 1]; k++) {
            if (lpr.csrIndex[k] === bj) {
              coef = lpr.csrValue[k];
              break;
            }
          }
          const desired = (dir * need) / coef;
          const old = this.x[bj];
          const nv = Math.min(Math.max(old + desired, this.lo[bj]), this.hi[bj]);
          const dd = nv - old;
          if (Math.abs(dd) <= 1e-12) break;
          this.x[bj] = nv;
          for (let k = lpr.cscStart[bj]; k < lpr.cscStart[bj + 1]; k++) {
            act[lpr.cscIndex[k]!] += lpr.cscValue[k] * dd;
          }
          changed = true;
          guard += 1;
          if (guard > n) break;
        }
      }
      if (!changed) break;
    }
  }

  refactorizeNoRecompute(): void {
    for (let k = 0; k < this.m; k++) {
      this.column(this.basis[k], this.baseCols[k]);
    }
    this.lu = SparseLu.factor(this.baseCols, this.m, this.sparseFactor);
  }

  /** Set costs for phase 1 (minimise sum of artificials). */
  setPhase1(): void {
    this.phase = 1;
    this.cost.fill(0);
    for (let r = 0; r < this.m; r++) {
      if (this.artSign[r] !== 0) {
        this.cost[this.n + this.m + r] = 1.0;
      }
    }
  }

  /** Set costs for phase 2 (real objective; artificials fixed at 0). */
  setPhase2(): void {
    this.phase = 2;
    for (let j = 0; j < this.n; j++) {
      this.cost[j] = this.lpr.cost[j];
    }
    for (let r = 0; r < this.m; r++) {
      const s = this.n + r;
      this.cost[s] = 0;
      const a = this.n + this.m + r;
      this.cost[a] = 0;
      this.lo[a] = 0;
      this.hi[a] = 0;
    }
  }

  run(opts: LpOptions, clock: Clock): LpSolution {
    const start = clock.nowSecs();
    this.initialize();
    this.setPhase1();

    const iters = { value: 0 };
    const st1 = this.optimize(opts, clock, start, iters);
    this.phase1Iterations = iters.value;
    if (st1 !== LpStatus.Optimal) {
      return this.finish(st1, iters.value);
    }
    let infeas = 0;
    for (let r = 0; r < this.m; r++) {
      infeas += Math.max(this.x[this.n + this.m + r], 0);
    }
    if (infeas > 1e-5 * (1.0 + this.m)) {
      return this.finish(LpStatus.Infeasible, iters.value);
    }

    this.setPhase2();
    this.purgeArtificials();
    const st2 = this.optimize(opts, clock, start, iters);
    return this.finish(st2, iters.value);
  }

  /** Bounded dual simplex: preserve reduced-cost feasibility while repairing
   * basic variables made infeasible by tighter child bounds. */
  reoptimize(saved: LpBasis, opts: LpOptions, clock: Clock): LpSolution | null {
    const start = clock.nowSecs();
    const { n, m } = this;
    if (saved.basis.length !== m || saved.x.length !== n + m) {
      return null;
    }
    this.setPhase2();
    for (let k = 0; k < m; k++) {
      this.basis[k] = saved.basis[k];
    }
    this.inBasis.fill(-1);
    for (let p = 0; p < m; p++) {
      const j = this.basis[p];
      if (j >= n + m || this.inBasis[j] >= 0) {
        return null;
      }
      this.inBasis[j!] = p;
    }
    for (let j = 0; j < n + m; j++) {
      this.x[j] = saved.x[j];
    }
    for (let j = 0; j < n + m; j++) {
      if (this.inBasis[j] < 0) {
        this.x[j] = clampNum(this.x[j], this.lo[j], this.hi[j]);
      }
    }
    this.refactorize();
    if (this.lu.singular || this.equationResidual() > 1e-6) {
      return null;
    }
    const y = new Float64Array(m);
    const row = new Float64Array(m);
    const d = new Float64Array(m);
    const rc = new Float64Array(n + m);
    for (;;) {
      if (this.warmIterations % 32 === 0 && clock.nowSecs() - start >= opts.timeLimitSecs) {
        return this.finish(LpStatus.TimeLimit, this.warmIterations);
      }
      if (this.warmIterations >= opts.maxIterations) {
        return null;
      }
      for (let p = 0; p < m; p++) {
        y[p] = this.cost[this.basis[p]];
      }
      this.btran(y);
      for (let j = 0; j < n + m; j++) {
        rc[j] = this.cost[j];
        if (j < n) {
          for (let k = this.lpr.cscStart[j]; k < this.lpr.cscStart[j + 1]; k++) {
            rc[j] -= this.lpr.cscValue[k] * y[this.lpr.cscIndex[k]];
          }
        } else {
          rc[j] += y[j - n];
        }
        if (this.inBasis[j] >= 0 || this.lo[j] === this.hi[j]) {
          continue;
        }
        const atLo = this.x[j] <= this.lo[j] + opts.primalTol;
        const atHi = this.x[j] >= this.hi[j] - opts.primalTol;
        if ((!atHi && rc[j] < -opts.dualTol * 10.0) || (!atLo && rc[j] > opts.dualTol * 10.0)) {
          return null;
        }
      }
      let leaveP = -1;
      let sign = 0;
      let violation = opts.primalTol;
      for (let p = 0; p < m; p++) {
        const j = this.basis[p];
        const low = this.lo[j] - this.x[j];
        const high = this.x[j] - this.hi[j];
        const v = Math.max(low, high);
        if (v > violation) {
          violation = v;
          leaveP = p;
          sign = low > high ? 1.0 : -1.0;
        }
      }
      if (leaveP < 0) {
        if (this.equationResidual() > 1e-6) {
          return null;
        }
        return this.finish(LpStatus.Optimal, this.warmIterations);
      }
      const p = leaveP;
      row.fill(0);
      row[p] = 1;
      this.btran(row);
      let bestJ = -1;
      let bestAlpha = 0;
      let ratio = Infinity;
      let bestPivotAlpha = 0;
      for (let j = 0; j < n + m; j++) {
        if (this.inBasis[j] >= 0 || this.lo[j] === this.hi[j]) {
          continue;
        }
        let alpha = 0;
        if (j < n) {
          let s = 0;
          for (let k = this.lpr.cscStart[j]; k < this.lpr.cscStart[j + 1]; k++) {
            s += row[this.lpr.cscIndex[k]] * this.lpr.cscValue[k];
          }
          alpha = s;
        } else {
          alpha = -row[j - n];
        }
        for (let di = 0; di < 2; di++) {
          const dir = di === 0 ? -1.0 : 1.0;
          if ((dir > 0 && this.x[j] >= this.hi[j] - opts.primalTol) || (dir < 0 && this.x[j] <= this.lo[j] + opts.primalTol)) {
            continue;
          }
          const q = -sign * alpha * dir;
          if (q < 1e-9) {
            continue;
          }
          const candidate = Math.max(rc[j] * dir, 0) / q;
          if (candidate < ratio - 1e-10 || (Math.abs(candidate - ratio) <= 1e-10 && Math.abs(alpha) > bestPivotAlpha)) {
            ratio = candidate;
            bestJ = j;
            bestAlpha = alpha;
            bestPivotAlpha = Math.abs(alpha);
          }
        }
      }
      if (bestJ < 0) {
        if (this.infeasibilityCertificate(row)) {
          return this.finish(LpStatus.Infeasible, this.warmIterations);
        }
        return null;
      }
      const enter = bestJ;
      const alpha = bestAlpha;
      d.fill(0);
      this.fillColumn(enter, d);
      this.ftran(d);
      if (Math.abs(d[p]) < 1e-9 || Math.abs(d[p] - alpha) > 1e-6 * (1.0 + Math.abs(alpha))) {
        return null;
      }
      const leaving = this.basis[p];
      const target = sign > 0 ? this.lo[leaving] : this.hi[leaving];
      const step = (this.x[leaving] - target) / d[p];
      if (!isFinite(step)) {
        return null;
      }
      this.x[enter] += step;
      for (let k = 0; k < m; k++) {
        this.x[this.basis[k]!] -= d[k] * step;
      }
      this.x[leaving!] = target;
      this.basis[p] = enter;
      this.inBasis[leaving!] = -1;
      this.inBasis[enter] = p;
      this.pushEta(p, d, d[p]);
      this.warmIterations += 1;
      if (this.etaP.length >= opts.refactorInterval) {
        this.refactorize();
        if (this.lu.singular) {
          return null;
        }
      }
    }
  }

  /** Check a numerical Farkas witness independently of the basis. */
  infeasibilityCertificate(y: Float64Array): boolean {
    const { n, m } = this;
    let lower = 0;
    let upper = 0;
    let magnitude = 1.0;
    for (let j = 0; j < n + m; j++) {
      let a = 0;
      let dot = 0;
      let ops = 0;
      if (j < n) {
        for (let k = this.lpr.cscStart[j]; k < this.lpr.cscStart[j + 1]; k++) {
          const v = this.lpr.cscValue[k] * y[this.lpr.cscIndex[k]];
          a += v;
          dot += Math.abs(v);
          ops += 2;
        }
      } else {
        a = -y[j - n];
      }
      const error = 8.0 * Number.EPSILON * (ops + 1) * dot;
      let lo = Infinity;
      let hi = -Infinity;
      for (let ci = 0; ci < 2; ci++) {
        const c = ci === 0 ? a - error : a + error;
        if (c === 0) {
          lo = Math.min(lo, 0);
          hi = Math.max(hi, 0);
          continue;
        }
        const l = c * (c > 0 ? this.lo[j] : this.hi[j]);
        const u = c * (c > 0 ? this.hi[j] : this.lo[j]);
        lo = Math.min(lo, l);
        hi = Math.max(hi, u);
      }
      lower += lo;
      upper += hi;
      if (isFinite(lo)) magnitude += Math.abs(lo);
      if (isFinite(hi)) magnitude += Math.abs(hi);
    }
    const guard = 64.0 * Number.EPSILON * (n + m + 1) * magnitude;
    return (isFinite(lower) && lower > guard) || (isFinite(upper) && upper < -guard);
  }

  /** Drive basic artificials (at 0) out of the basis where a pivot exists. */
  purgeArtificials(): void {
    const { n, m } = this;
    for (let p = 0; p < m; p++) {
      const b = this.basis[p];
      if (b < n + m) {
        continue;
      }
      const y = new Float64Array(m);
      y[p] = 1;
      this.btran(y); // y = B^{-T} e_p
      let chosenJ = -1;
      let chosenProd = 0;
      for (let j = 0; j < n + m; j++) {
        if (this.inBasis[j] >= 0) continue;
        this.column(j, this.scratch);
        const idx = this.scratch.idx;
        const val = this.scratch.val;
        let prod = 0;
        for (let t = 0; t < idx.length; t++) {
          prod += val[t] * y[idx[t]];
        }
        if (Math.abs(prod) > 1e-8) {
          chosenJ = j;
          chosenProd = prod;
          break;
        }
      }
      if (chosenJ >= 0) {
        const d = this.denseColumn(chosenJ);
        this.ftran(d);
        const dp = Math.abs(d[p]) > 1e-12 ? d[p] : chosenProd;
        if (this.etaP.length >= REFACTOR_INTERVAL) {
          this.refactorize();
        }
        const old = this.basis[p];
        this.basis[p] = chosenJ;
        this.inBasis[old!] = -1;
        this.inBasis[chosenJ] = p;
        this.pushEta(p, d, dp);
      }
    }
    this.recomputeBasic();
  }

  finish(status: LpStatus, iterations: number): LpSolution {
    const { n, m } = this;
    const colValue: number[] = new Array(n);
    for (let j = 0; j < n; j++) {
      const v = this.x[j] * this.lpr.colScale[j];
      colValue[j] = clampNum(
        v,
        isFinite(this.lpr.colLo[j]!) ? this.lpr.colLo[j] : -INF,
        isFinite(this.lpr.colHi[j]!) ? this.lpr.colHi[j] : INF,
      );
    }
    let objective = 0;
    for (let j = 0; j < n; j++) {
      objective += this.lpr.costTrue[j] * this.x[j];
    }
    const y = new Float64Array(m);
    for (let k = 0; k < m; k++) {
      const j = this.basis[k];
      y[k] = j < n ? this.lpr.costTrue[j] : 0;
    }
    this.btran(y);
    for (let r = 0; r < m; r++) {
      const j = n + r;
      if (!isFinite(this.lo[j]!)) y[r] = Math.min(y[r], 0);
      if (!isFinite(this.hi[j]!)) y[r] = Math.max(y[r], 0);
    }
    let dual = 0;
    let magnitude = 0;
    for (let j = 0; j < n + m; j++) {
      const cost = j < n ? this.lpr.costTrue[j] : 0;
      let rc = cost;
      let dotMagnitude = Math.abs(cost);
      let operations = 1;
      if (j < n) {
        for (let k = this.lpr.cscStart[j]; k < this.lpr.cscStart[j + 1]; k++) {
          const product = this.lpr.cscValue[k] * y[this.lpr.cscIndex[k]];
          rc -= product;
          dotMagnitude += Math.abs(product);
          operations += 2;
        }
      } else {
        rc += y[j - n];
        dotMagnitude += Math.abs(y[j - n]);
      }
      const error = 8.0 * Number.EPSILON * operations * dotMagnitude;
      let term = Infinity;
      for (let ci = 0; ci < 2; ci++) {
        const coefficient = ci === 0 ? rc - error : rc + error;
        const bound = coefficient >= 0 ? this.lo[j] : this.hi[j];
        const candidate = coefficient === 0 ? 0 : coefficient * bound;
        term = Math.min(term, candidate);
      }
      dual += term;
      magnitude += Math.abs(term);
    }
    const dualBound =
      status === LpStatus.Optimal && isFinite(dual)
        ? dual - 64.0 * Number.EPSILON * (1.0 + magnitude) * (n + m + 1)
        : null;
    const out = new LpSolution();
    out.status = status;
    out.objective = objective;
    out.dualBound = dualBound;
    out.colValue = colValue;
    out.iterations = iterations;
    out.stats = new LpStats();
    out.stats.solves = 1;
    out.stats.phase1Iterations = this.phase1Iterations;
    out.stats.phase2Iterations = Math.max(iterations - this.phase1Iterations, 0);
    let cleanBasis = status === LpStatus.Optimal;
    if (cleanBasis) {
      for (let k = 0; k < m; k++) {
        if (this.basis[k] >= n + m) {
          cleanBasis = false;
          break;
        }
      }
    }
    if (cleanBasis) {
      const bx: number[] = new Array(n + m);
      for (let k = 0; k < n + m; k++) bx[k] = this.x[k];
      out.basis = new LpBasis(this.basis.slice(), bx);
    }
    return out;
  }

  optimize(opts: LpOptions, clock: Clock, start: number, iters: { value: number }): LpStatus {
    const ptol = opts.primalTol;
    const dtol = opts.dualTol;
    let degenerate = 0;
    const banned: number[] = [];
    const y = new Float64Array(this.m);
    const d = new Float64Array(this.m);

    for (;;) {
      if (iters.value >= opts.maxIterations) {
        return LpStatus.IterationLimit;
      }
      if (iters.value % 256 === 0 && clock.nowSecs() - start > opts.timeLimitSecs) {
        return LpStatus.TimeLimit;
      }

      // Duals: B^T y = c_B.
      y.fill(0);
      for (let k = 0; k < this.m; k++) {
        y[k] = this.cost[this.basis[k]];
      }
      this.btran(y);

      // Pricing (Dantzig; Bland on prolonged degeneracy).
      const bland = degenerate > 100;
      const priced = this.price(y, ptol, dtol, bland, banned);
      if (priced === null) {
        return banned.length === 0 ? LpStatus.Optimal : LpStatus.Numerical;
      }
      const enter = priced.v;
      const dir = priced.dir;

      // Direction of movement.
      this.fillColumn(enter, d);
      this.ftran(d);

      // Harris two-pass ratio test.
      let tBound = Infinity;
      if (dir > 0) {
        if (isFinite(this.hi[enter]!)) {
          tBound = Math.max(this.hi[enter] - this.x[enter], 0);
        }
      } else if (isFinite(this.lo[enter]!)) {
        tBound = Math.max(this.x[enter] - this.lo[enter], 0);
      }
      const dthr = 1e-10;
      let tRelaxed = tBound;
      for (let i = 0; i < this.m; i++) {
        const coef = -d[i] * dir;
        const bi = this.basis[i];
        if (coef > dthr) {
          if (isFinite(this.hi[bi]!)) {
            const r = (this.hi[bi] + ptol - this.x[bi]) / coef;
            if (r < tRelaxed) {
              tRelaxed = Math.max(r, 0);
            }
          }
        } else if (coef < -dthr && isFinite(this.lo[bi]!)) {
          const r = (this.x[bi] + ptol - this.lo[bi]) / -coef;
          if (r < tRelaxed) {
            tRelaxed = Math.max(r, 0);
          }
        }
      }
      const t = Math.min(tRelaxed, tBound);
      let leave = -1;
      let bestPivot = 0;
      for (let i = 0; i < this.m; i++) {
        const coef = -d[i] * dir;
        const bi = this.basis[i];
        let room: number;
        let mag: number;
        if (coef > 0) {
          if (!isFinite(this.hi[bi]!)) continue;
          room = this.hi[bi] - this.x[bi];
          mag = coef;
        } else if (coef < 0) {
          if (!isFinite(this.lo[bi]!)) continue;
          room = this.x[bi] - this.lo[bi];
          mag = -coef;
        } else {
          continue;
        }
        if (mag < dthr) continue;
        const ratio = Math.max(room / mag, 0);
        if (ratio <= t + 1e-9 && mag > bestPivot) {
          bestPivot = mag;
          leave = i;
        }
      }
      // Reject a numerically unusable pivot.
      let dmax = 0;
      for (let i = 0; i < this.m; i++) {
        const a = Math.abs(d[i]);
        if (a > dmax) dmax = a;
      }
      if (leave >= 0 && Math.abs(d[leave]) < 1e-8 * Math.max(dmax, 1e-12)) {
        banned.push(enter);
        continue;
      }

      if (!isFinite(t)) {
        return LpStatus.Unbounded;
      }

      // Apply the step (exact: do NOT clamp basics).
      this.x[enter] += dir * t;
      for (let i = 0; i < this.m; i++) {
        const bi = this.basis[i];
        this.x[bi!] -= d[i] * dir * t;
      }

      if (leave < 0) {
        this.x[enter] = clampBound(this.x[enter], this.lo[enter], this.hi[enter]);
      } else {
        const p = leave;
        const leaving = this.basis[p];
        const dp = d[p];
        this.basis[p] = enter;
        this.inBasis[leaving!] = -1;
        this.inBasis[enter] = p;
        this.x[leaving!] = clampBound(this.x[leaving], this.lo[leaving], this.hi[leaving]);
        this.pushEta(p, d, dp);
      }
      banned.length = 0;

      if (this.etaP.length >= opts.refactorInterval) {
        this.refactorize();
      }

      if (t <= ptol) {
        degenerate += 1;
      } else {
        degenerate = 0;
      }
      iters.value += 1;
    }
  }

  private considerVar(varIdx: number, rc: number, dtol: number, st: ConsiderState, bland: boolean, banned: number[]): void {
    if (this.hi[varIdx] - this.lo[varIdx] <= 1e-12) {
      return;
    }
    if (banned.indexOf(varIdx) >= 0) {
      return;
    }
    const canUp = this.hi[varIdx] - this.x[varIdx] > 1e-12;
    const canDown = this.x[varIdx] - this.lo[varIdx] > 1e-12;
    if (rc < -dtol && canUp && (bland || -rc > st.mag)) {
      st.mag = -rc;
      st.v = varIdx;
      st.dir = 1.0;
      st.has = true;
    } else if (rc > dtol && canDown && (bland || rc > st.mag)) {
      st.mag = rc;
      st.v = varIdx;
      st.dir = -1.0;
      st.has = true;
    }
  }

  /** Choose an entering variable. Returns `(var, +1|-1)`, or null. */
  price(y: Float64Array, _ptol: number, dtol: number, bland: boolean, banned: number[]): ConsiderState | null {
    const st: ConsiderState = { v: -1, dir: 0, mag: 0, has: false };

    const n = this.n;
    let newPos = this.pricePos;
    const window = bland || n < 2000 ? Math.max(n, 1) : n < 6000 ? Math.min(Math.max((n / 2) | 0, 256), Math.max(n, 1)) : Math.min(Math.max((n / 4) | 0, 256), Math.max(n, 1));
    let start = bland ? 0 : this.pricePos % Math.max(n, 1);
    let covered = 0;
    while (covered < n && !(bland && st.has)) {
      const hi = Math.min(start + window, n);
      for (let j = start; j < hi; j++) {
        if (this.inBasis[j] >= 0) {
          continue;
        }
        let rc = this.cost[j];
        for (let k = this.lpr.cscStart[j]; k < this.lpr.cscStart[j + 1]; k++) {
          rc -= y[this.lpr.cscIndex[k]] * this.lpr.cscValue[k];
        }
        this.considerVar(j, rc, dtol, st, bland, banned);
        if (bland && st.has) {
          break;
        }
      }
      covered += hi - start;
      if (st.has) {
        newPos = hi % Math.max(n, 1);
        break;
      }
      if (hi >= n) {
        start = 0;
      } else {
        start = hi;
      }
    }
    // Logical columns (cost 0, column -e_r -> rc = y[r]).
    for (let r = 0; r < this.m; r++) {
      const varIdx = this.n + r;
      if (this.inBasis[varIdx] >= 0) continue;
      this.considerVar(varIdx, y[r], dtol, st, bland, banned);
      if (bland && st.has) {
        this.pricePos = newPos % Math.max(n, 1);
        return st;
      }
    }
    // Artificial columns (phase 1 only, cost 1, column artSign * e_r).
    for (let r = 0; r < this.m; r++) {
      if (this.artSign[r] === 0) continue;
      const varIdx = this.n + this.m + r;
      if (this.inBasis[varIdx] >= 0) continue;
      const rc = this.cost[varIdx] - y[r] * this.artSign[r];
      this.considerVar(varIdx, rc, dtol, st, bland, banned);
      if (bland && st.has) {
        this.pricePos = newPos % Math.max(n, 1);
        return st;
      }
    }
    this.pricePos = newPos % Math.max(n, 1);
    return st.has ? st : null;
  }
}
