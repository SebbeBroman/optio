import { test } from "node:test";
import assert from "node:assert/strict";
import { Model, INF, VAR_INTEGER, VAR_BINARY, checkSolution } from "../src/model.ts";
import { StdClock } from "../src/clock.ts";
import { defaultOptions } from "../src/options.ts";
import { findFeasible } from "../src/heuristic.ts";
import { appShapedModel } from "./fixtures.ts";

test("finds_feasible_assignment", () => {
  const m = new Model();
  for (let i = 0; i < 3; i++) m.addCol(0, 2);
  for (let c = 0; c < 3; c++) m.setIntegrality(c, VAR_INTEGER);
  m.addRow(4, 4, [
    [0, 1],
    [1, 1],
    [2, 1],
  ]);
  const opts = defaultOptions();
  opts.timeLimitSecs = 2;
  opts.seed = 7;
  const f = findFeasible(m, opts, new StdClock(), 100000);
  assert.notEqual(f, null);
  let sum = 0;
  for (const v of (f as { x: number[] }).x) sum += v;
  assert.ok(Math.abs(sum - 4) < 1e-9);
});

test("finds_feasible_with_linked_fixed_charge", () => {
  const m = new Model();
  const x: number[][] = [
    [0, 0],
    [0, 0],
  ];
  const z: number[][] = [
    [0, 0],
    [0, 0],
  ];
  for (let u = 0; u < 2; u++) {
    for (let r = 0; r < 2; r++) {
      x[u][r] = m.addCol(0, 2);
      z[u][r] = m.addCol(0, 1);
      m.setIntegrality(x[u][r], VAR_INTEGER);
      m.setIntegrality(z[u][r], VAR_BINARY);
    }
    m.addRow(2, 2, [
      [x[u][0], 1],
      [x[u][1], 1],
    ]);
    m.addRow(0, 1, [
      [z[u][0], 1],
      [z[u][1], 1],
    ]);
    for (let r = 0; r < 2; r++) {
      m.addRow(-INF, 0, [
        [x[u][r], 1],
        [z[u][r], -2],
      ]);
    }
  }
  const peak = m.addCol(0, INF);
  const load = m.addCol(0, INF);
  const terms: [number, number][] = [[load, 1]];
  for (let u = 0; u < 2; u++) {
    for (let r = 0; r < 2; r++) terms.push([x[u][r], -100]);
  }
  m.addRow(0, 0, terms);
  m.addRow(0, INF, [
    [load, 1],
    [peak, -1],
  ]);
  const opts = defaultOptions();
  opts.timeLimitSecs = 2;
  opts.seed = 3;
  const f = findFeasible(m, opts, new StdClock(), 200000);
  assert.notEqual(f, null);
  assert.ok(checkSolution(m, (f as { x: number[] }).x, 1e-6).feasible);
});

test("find_feasible_accepts_app_model_placement", () => {
  const m = appShapedModel(7);
  const opts = defaultOptions();
  opts.timeLimitSecs = 5;
  const f = findFeasible(m, opts, new StdClock(), 200000);
  assert.notEqual(f, null, "find_feasible must return a feasible app-model plan");
  const x = (f as { x: number[] }).x;
  const chk = checkSolution(m, x, 1e-6);
  assert.ok(chk.feasible, `check_solution rejected the plan`);
  assert.ok(Math.abs(x[0] - 1) < 1e-9, `y=${x[0]}`);
  assert.ok(Math.abs(x[1] - 4) < 1e-9, `x1=${x[1]}`);
  assert.ok(Math.abs(x[3] - 3) < 1e-9, `x2=${x[3]}`);
});

test("polish_does_not_hide_real_infeasibility", () => {
  const m = appShapedModel(8);
  const opts = defaultOptions();
  opts.timeLimitSecs = 3;
  const f = findFeasible(m, opts, new StdClock(), 200000);
  assert.equal(f, null, "heuristic returned an incumbent on an infeasible model");
});
