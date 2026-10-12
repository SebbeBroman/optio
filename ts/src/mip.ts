// Branch & bound on top of the LP relaxation (`lp.ts`).
// (Port of `crates/core/src/mip.rs`.)
//
// Primal-first: the caller passes the best incumbent found by the feasibility
// heuristics, and B&B only improves or proves it. Node LPs are solved with the
// revised simplex; a node is pruned when its LP bound cannot beat the
// incumbent. Incumbents and progress are reported through `Callbacks`.
//
// The native parallel path (`branch_and_bound_mt`) is intentionally not ported:
// this is a single-threaded port, and the wasm build never takes it.

import { Model, INF, checkSolution, isIntegral } from "./model.ts";
import type { Options } from "./options.ts";
import type { Clock } from "./clock.ts";
import { LpRelaxation, LpStatus, LpStats, defaultLpOptions, type LpSolution, type LpBasis } from "./lp.ts";
import { polish } from "./heuristic.ts";
import { ModelStatus } from "./status.ts";
import { Solution, type Callbacks } from "./types.ts";

const INT_TOL = 1e-6;
const CHECK_TOL = 1e-6;
const DEFAULT_MAX_NODES = 100000;
const MASK64 = (1n << 64n) - 1n;

/** `(objective, rounded point)` from rounding an LP solution. */
type Inc = [number, number[]];

/**
 * Round the integer columns of `x` and return `(objective, rounded)` if the
 * rounded point is feasible at a tight tolerance. This is a cheap primal
 * heuristic and the *validation* used before accepting an incumbent.
 */
function roundIntegerPoint(model: Model, x: readonly number[], intCols: readonly number[]): Inc | null {
  const xr = x.slice();
  for (const j of intCols) {
    xr[j] = Math.round(xr[j]);
  }
  if (!polish(model, xr)) {
    return null;
  }
  const chk = checkSolution(model, xr, CHECK_TOL);
  if (chk.feasible) {
    return [chk.objective, xr];
  }
  return null;
}

interface Node {
  lo: number[];
  hi: number[];
  bound: number;
  isRoot: boolean;
  basis: LpBasis | null;
  branch: [number, boolean, number, number] | null;
}

/** Min-heap over nodes by `bound` (best-bound search). */
class NodeHeap {
  private a: Node[] = [];
  static boundLess(a: Node, b: Node): boolean {
    return a.bound < b.bound;
  }
  push(n: Node): void {
    const a = this.a;
    a.push(n);
    let i = a.length - 1;
    while (i > 0) {
      const p = (i - 1) >> 1;
      if (a[i].bound < a[p].bound) {
        const t = a[i];
        a[i] = a[p];
        a[p] = t;
        i = p;
      } else {
        break;
      }
    }
  }
  pop(): Node | null {
    const a = this.a;
    if (a.length === 0) return null;
    const top = a[0];
    const last = a.pop() as Node;
    if (a.length > 0) {
      a[0] = last;
      let i = 0;
      const n = a.length;
      for (;;) {
        const l = 2 * i + 1;
        const r = l + 1;
        let m = i;
        if (l < n && a[l].bound < a[m].bound) m = l;
        if (r < n && a[r].bound < a[m].bound) m = r;
        if (m === i) break;
        const t = a[i];
        a[i] = a[m];
        a[m] = t;
        i = m;
      }
    }
    return top;
  }
  peek(): Node | null {
    return this.a.length > 0 ? this.a[0] : null;
  }
}

/** Per-worker learned objective degradation per unit of branching distance. */
class Pseudocosts {
  sums: number[][];
  counts: number[][];
  constructor(n: number) {
    this.sums = new Array(n);
    this.counts = new Array(n);
    for (let j = 0; j < n; j++) {
      this.sums[j] = [0, 0];
      this.counts[j] = [0, 0];
    }
  }
  update(branch: [number, boolean, number, number] | null, bound: number): void {
    if (branch !== null) {
      const j = branch[0];
      const up = branch[1];
      const distance = branch[2];
      const parent = branch[3];
      if (isFinite(parent) && isFinite(bound) && distance > 1e-6) {
        const side = up ? 1 : 0;
        this.sums[j][side] += Math.max(bound - parent, 0) / distance;
        this.counts[j][side] += 1;
      }
    }
  }
  choose(cols: readonly number[], x: readonly number[], learn: boolean): number | null {
    let best: number | null = null;
    let score = 0;
    for (const j of cols) {
      const f = x[j] - Math.floor(x[j]);
      if (Math.min(f, 1 - f) <= INT_TOL) {
        continue;
      }
      let value: number;
      if (learn && (this.counts[j][0] > 0 || this.counts[j][1] > 0)) {
        const avg = (side: number): number => {
          return this.counts[j][side] > 0 ? this.sums[j][side] / this.counts[j][side] : 1.0;
        };
        const a = f * avg(0);
        const b = (1 - f) * avg(1);
        value = Math.min(a, b) + 0.1 * Math.max(a, b);
      } else {
        value = Math.min(f, 1 - f);
      }
      if (value > score || best === null) {
        score = value;
        best = j;
      }
    }
    return best;
  }
}

