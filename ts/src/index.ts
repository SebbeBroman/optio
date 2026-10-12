// Public surface of the optio TypeScript port (mirrors `optio-core`'s re-exports).

export { Model, checkSolution, INF, VAR_CONTINUOUS, VAR_INTEGER, VAR_BINARY, isIntegral, fmax, fmin } from "./model.ts";
export type { VarType, SolutionCheck } from "./model.ts";
export { ModelStatus, statusIsInfeasible, statusHasFeasibleSolution } from "./status.ts";
export { defaultOptions } from "./options.ts";
export type { Options } from "./options.ts";
export { StdClock } from "./clock.ts";
export type { Clock } from "./clock.ts";
export { Solution } from "./types.ts";
export type { Incumbent, Progress, Callbacks } from "./types.ts";
export { initialPoint, solve, solveWith, solveWithStart } from "./solve.ts";
export { LpStatus, LpOptions, LpStats, LpSolution, LpBasis, LpRelaxation, defaultLpOptions } from "./lp.ts";
export { propagate, strengthen } from "./presolve.ts";
export { findFeasible, polish } from "./heuristic.ts";
export type { Feasible } from "./heuristic.ts";
export { feasibilityPump, diveIncumbent, branchAndBound } from "./mip.ts";
export { improve as blockImprove, rowBound, schedulingBound } from "./block_planner.ts";
export { repair, constructInlinedPlanning, improvePlanning, improvePlanningAdaptive } from "./local_search.ts";
export type { Repair } from "./local_search.ts";
