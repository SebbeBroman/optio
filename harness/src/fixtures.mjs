// Fixture catalogue. Files live in ../fixtures; `seconds` is the per-fixture
// time limit.
//
// - Small linear LP/MIP regression instances from HiGHS.
// - planning-export: a real app export (anonymized), taken from the app's
//   infeasible/IIS branch, but independently proved feasible.
// - planning-infeasible: planning-s with six ring capacities reduced to one;
//   mandatory demand exceeds available capacity, including in the LP relaxation.
// - planning-s/m/l/tight: synthetically generated, guaranteed-feasible
//   planning-shaped MILPs (see tools/gen-planning.mjs).
export const FIXTURES = [
  { file: '1451.lp', seconds: 5 },
  { file: 'fixed-binary.lp', seconds: 5 },
  { file: 'semi-integer.lp', seconds: 5 },
  { file: 'issue-2585.lp', seconds: 10 },
  { file: 'issue-2388.lp', seconds: 20 },
  { file: '2122.lp', seconds: 30 },
  { file: 'life_goe.mod.lp', seconds: 20 },
  { file: 'exceeds_stack.lp', seconds: 30 },
  { file: 'planning-s.lp', seconds: 10 },
  { file: 'planning-faithful.lp', seconds: 10 },
  { file: 'planning-m.lp', seconds: 20 },
  { file: 'planning-l.lp', seconds: 30 },
  { file: 'planning-tight.lp', seconds: 20 },
  { file: 'planning-export.lp', seconds: 30 },
  { file: 'planning-infeasible.lp', seconds: 5 },
];

// Statuses for which no primal solution is expected; the oracle reports "n/a".
export const NO_SOLUTION_STATUSES = new Set([
  'NotSet', 'LoadError', 'ModelError', 'PresolveError', 'SolveError',
  'PostsolveError', 'Empty', 'Infeasible', 'UnboundedOrInfeasible', 'Unbounded',
]);
