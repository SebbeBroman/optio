// Phase-2 primal heuristic: find a feasible (not necessarily optimal) point.
// (Port of `crates/core/src/heuristic.rs`.)
//
// Generic min-conflicts with cost-aware move selection, preceded by a cheap
// LP-rounding fast path. It is the first priority for the planning use-case,
// where a fully-placed plan matters far more than optimality. The LP relaxation
// + branch & bound use this point as a warm start.

import { Model, VAR_CONTINUOUS, isIntegral, checkSolution, fmax } from "./model.ts";
import type { Options } from "./options.ts";
import type { Clock } from "./clock.ts";
import { LpRelaxation, LpStatus, defaultLpOptions, type LpSolution } from "./lp.ts";

const MASK64 = (1n << 64n) - 1n;

/** SplitMix-ish xorshift over a 64-bit state (matches the Rust `Rng`). */
class Rng {
  state: bigint;
  constructor(seed: bigint) {
    this.state = BigInt.asUintN(64, seed);
  }
  nextF64(): number {
    let s = this.state;
    s = (s ^ ((s << 13n) & MASK64)) & MASK64;
    s = (s ^ (s >> 7n)) & MASK64;
    s = (s ^ ((s << 17n) & MASK64)) & MASK64;
    this.state = s;
    return Number(s >> 11n) / 9007199254740992;
  }
}

/** Sparse matrix in CSC (column-major) form, with reusable accessors. */
class Csc {
  start: Int32Array;
  index: Int32Array;
  value: Float64Array;
  constructor(start: Int32Array, index: Int32Array, value: Float64Array) {
    this.start = start;
    this.index = index;
    this.value = value;
  }
  static build(model: Model): Csc {
    const n = model.numCol;
    const start = new Int32Array(n + 1).fill(0);
    for (let k = 0; k < model.aIndex.length; k++) {
      start[model.aIndex[k] + 1] += 1;
    }
    for (let i = 0; i < n; i++) {
      start[i + 1] += start[i];
    }
    const nnz = model.aValue.length;
    const index = new Int32Array(nnz);
    const value = new Float64Array(nnz);
    const cursor = Int32Array.from(start);
    for (let r = 0; r < model.numRow; r++) {
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const c = model.aIndex[k];
        const p = cursor[c];
        index[p!] = r;
        value[p!] = model.aValue[k];
        cursor[c!] += 1;
      }
    }
    return new Csc(start, index, value);
  }
}

function violation(lo: number, hi: number, lhs: number): number {
  return fmax(fmax(lo - lhs, lhs - hi), 0.0);
}

function initPoint(model: Model, _rng: Rng): number[] {
  const x: number[] = new Array(model.numCol);
  for (let c = 0; c < model.numCol; c++) {
    let v = clampNum(0.0, model.colLower[c], model.colUpper[c]);
    if (isIntegral(model.colType[c])) {
      v = Math.round(v);
    }
    x[c] = v;
  }
  return x;
}

function clampNum(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

function compute(model: Model, x: readonly number[]): [Float64Array, Float64Array, number] {
  const m = model.numRow;
  const lhs = new Float64Array(m);
  for (let r = 0; r < m; r++) {
    let s = 0;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      s += model.aValue[k] * x[model.aIndex[k]];
    }
    lhs[r] = s;
  }
  const viol = new Float64Array(m);
  let total = 0;
  for (let r = 0; r < m; r++) {
    const v = violation(model.rowLower[r], model.rowUpper[r], lhs[r]);
    viol[r] = v;
    total += v;
  }
  return [lhs, viol, total];
}

function deltaViolation(model: Model, csc: Csc, lhs: Float64Array, viol: Float64Array, c: number, d: number): number {
  let dv = 0;
  for (let k = csc.start[c]; k < csc.start[c + 1]; k++) {
    const r = csc.index[k];
    dv += violation(model.rowLower[r], model.rowUpper[r], lhs[r] + csc.value[k] * d) - viol[r];
  }
  return dv;
}

