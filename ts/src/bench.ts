// Native/bench entry logic: read a detached `highs-js` model
// (`getModel('csr')` shape), solve it, and report one JSON object. This mirrors
// `crates/bench/src/main.rs` so a differential harness can drive both engines
// with the same input/contract.

import { Model, INF, VAR_CONTINUOUS, VAR_INTEGER, VAR_BINARY, checkSolution, type VarType } from "./model.ts";
import { StdClock } from "./clock.ts";
import { defaultOptions, type Options } from "./options.ts";
import { ModelStatus, statusHasFeasibleSolution } from "./status.ts";
import { LpRelaxation, LpStatus, defaultLpOptions } from "./lp.ts";
import { solveWith } from "./solve.ts";

export interface ModelJson {
  numCols: number;
  numRows: number;
  sense: number;
  offset: number;
  colCost: number[];
  colLower: number[];
  colUpper: number[];
  rowLower: number[];
  rowUpper: number[];
  integrality: number[];
  matrix: {
    starts: number[];
    indices: number[];
    values: number[];
  };
}

export interface BenchResult {
  statusCode: number;
  objective: number | null;
  dualBound: number | null;
  colValue: number[];
  nodes: number;
  iterations: number;
  ms: number;
  maxRowViolation: number | null;
}

export interface BenchOptions {
  time: number;
  gap: number;
  lp: boolean;
  threads: number;
  presolve: boolean;
}

export function benchOptions(): BenchOptions {
  return { time: 10, gap: 1e-4, lp: false, threads: 1, presolve: true };
}

function numAt(a: readonly number[], i: number, dflt: number): number {
  return i < a.length ? a[i] : dflt;
}

export function modelFromJson(v: ModelJson, costSign: number): Model {
  const m = new Model();
  const numCol = v.numCols;
  for (let c = 0; c < numCol; c++) {
    m.addCol(numAt(v.colLower, c, 0), numAt(v.colUpper, c, INF));
    m.setCost(c, costSign * numAt(v.colCost, c, 0));
    const raw = numAt(v.integrality, c, 0);
    const ty: VarType = raw === 0 ? VAR_CONTINUOUS : raw === 2 ? VAR_BINARY : VAR_INTEGER;
    m.setIntegrality(c, ty);
  }
  const starts = v.matrix.starts;
  const indices = v.matrix.indices;
  const values = v.matrix.values;
  for (let r = 0; r < v.numRows; r++) {
    const lo = r < starts.length ? starts[r] : 0;
    const hi = r + 1 < starts.length ? starts[r + 1] : lo;
    const entries: [number, number][] = [];
    for (let k = lo; k < hi; k++) {
      entries.push([numAt(indices, k, 0), numAt(values, k, 0)]);
    }
    m.addRow(numAt(v.rowLower, r, -INF), numAt(v.rowUpper, r, INF), entries);
  }
  return m;
}

function lpStatusCode(s: LpStatus): number {
  switch (s) {
    case LpStatus.Optimal:
      return 7;
    case LpStatus.Infeasible:
      return 8;
    case LpStatus.Unbounded:
      return 10;
    case LpStatus.TimeLimit:
      return 13;
    case LpStatus.IterationLimit:
      return 14;
    default:
      return 15;
  }
}

export function runModelJson(v: ModelJson, opts: BenchOptions): BenchResult {
  const sign = v.sense < 0 ? -1 : 1;
  const offset = v.offset;
  const model = modelFromJson(v, sign);
  const clock = new StdClock();

  if (opts.lp) {
    const lopts = defaultLpOptions();
    lopts.maxIterations = 500000;
    lopts.timeLimitSecs = opts.time;
    const t0 = clock.nowSecs();
    const sol = LpRelaxation.build(model).solve(lopts, clock);
    const ms = (clock.nowSecs() - t0) * 1000;
    const feasible = sol.status === LpStatus.Optimal;
    return {
      statusCode: lpStatusCode(sol.status),
      objective: feasible ? sign * sol.objective + offset : null,
      dualBound: sol.dualBound !== null ? sign * sol.dualBound + offset : null,
      colValue: sol.colValue,
      nodes: 0,
      iterations: sol.iterations,
      ms,
      maxRowViolation: null,
    };
  }

  const options: Options = defaultOptions();
  options.timeLimitSecs = opts.time;
  options.mipRelGap = opts.gap;
  options.outputFlag = false;
  options.threads = opts.threads;
  options.presolve = opts.presolve;
  const t0 = clock.nowSecs();
  const sol = solveWith(model, options, clock, {});
  const ms = (clock.nowSecs() - t0) * 1000;
  const has = sol.hasSolution();
  const check = has ? checkSolution(model, sol.colValue, 1e-6) : null;
  return {
    statusCode: sol.status,
    objective: has ? sign * sol.objective + offset : null,
    dualBound: sol.dualBound !== null ? sign * sol.dualBound + offset : null,
    colValue: sol.colValue,
    nodes: sol.nodes,
    iterations: sol.lpIterations,
    ms,
    maxRowViolation: check !== null ? check.maxRowViolation : null,
  };
}

/** Kept so callers can mirror Rust's `has_feasible_solution` checks. */
export function resultHasSolution(r: BenchResult): boolean {
  return statusHasFeasibleSolution(r.statusCode as ModelStatus) && r.colValue.length > 0 && r.objective !== null;
}

export { ModelStatus };
