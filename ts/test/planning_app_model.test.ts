import { test } from "node:test";
import assert from "node:assert/strict";
import { checkSolution } from "../src/model.ts";
import { StdClock } from "../src/clock.ts";
import { defaultOptions } from "../src/options.ts";
import { solve, solveWithStart } from "../src/solve.ts";
import { branchAndBound } from "../src/mip.ts";
import { ModelStatus } from "../src/status.ts";
import type { Incumbent, Progress } from "../src/types.ts";
import { appShapedModel } from "./fixtures.ts";

test("branch_and_bound_solves_app_model", () => {
  const m = appShapedModel(7);
  const opts = defaultOptions();
  opts.timeLimitSecs = 5;
  const sol = solve(m, opts, new StdClock());
  assert.ok(sol.hasSolution(), `no solution: status ${sol.status}`);
  const chk = checkSolution(m, sol.colValue, 1e-6);
  assert.ok(chk.feasible, "check_solution rejected B&B solution");
  assert.ok(Math.abs(chk.objective - 251.9) < 1e-2, `obj=${chk.objective}`);
});

test("branch_and_bound_rounding_polishes_root_candidate", () => {
  const m = appShapedModel(7);
  const opts = defaultOptions();
  opts.timeLimitSecs = 5;
  const sol = branchAndBound(m, opts, new StdClock(), {}, null, null);
  assert.ok(sol.hasSolution(), `B&B found no incumbent: ${sol.status}`);
  assert.ok(checkSolution(m, sol.colValue, 1e-6).feasible);
});

test("placement_start_is_reported_and_can_stop_before_search", () => {
  const m = appShapedModel(7);
  const seed = [1, 4, 4, 3, 1, 2, 0, 0, 0, 0, 0, 0];
  assert.ok(!checkSolution(m, seed, 1e-6).feasible);
  const candidates: number[][] = [];
  let stop = false;
  const sol = solveWithStart(
    m,
    defaultOptions(),
    new StdClock(),
    {
      onIncumbent: (inc: Incumbent) => candidates.push(inc.colValue.slice()),
      onProgress: (p: Progress) => {
        assert.ok(p.hasIncumbent);
        stop = true;
      },
      interrupt: () => stop,
    },
    seed,
  );
  assert.equal(sol.status, ModelStatus.Interrupt);
  assert.equal(sol.nodes, 0, "a caller-accepted start needs no B&B search");
  assert.equal(sol.lpIterations, 0);
  assert.equal(candidates.length, 1);
  assert.deepEqual(candidates[0].slice(0, 6), seed.slice(0, 6), "polish changed integer placements");
  assert.ok(checkSolution(m, candidates[0], 1e-6).feasible);
});

test("invalid_start_does_not_discard_the_heuristic_incumbent", () => {
  const m = appShapedModel(7);
  const seed = new Array<number>(m.numCol).fill(0);
  const sol = solveWithStart(m, defaultOptions(), new StdClock(), {}, seed);
  assert.ok(sol.hasSolution());
  assert.ok(checkSolution(m, sol.colValue, 1e-6).feasible);
});

test("non_finite_candidates_are_never_feasible", () => {
  const m = appShapedModel(7);
  const sol = solve(m, defaultOptions(), new StdClock());
  assert.ok(sol.hasSolution());
  for (const invalid of [NaN, Infinity, -Infinity]) {
    for (const col of [0, 6]) {
      const x = sol.colValue.slice();
      x[col] = invalid;
      assert.ok(!checkSolution(m, x, 1e-6).feasible);
    }
  }
});