function applyMove(
  model: Model,
  csc: Csc,
  x: number[],
  lhs: Float64Array,
  viol: Float64Array,
  c: number,
  delta: number,
): void {
  x[c!] += delta;
  for (let k = csc.start[c]; k < csc.start[c + 1]; k++) {
    const r = csc.index[k];
    const a = csc.value[k];
    viol[r!] -= violation(model.rowLower[r], model.rowUpper[r], lhs[r]);
    const newLhs = lhs[r] + a * delta;
    lhs[r!] = newLhs;
    viol[r!] += violation(model.rowLower[r], model.rowUpper[r], newLhs);
  }
}

/** Re-project the continuous variables onto the rows (a few passes). */
export function polish(model: Model, x: number[]): boolean {
  if (checkSolution(model, x, 1e-6).feasible) {
    return true;
  }
  const csc = Csc.build(model);
  let [lhs, viol] = compute(model, x);
  for (let iter = 0; iter < 4; iter++) {
    projectAll(model, csc, x, lhs, viol);
    if (checkSolution(model, x, 1e-6).feasible) {
      return true;
    }
  }
  return false;
}

/** Adjust a continuous variable in row `r` to satisfy it, then propagate. */
function projectRow(
  model: Model,
  csc: Csc,
  x: number[],
  lhs: Float64Array,
  viol: Float64Array,
  r: number,
  depth: number,
): void {
  if (depth > 8 || viol[r] <= 1e-12) {
    return;
  }
  const lo = model.rowLower[r];
  const hi = model.rowUpper[r];
  let target: number;
  if (lhs[r] < lo) {
    target = lo;
  } else if (lhs[r] > hi) {
    target = hi;
  } else {
    return;
  }
  for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
    const c = model.aIndex[k];
    if (model.colType[c] !== VAR_CONTINUOUS) {
      continue;
    }
    const a = model.aValue[k];
    if (a === 0) continue;
    const old = x[c];
    const nv = clampNum(old + (target - lhs[r]) / a, model.colLower[c], model.colUpper[c]);
    const d = nv - old;
    if (Math.abs(d) > 1e-12) {
      applyMove(model, csc, x, lhs, viol, c, d);
      const affected: number[] = [];
      for (let t = csc.start[c]; t < csc.start[c + 1]; t++) affected.push(csc.index[t]);
      for (const rr of affected) {
        if (rr !== r) {
          projectRow(model, csc, x, lhs, viol, rr, depth + 1);
        }
      }
    }
    return;
  }
}

function projectAll(model: Model, csc: Csc, x: number[], lhs: Float64Array, viol: Float64Array): void {
  for (let r = 0; r < model.numRow; r++) {
    projectRow(model, csc, x, lhs, viol, r, 0);
  }
}

/**
 * Cheap LP-based feasibility for assignment / exact-cover models. Returns the
 * validated rounded point (if any) and the solved root relaxation (if optimal).
 */
function lpRoundFeasible(model: Model, clock: Clock, budgetSecs: number): [number[] | null, LpSolution | null] {
  const intCols: number[] = [];
  for (let j = 0; j < model.numCol; j++) {
    if (isIntegral(model.colType[j])) intCols.push(j);
  }
  if (intCols.length === 0) {
    return [null, null];
  }
  const opts = defaultLpOptions();
  opts.maxIterations = 50000;
  opts.timeLimitSecs = Math.max(budgetSecs, 0.02);
  const lpr = LpRelaxation.build(model);
  const lp = lpr.solve(opts, clock);
  if (lp.status !== LpStatus.Optimal) {
    return [null, null];
  }
  const xr = lp.colValue.slice();
  for (const j of intCols) {
    xr[j] = Math.round(xr[j]);
  }
  const rounded = polish(model, xr) ? xr : null;
  return [rounded, lp];
}

export interface Feasible {
  x: number[];
  iterations: number;
  /** The root LP relaxation, when the LP-rounding fast path solved it. */
  rootLp: LpSolution | null;
}

