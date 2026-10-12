// Model status codes, numbered to match the HiGHS / `highs-js` `ModelStatus`
// enum so a differential harness can compare them directly.
// (Port of `crates/core/src/status.rs`.)

export const ModelStatus = {
  NotSet: 0,
  LoadError: 1,
  ModelError: 2,
  PresolveError: 3,
  SolveError: 4,
  PostsolveError: 5,
  Empty: 6,
  Optimal: 7,
  Infeasible: 8,
  UnboundedOrInfeasible: 9,
  Unbounded: 10,
  ObjectiveBound: 11,
  ObjectiveTarget: 12,
  TimeLimit: 13,
  IterationLimit: 14,
  Unknown: 15,
  SolutionLimit: 16,
  Interrupt: 17,
} as const;
export type ModelStatus = (typeof ModelStatus)[keyof typeof ModelStatus];

export function statusIsInfeasible(s: ModelStatus): boolean {
  return s === ModelStatus.Infeasible || s === ModelStatus.UnboundedOrInfeasible;
}

export function statusHasFeasibleSolution(s: ModelStatus): boolean {
  return (
    s === ModelStatus.Optimal ||
    s === ModelStatus.TimeLimit ||
    s === ModelStatus.IterationLimit ||
    s === ModelStatus.ObjectiveBound ||
    s === ModelStatus.ObjectiveTarget ||
    s === ModelStatus.SolutionLimit ||
    s === ModelStatus.Interrupt
  );
}
