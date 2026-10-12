// Solve entry points and the platform clock abstraction.
// (Port of `crates/core/src/solve.rs`.)
//
// Pipeline: a structure-aware feasibility heuristic finds an incumbent fast
// (the planning use-case cares far more about feasibility than optimality),
// then branch & bound with the LP relaxation proves/improves it. `checkSolution`
// is the source of truth for correctness, never the reported status.

import { Model, checkSolution } from "./model.ts";
import type { Options } from "./options.ts";
import type { Clock } from "./clock.ts";
import { ModelStatus, statusIsInfeasible } from "./status.ts";
import { propagate, strengthen } from "./presolve.ts";
import { findFeasible, polish } from "./heuristic.ts";
import { feasibilityPump, diveIncumbent, branchAndBound } from "./mip.ts";
import { improve as blockImprove, rowBound as blockRowBound, schedulingBound } from "./block_planner.ts";
import { improvePlanningAdaptive, constructInlinedPlanning, repair } from "./local_search.ts";
import { Solution, type Callbacks, type Progress } from "./types.ts";
import type { LpSolution } from "./lp.ts";

export { StdClock } from "./clock.ts";
export type { Clock } from "./clock.ts";

function arraysEqual(a: readonly number[], b: readonly number[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

/** Bounds-clamped zero point; the starting guess for primal heuristics. */
export function initialPoint(model: Model): number[] {
  const x: number[] = new Array(model.numCol);
  for (let c = 0; c < model.numCol; c++) {
    let v = 0;
    if (v < model.colLower[c]) v = model.colLower[c];
    if (v > model.colUpper[c]) v = model.colUpper[c];
    x[c] = v;
  }
  return x;
}

/**
 * Run the planning objective local search. Block-planner first (structure
 * aware), then the adaptive local search. Single-threaded, so no multi-start
 * portfolio is used.
 */
function planObjectiveSearch(
  model: Model,
  x0: number[],
  clock: Clock,
  budget: number,
  seed: number,
  _threads: number,
  adaptive: boolean,
): [number, number[]] | null {
  const bx = blockImprove(model, x0, clock, budget);
  if (bx !== null) {
    return [model.objective(bx), bx];
  }
  return improvePlanningAdaptive(model, x0, clock, budget, seed, adaptive);
}

/** Solve `model` without callbacks. See [`solveWith`]. */
export function solve(model: Model, options: Options, clock: Clock): Solution {
  return solveWith(model, options, clock, {});
}

/** Solve `model` with callbacks but no warm start. */
export function solveWith(model: Model, options: Options, clock: Clock, callbacks: Callbacks): Solution {
  return solveWithStart(model, options, clock, callbacks, null);
}

/**
 * Solve `model` with callbacks and an optional warm-start primal point.
 */
export function solveWithStart(
  model: Model,
  options: Options,
  clock: Clock,
  callbacks: Callbacks,
  warmStart: readonly number[] | null,
): Solution {
  if (model.numCol === 0) {
    const s = new Solution();
    s.status = ModelStatus.Empty;
    return s;
  }

  const solveStart = clock.nowSecs();
  const original = model;
  let pmodel: Model = model;
  if (options.presolve) {
    const reduced = propagate(model, clock, solveStart + options.timeLimitSecs);
    if (reduced === null) {
      const s = new Solution();
      s.status = ModelStatus.Infeasible;
      s.presolveSecs = clock.nowSecs() - solveStart;
      return s;
    }
    pmodel = reduced;
  }
  const presolveSecs = clock.nowSecs() - solveStart;
  if (arraysEqual(pmodel.colLower, pmodel.colUpper)) {
    const chk = checkSolution(original, pmodel.colLower, 1e-6);
    if (chk.feasible) {
      const onInc = callbacks.onIncumbent;
      if (onInc !== undefined) {
        onInc({ objective: chk.objective, colValue: pmodel.colLower.slice() });
      }
      const s = new Solution();
      s.status = ModelStatus.Optimal;
      s.objective = chk.objective;
      s.dualBound = chk.objective;
      s.presolveSecs = presolveSecs;
      s.colValue = pmodel.colLower.slice();
      return s;
    }
    const s = new Solution();
    s.status = ModelStatus.Infeasible;
    return s;
  }

  // Primal-first: an early incumbent from the LP-free heuristic, but only for
  // models that actually have integer variables.
  let hasInt = false;
  for (let j = 0; j < pmodel.numCol; j++) {
    if (pmodel.colType[j] !== 0) {
      hasInt = true;
      break;
    }
  }
  const t0 = solveStart;

  // Validate the caller's incumbent before doing any search.
  let suppliedWarm: number[] | null = null;
  if (warmStart !== null && warmStart.length === pmodel.numCol) {
    const x = warmStart.slice();
    if (polish(pmodel, x)) {
      suppliedWarm = x;
    }
  }

  let rootLp: LpSolution | null = null;
  let heuristicWarm: number[] | null = null;
  if (hasInt && suppliedWarm === null) {
    const heurLimit = clampNum(options.timeLimitSecs * 0.3, 0.05, 2.0);
    const heurStarted = clock.nowSecs();
    const heurOpts: Options = { ...options, timeLimitSecs: heurLimit };
    const b = blockImprove(pmodel, null, clock, heurLimit);
    if (b !== null) {
      heuristicWarm = b;
    } else {
      const c = constructInlinedPlanning(pmodel);
      if (c !== null) {
        heuristicWarm = c;
      } else {
        heurOpts.timeLimitSecs = Math.max(heurLimit - (clock.nowSecs() - heurStarted), 0);
        const f = findFeasible(pmodel, heurOpts, clock, 5000000);
        if (f !== null) {
          rootLp = f.rootLp;
          heuristicWarm = f.x;
        } else {
          const r = repair(pmodel, initialPoint(pmodel), 200000, options.seed);
          heuristicWarm = r !== null ? r.x : null;
        }
      }
    }
  }
  // A rejected supplied start must not discard a valid heuristic incumbent.
  let warm: number[] | null = suppliedWarm !== null ? suppliedWarm : heuristicWarm;

  if (warm === null && hasInt) {
    const remaining = Math.max(options.timeLimitSecs - (clock.nowSecs() - t0), 0.2);
    const pumpOpts: Options = { ...options, timeLimitSecs: Math.min(remaining * 0.2, 2.0) };
    const p = feasibilityPump(pmodel, pumpOpts, clock);
    if (p !== null) {
      warm = p[1];
    } else {
      const d = diveIncumbent(pmodel, pumpOpts, clock);
      warm = d !== null ? d[1] : null;
    }
  }

  const elapsed = clock.nowSecs() - t0;

  // Report the starting incumbent before optimising.
  if (warm !== null) {
    const w = warm;
    const chk = checkSolution(pmodel, w, 1e-6);
    if (chk.feasible) {
      const onInc = callbacks.onIncumbent;
      if (onInc !== undefined) {
        onInc({ objective: chk.objective, colValue: w.slice() });
      }
      const onProg = callbacks.onProgress;
      if (onProg !== undefined) {
        const prog: Progress = {
          runningTime: clock.nowSecs() - t0,
          nodes: 0,
          lpIterations: 0,
          primalBound: chk.objective,
          dualBound: -Infinity,
          gap: Infinity,
          hasIncumbent: true,
        };
        onProg(prog);
      }
    }
  }

  const interrupt = callbacks.interrupt;
  if (warm !== null && interrupt !== undefined && interrupt()) {
    const s = new Solution();
    s.status = ModelStatus.Interrupt;
    s.objective = pmodel.objective(warm);
    s.colValue = warm.slice();
    s.heuristicSecs = elapsed;
    return s;
  }

  const boundStarted = clock.nowSecs();
  let rowBoundVal: number | null = null;
  if (hasInt) {
    const a = blockRowBound(original);
    const b = schedulingBound(original, clock, t0 + options.timeLimitSecs);
    rowBoundVal = a !== null && b !== null ? Math.max(a, b) : a !== null ? a : b;
  }
  const boundSecs = clock.nowSecs() - boundStarted;
  if (warm !== null && rowBoundVal !== null) {
    const check = checkSolution(original, warm, 1e-9);
    if (
      check.feasible &&
      rowBoundVal <= check.objective &&
      check.objective - rowBoundVal <=
        Math.max(options.mipAbsGap, options.mipRelGap * Math.abs(check.objective), 1e-7 * (1 + Math.abs(check.objective)))
    ) {
      const onProg = callbacks.onProgress;
      if (onProg !== undefined) {
        onProg({
          runningTime: clock.nowSecs() - t0,
          nodes: 0,
          lpIterations: 0,
          primalBound: check.objective,
          dualBound: rowBoundVal,
          gap: Math.max((check.objective - rowBoundVal) / Math.max(Math.abs(check.objective), 1), 0),
          hasIncumbent: true,
        });
      }
      const s = new Solution();
      s.status = ModelStatus.Optimal;
      s.objective = check.objective;
      s.dualBound = rowBoundVal;
      s.boundSecs = boundSecs;
      s.colValue = warm.slice();
      s.presolveSecs = presolveSecs;
      s.heuristicSecs = Math.max(elapsed - presolveSecs, 0);
      return s;
    }
  }

  // Objective local search for the planning shape.
  if (hasInt && warm !== null) {
    const cur = warm;
    const remaining = Math.max(options.timeLimitSecs - (clock.nowSecs() - t0), 0);
    const budget = clampNum(remaining * 0.5, 0, 30);
    if (budget > 0.01) {
      const res = planObjectiveSearch(pmodel, cur, clock, budget, options.seed, options.threads, options.adaptiveSearch);
      if (res !== null) {
        const curObj = checkSolution(pmodel, cur, 1e-6).objective;
        if (res[0] < curObj - 1e-9) {
          const onInc = callbacks.onIncumbent;
          if (onInc !== undefined) {
            onInc({ objective: res[0], colValue: res[1].slice() });
          }
          warm = res[1];
        }
      }
    }
  }

  const localSecs = Math.max(clock.nowSecs() - t0 - elapsed - boundSecs, 0);

  if (warm !== null && rowBoundVal !== null) {
    const check = checkSolution(original, warm, 1e-9);
    if (
      check.feasible &&
      rowBoundVal <= check.objective &&
      check.objective - rowBoundVal <=
        Math.max(options.mipAbsGap, options.mipRelGap * Math.abs(check.objective), 1e-7 * (1 + Math.abs(check.objective)))
    ) {
      const onProg = callbacks.onProgress;
      if (onProg !== undefined) {
        onProg({
          runningTime: clock.nowSecs() - t0,
          nodes: 0,
          lpIterations: 0,
          primalBound: check.objective,
          dualBound: rowBoundVal,
          gap: Math.max((check.objective - rowBoundVal) / Math.max(Math.abs(check.objective), 1), 0),
          hasIncumbent: true,
        });
      }
      const s = new Solution();
      s.status = ModelStatus.Optimal;
      s.objective = check.objective;
      s.dualBound = rowBoundVal;
      s.boundSecs = boundSecs;
      s.colValue = warm.slice();
      s.presolveSecs = presolveSecs;
      s.heuristicSecs = Math.max(elapsed - presolveSecs, 0);
      s.localSearchSecs = localSecs;
      return s;
    }
  }

  const cutStarted = clock.nowSecs();
  let rootCuts = 0;
  let bbModel: Model;
  if (hasInt && options.mipCuts) {
    // A tolerance-feasible point can have an objective slightly below the exact
    // optimum: do not derive hard objective-domain bounds from it.
    let cutoff: number | null = null;
    if (warm !== null) {
      const check = checkSolution(original, warm, 1e-9);
      if (check.feasible) {
        cutoff = pmodel.objective(warm);
      } else {
        const [typed] = strengthen(pmodel, original, null);
        const candidate = warm.slice();
        for (let j = 0; j < candidate.length; j++) {
          if (typed.colType[j] !== 0) {
            candidate[j] = Math.round(candidate[j]);
          }
        }
        const check2 = checkSolution(original, candidate, 1e-9);
        cutoff = check2.feasible ? pmodel.objective(candidate) : null;
      }
    }
    const [m, n] = strengthen(pmodel, original, cutoff);
    rootCuts = n;
    if (
      n > 0 ||
      !arraysEqual(m.rowLower, pmodel.rowLower) ||
      !arraysEqual(m.rowUpper, pmodel.rowUpper) ||
      !arraysEqual(m.colLower, pmodel.colLower) ||
      !arraysEqual(m.colUpper, pmodel.colUpper)
    ) {
      rootLp = null;
    }
    bbModel = m;
  } else {
    bbModel = pmodel;
  }
  const cutSecs = clock.nowSecs() - cutStarted;

  const bbOpts: Options = {
    ...options,
    timeLimitSecs: Math.max(options.timeLimitSecs - (clock.nowSecs() - t0), 0.2),
  };
  const sol = branchAndBound(
    bbModel,
    bbOpts,
    clock,
    callbacks,
    warm,
    rootLp,
  );
  if (rowBoundVal !== null) {
    sol.dualBound = sol.dualBound !== null ? Math.max(sol.dualBound, rowBoundVal) : rowBoundVal;
  }
  sol.boundSecs = boundSecs;
  sol.cutSecs = cutSecs;
  sol.rootCuts = rootCuts;
  let inferred = 0;
  for (let j = 0; j < bbModel.colType.length && j < pmodel.colType.length; j++) {
    if (bbModel.colType[j] !== pmodel.colType[j]) inferred += 1;
  }
  sol.inferredIntegerCols = inferred;
  sol.presolveSecs = presolveSecs;
  sol.heuristicSecs = Math.max(elapsed - presolveSecs, 0);
  sol.localSearchSecs = localSecs;
  sol.bbSecs = Math.max(clock.nowSecs() - t0 - elapsed - localSecs - cutSecs - boundSecs, 0);
  if (sol.hasSolution() || statusIsInfeasible(sol.status) || sol.status === ModelStatus.Optimal) {
    return sol;
  }

  // B&B inconclusive: prefer the raw incumbent if it is feasible.
  if (warm !== null) {
    const check = checkSolution(pmodel, warm, 1e-6);
    if (check.feasible) {
      const s = new Solution();
      s.status = ModelStatus.SolutionLimit;
      s.objective = check.objective;
      s.dualBound = sol.dualBound;
      s.colValue = warm;
      s.nodes = sol.nodes;
      s.lpIterations = sol.lpIterations;
      s.heuristicSecs = sol.heuristicSecs;
      s.localSearchSecs = sol.localSearchSecs;
      s.bbSecs = sol.bbSecs;
      s.presolveSecs = sol.presolveSecs;
      s.lpStats = sol.lpStats;
      s.cutSecs = sol.cutSecs;
      s.boundSecs = sol.boundSecs;
      s.rootCuts = sol.rootCuts;
      s.inferredIntegerCols = sol.inferredIntegerCols;
      return s;
    }
  }

  return sol;
}

function clampNum(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}
