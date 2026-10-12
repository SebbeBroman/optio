// Shared solve result and callback types (port of the type half of
// `crates/core/src/solve.rs`). Kept separate from the orchestration so that
// `mip.ts` can depend on the result types without importing the pipeline.

import { ModelStatus, statusHasFeasibleSolution } from "./status.ts";
import type { LpStats } from "./lp.ts";

/** A candidate solution found during search. */
export interface Incumbent {
  objective: number;
  colValue: number[];
}

/** Progress snapshot pushed to the callback (mirrors `MipProgress`). */
export interface Progress {
  runningTime: number;
  nodes: number;
  lpIterations: number;
  gap: number;
  primalBound: number;
  dualBound: number;
  hasIncumbent: boolean;
}

export function defaultProgress(): Progress {
  return {
    runningTime: 0,
    nodes: 0,
    lpIterations: 0,
    gap: 0,
    primalBound: 0,
    dualBound: 0,
    hasIncumbent: false,
  };
}

/** Anytime hooks. Both are optional and called from the solve loop. */
export interface Callbacks {
  onProgress?: (p: Progress) => void;
  onIncumbent?: (i: Incumbent) => void;
  /** Return `true` to request an early stop (time/interrupt). */
  interrupt?: () => boolean;
}

export function defaultCallbacks(): Callbacks {
  return {};
}

/** Final solve result. */
export class Solution {
  status: ModelStatus = ModelStatus.NotSet;
  objective = 0;
  /** Conservative global lower bound; `null` when no bound was established. */
  dualBound: number | null = null;
  colValue: number[] = [];
  nodes = 0;
  lpIterations = 0;
  /** Wall-clock phase timings (seconds). */
  heuristicSecs = 0;
  localSearchSecs = 0;
  bbSecs = 0;
  presolveSecs = 0;
  lpStats?: LpStats;
  cutSecs = 0;
  boundSecs = 0;
  rootCuts = 0;
  inferredIntegerCols = 0;

  hasSolution(): boolean {
    return statusHasFeasibleSolution(this.status) && this.colValue.length > 0;
  }
}