export function findFeasible(model: Model, options: Options, clock: Clock, maxIters: number): Feasible | null {
  const tol = 1e-6;
  const lpBudget = clampNum(options.timeLimitSecs * 0.25, 0.02, 0.5);
  const [rounded, rootLp] = lpRoundFeasible(model, clock, lpBudget);
  if (rounded !== null) {
    return { x: rounded, iterations: 0, rootLp };
  }
  const csc = Csc.build(model);
  const seed = BigInt.asUintN(64, BigInt(options.seed) * 0x9e3779b97f4a7c15n + 1n);
  const rng = new Rng(seed);

  let x = initPoint(model, rng);
  let [lhs, viol, _t] = compute(model, x);
  projectAll(model, csc, x, lhs, viol);
  let total = 0;
  for (let r = 0; r < model.numRow; r++) total += viol[r];
  let bestTotal = total;
  let sinceImprove = 0;
  const start = clock.nowSecs();

  for (let iter = 0; iter < maxIters; iter++) {
    if (total <= tol) {
      return { x, iterations: iter, rootLp };
    }
    if ((iter & 0x3f) === 0 && clock.nowSecs() - start > options.timeLimitSecs) {
      break;
    }

    // Most-violated row (random tie-break).
    let r = 0;
    let bestV = -1.0;
    for (let rr = 0; rr < model.numRow; rr++) {
      if (viol[rr] > bestV + 1e-12) {
        bestV = viol[rr];
        r = rr;
      } else if (viol[rr] > bestV - 1e-12 && rng.nextF64() < 0.25) {
        r = rr;
      }
    }
    if (bestV <= 0.0) {
      return { x, iterations: iter, rootLp };
    }

    const lo = model.rowLower[r];
    const hi = model.rowUpper[r];
    const dir = lhs[r] < lo ? 1.0 : -1.0;
    const need = lhs[r] < lo ? lo - lhs[r] : lhs[r] - hi;

    // Best candidate in this row by true effect on total violation.
    let candC = -1;
    let candD = 0;
    let candDv = 0;
    let haveCand = false;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      const a = model.aValue[k];
      if (a === 0) continue;
      const desired = (dir * need) / a;
      const old = x[c];
      const clo = model.colLower[c];
      const chi = model.colUpper[c];
      let newv = clampNum(old + desired, clo, chi);
      if (isIntegral(model.colType[c])) {
        newv = Math.round(newv);
        if (Math.abs(newv - old) < 1e-9) {
          newv = Math.round(clampNum(old + dir * Math.sign(a), clo, chi));
        }
      }
      const d = newv - old;
      if (Math.abs(d) < 1e-9) continue;
      const dv = deltaViolation(model, csc, lhs, viol, c, d);
      if (!haveCand || dv < candDv) {
        candC = c;
        candD = d;
        candDv = dv;
        haveCand = true;
      }
    }

    if (haveCand && candDv < -1e-12) {
      applyMove(model, csc, x, lhs, viol, candC, candD);
      const affected: number[] = [];
      for (let t = csc.start[candC]; t < csc.start[candC + 1]; t++) affected.push(csc.index[t]);
      for (const rr of affected) {
        projectRow(model, csc, x, lhs, viol, rr, 0);
      }
    } else {
      // No improving move in this (or any) row: perturb to escape.
      const cc = Math.floor(rng.nextF64() * model.numCol) % Math.max(model.numCol, 1);
      if (isIntegral(model.colType[cc])) {
        const old = x[cc];
        const step = rng.nextF64() < 0.5 ? 1.0 : -1.0;
        const nv = Math.round(clampNum(old + step, model.colLower[cc], model.colUpper[cc]));
        const dd = nv - old;
        if (Math.abs(dd) > 1e-9) {
          applyMove(model, csc, x, lhs, viol, cc, dd);
        }
      }
    }

    total = 0;
    for (let rr = 0; rr < model.numRow; rr++) total += viol[rr];
    if (total < bestTotal - 1e-9) {
      bestTotal = total;
      sinceImprove = 0;
    } else {
      sinceImprove += 1;
      if (sinceImprove > 4000) {
        x = initPoint(model, rng);
        const [l, v, _tt] = compute(model, x);
        lhs = l;
        viol = v;
        projectAll(model, csc, x, lhs, viol);
        total = 0;
        for (let rr = 0; rr < model.numRow; rr++) total += viol[rr];
        bestTotal = total;
        sinceImprove = 0;
      }
    }
  }

  if (total <= tol) {
    return { x, iterations: maxIters, rootLp };
  }
  return null;
}