class Xorshift {
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

/**
 * Feasibility pump: alternate rounding the LP point and re-solving an LP that
 * minimises the L1 distance to that rounded point, until the rounding is
 * feasible. General and structure-free.
 */
export function feasibilityPump(model: Model, options: Options, clock: Clock): Inc | null {
  const intCols: number[] = [];
  for (let j = 0; j < model.numCol; j++) {
    if (isIntegral(model.colType[j])) intCols.push(j);
  }
  if (intCols.length === 0) return null;
  const start = clock.nowSecs();
  const deadline = clampNum(options.timeLimitSecs * 0.5, 0.2, 8.0);
  const lpOpts = defaultLpOptions();
  lpOpts.maxIterations = 20000;
  lpOpts.timeLimitSecs = deadline;

  const lpr = LpRelaxation.build(model);
  const lp = lpr.solve(lpOpts, clock);
  if (lp.status !== LpStatus.Optimal) return null;
  const r0 = roundIntegerPoint(model, lp.colValue, intCols);
  if (r0 !== null) return r0;
  let xtilde: number[] = lp.colValue.slice();
  for (const j of intCols) xtilde[j] = Math.round(xtilde[j]);

  const next = new Xorshift(0x9e3779b97f4a7c15n);

  for (let it = 0; it < 200; it++) {
    if (clock.nowSecs() - start > deadline) break;
    const active = intCols.slice();
    if (active.length === 0) break;
    const aux = model.clone();
    for (let j = 0; j < model.numCol; j++) aux.setCost(j, 0);
    for (const j of active) {
      const p = aux.addCol(0, INF);
      const mv = aux.addCol(0, INF);
      aux.setCost(p, 1);
      aux.setCost(mv, 1);
      aux.addRow(xtilde[j], xtilde[j], [
        [j, 1],
        [p, 1],
        [mv, -1],
      ]);
    }
    const alpr = LpRelaxation.build(aux);
    const res = alpr.solve(lpOpts, clock);
    if (res.status !== LpStatus.Optimal || res.colValue.length < model.numCol) break;
    const x = res.colValue.slice(0, model.numCol);
    const rr = roundIntegerPoint(model, x, intCols);
    if (rr !== null) return rr;
    const newtilde = x.slice();
    for (const j of intCols) newtilde[j] = Math.round(newtilde[j]);
    let changed = false;
    for (const j of intCols) {
      if (Math.abs(newtilde[j] - xtilde[j]) > 0.5) {
        changed = true;
        break;
      }
    }
    if (!changed || it % 4 === 3) {
      const k = 1 + Math.floor(next.nextF64() * 4);
      for (let t = 0; t < k; t++) {
        const j = intCols[Math.floor(next.nextF64() * intCols.length) % intCols.length];
        newtilde[j] = Math.max(0, Math.floor(x[j]) + Math.floor(next.nextF64() * 2));
      }
    }
    xtilde = newtilde;
  }
  return null;
}

/**
 * Depth-first LP diving to find a feasible integer point. Returns a validated
 * `(objective, x)` incumbent, or `null`.
 */
export function diveIncumbent(model: Model, options: Options, clock: Clock): Inc | null {
  const intCols: number[] = [];
  for (let j = 0; j < model.numCol; j++) {
    if (isIntegral(model.colType[j])) intCols.push(j);
  }
  if (intCols.length === 0) return null;
  const lpr = LpRelaxation.build(model);
  const start = clock.nowSecs();
  const deadline = clampNum(options.timeLimitSecs * 0.5, 0.2, 8.0);
  const lpOpts = defaultLpOptions();
  lpOpts.maxIterations = 20000;
  lpOpts.timeLimitSecs = deadline;
  const maxNodes = 3000;
  let nodes = 0;
  const stack: [number[], number[]][] = [[model.colLower.slice(), model.colUpper.slice()]];

  while (stack.length > 0) {
    const top = stack.pop() as [number[], number[]];
    const lo = top[0];
    const hi = top[1];
    if (nodes >= maxNodes || clock.nowSecs() - start > deadline) break;
    nodes += 1;
    lpOpts.timeLimitSecs = Math.max(deadline - (clock.nowSecs() - start), 0.02);
    const lp = lpr.solveWithBounds(lo, hi, lpOpts, clock);
    if (lp.status !== LpStatus.Optimal) continue;
    const rr = roundIntegerPoint(model, lp.colValue, intCols);
    if (rr !== null) return rr;
    let bj = -1;
    let bf = INT_TOL;
    for (const j of intCols) {
      const v = lp.colValue[j];
      const f = Math.abs(v - Math.round(v));
      if (f > bf) {
        bf = f;
        bj = j;
      }
    }
    if (bj < 0) continue;
    const v = lp.colValue[bj];
    const down = Math.floor(v);
    const up = Math.ceil(v);
    const loD = lo.slice();
    const hiD = hi.slice();
    hiD[bj] = Math.min(hiD[bj], down);
    const loU = lo;
    const hiU = hi;
    loU[bj] = Math.max(loU[bj], up);
    stack.push([loU, hiU]);
    stack.push([loD, hiD]);
  }
  return null;
}

function clampNum(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

/** Report a new incumbent if it improves on the current one. */
function report(inc: Inc | null, x: number[], obj: number, callbacks: Callbacks): Inc {
  const better = inc === null || obj < inc[0] - 1e-9;
  if (better) {
    const onInc = callbacks.onIncumbent;
    if (onInc !== undefined) {
      onInc({ objective: obj, colValue: x.slice() });
    }
    return [obj, x.slice()];
  }
  return inc as Inc;
}

/** Branch & bound for the MILP (single-threaded). */
export function branchAndBound(
  model: Model,
  options: Options,
  clock: Clock,
  callbacks: Callbacks,
  warmStart: readonly number[] | null,
  rootLpIn: LpSolution | null,
): Solution {
  const s = new Solution();
  if (model.numCol === 0) {
    s.status = ModelStatus.Empty;
    return s;
  }

  const intCols: number[] = [];
  for (let j = 0; j < model.numCol; j++) {
    if (isIntegral(model.colType[j])) intCols.push(j);
  }

  const lpr = LpRelaxation.build(model);
  const lpOpts = defaultLpOptions();
  lpOpts.maxIterations = 20000;
  lpOpts.timeLimitSecs = options.timeLimitSecs;
  const start = clock.nowSecs();
  const deadline = options.timeLimitSecs;

  let incumbent: Inc | null = null;
  if (warmStart !== null) {
    const chk = checkSolution(model, warmStart, 1e-6);
    if (chk.feasible) {
      incumbent = report(incumbent, warmStart.slice(), chk.objective, callbacks);
    }
  }

  // No integer variables: the LP relaxation is the answer.
  if (intCols.length === 0) {
    const lp = lpr.solve(lpOpts, clock);
    s.lpIterations = lp.iterations;
    s.lpStats = lp.stats;
    s.nodes = 1;
    switch (lp.status) {
      case LpStatus.Optimal:
        s.status = ModelStatus.Optimal;
        s.objective = lp.objective;
        s.dualBound = lp.dualBound;
        s.colValue = lp.colValue;
        break;
      case LpStatus.Infeasible:
        s.status = ModelStatus.Infeasible;
        break;
      case LpStatus.Unbounded:
        s.status = ModelStatus.Unbounded;
        break;
      case LpStatus.TimeLimit:
        s.status = ModelStatus.TimeLimit;
        break;
      default:
        s.status = ModelStatus.Unknown;
        break;
    }
    return s;
  }

  let nodes = 0;
  let lpIterations = 0;
  const lpStats = new LpStats();
  const pseudocosts = new Pseudocosts(model.numCol);
  let proved = true;
  let sawUnbounded = false;
  let globalBound: number | null = null;
  let remainingBound = Infinity;
  let rootLp = rootLpIn;
  const stack = new NodeHeap();
  stack.push({
    lo: model.colLower.slice(),
    hi: model.colUpper.slice(),
    bound: -Infinity,
    isRoot: true,
    basis: null,
    branch: null,
  });

  for (;;) {
    const node = stack.pop();
    if (node === null) break;
    if (clock.nowSecs() - start > deadline) {
      remainingBound = Math.min(remainingBound, node.bound);
      proved = false;
      break;
    }
    const interrupt = callbacks.interrupt;
    if (interrupt !== undefined && interrupt()) {
      remainingBound = Math.min(remainingBound, node.bound);
      proved = false;
      break;
    }
    if (nodes >= DEFAULT_MAX_NODES) {
      remainingBound = Math.min(remainingBound, node.bound);
      proved = false;
      break;
    }
    if (incumbent !== null) {
      const tol = Math.max(options.mipAbsGap, options.mipRelGap * Math.abs(incumbent[0]));
      if (node.bound >= incumbent[0] - tol - 1e-9) {
        remainingBound = Math.min(remainingBound, node.bound);
        continue;
      }
    }
    const elapsed = clock.nowSecs() - start;
    lpOpts.timeLimitSecs = Math.max(deadline - elapsed, 0.05);

    let lp: LpSolution;
    if (node.isRoot && rootLp !== null) {
      lp = rootLp;
      rootLp = null;
    } else {
      lp = lpr.solveWithBasis(node.lo, node.hi, lpOpts, clock, options.lpWarmStart ? node.basis : null);
    }
    nodes += 1;
    lpIterations += lp.iterations;
    lpStats.add(lp.stats);

    const childBasis = lp.basis;
    let obj: number;
    let x: number[];
    if (lp.status === LpStatus.Optimal) {
      obj = lp.dualBound !== null ? lp.dualBound : -Infinity;
      x = lp.colValue;
    } else if (lp.status === LpStatus.Infeasible) {
      continue;
    } else if (lp.status === LpStatus.Unbounded) {
      remainingBound = Math.min(remainingBound, node.bound);
      sawUnbounded = true;
      proved = false;
      continue;
    } else {
      remainingBound = Math.min(remainingBound, node.bound);
      proved = false;
      continue;
    }

    if (node.isRoot) {
      globalBound = obj;
    }

    if (incumbent !== null) {
      const tol = Math.max(options.mipAbsGap, options.mipRelGap * Math.abs(incumbent[0]));
      if (obj >= incumbent[0] - tol - 1e-9) {
        remainingBound = Math.min(remainingBound, obj);
        continue;
      }
    }

    const rounded = roundIntegerPoint(model, x, intCols);
    if (rounded !== null) {
      incumbent = report(incumbent, rounded[1], rounded[0], callbacks);
    }

    pseudocosts.update(node.branch, obj);
    const branchVar = pseudocosts.choose(intCols, x, options.mipPseudocost);

    if (branchVar === null) {
      remainingBound = Math.min(remainingBound, obj);
      if (roundIntegerPoint(model, x, intCols) === null) {
        proved = false;
      }
    } else {
      const j = branchVar;
      const v = x[j];
      const down = Math.floor(v);
      const up = Math.ceil(v);
      const loD = node.lo;
      const hiD = node.hi.slice();
      hiD[j] = Math.min(hiD[j], down);
      const loU = node.lo.slice();
      const hiU = node.hi;
      loU[j] = Math.max(loU[j], up);
      stack.push({
        lo: loD,
        hi: hiD,
        bound: obj,
        isRoot: false,
        basis: childBasis,
        branch: [j, false, v - down, obj],
      });
      stack.push({
        lo: loU,
        hi: hiU,
        bound: obj,
        isRoot: false,
        basis: childBasis,
        branch: [j, true, up - v, obj],
      });
    }

    if (nodes % 32 === 0) {
      const onProg = callbacks.onProgress;
      if (onProg !== undefined) {
        const peek = stack.peek();
        const frontier = Math.min(peek === null ? Infinity : peek.bound, remainingBound);
        let primal = NaN;
        let dual = NaN;
        let has = false;
        let gap = Infinity;
        if (incumbent !== null) {
          primal = incumbent[0];
          dual = Math.min(frontier, incumbent[0]);
          has = true;
          gap = Math.abs(dual) > 1e-12 ? Math.max((primal - dual) / Math.abs(dual), 0) : Infinity;
        } else {
          dual = frontier;
        }
        onProg({
          runningTime: clock.nowSecs() - start,
          nodes,
          lpIterations,
          gap,
          primalBound: primal,
          dualBound: dual,
          hasIncumbent: has,
        });
      }
    }
  }

  let status: ModelStatus;
  if (sawUnbounded && incumbent === null) {
    status = ModelStatus.UnboundedOrInfeasible;
  } else if (incumbent !== null) {
    status = proved && clock.nowSecs() - start <= deadline ? ModelStatus.Optimal : ModelStatus.TimeLimit;
  } else if (proved) {
    status = ModelStatus.Infeasible;
  } else {
    status = ModelStatus.Unknown;
  }

  const peek = stack.peek();
  let frontier = Math.min(peek === null ? Infinity : peek.bound, remainingBound);
  const cap = incumbent !== null ? incumbent[0] : Infinity;
  const finalBound = Math.min(Math.max(frontier, globalBound !== null ? globalBound : -Infinity), cap);
  const objective = incumbent !== null ? incumbent[0] : 0;
  const colValue = incumbent !== null ? incumbent[1] : [];
  if (
    status === ModelStatus.Optimal &&
    objective - finalBound >
      Math.max(options.mipAbsGap, options.mipRelGap * Math.abs(objective), 1e-7 * (1 + Math.abs(objective)))
  ) {
    status = ModelStatus.SolutionLimit;
  }
  s.status = status;
  s.objective = objective;
  s.dualBound = finalBound;
  s.colValue = colValue;
  s.nodes = nodes;
  s.lpIterations = lpIterations;
  s.lpStats = lpStats;
  return s;
}
