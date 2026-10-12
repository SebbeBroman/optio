import { test } from "node:test";
import assert from "node:assert/strict";
import { Model, INF } from "../src/model.ts";
import { StdClock } from "../src/clock.ts";
import { LpRelaxation, LpStatus, defaultLpOptions } from "../src/lp.ts";

function solveModel(m: Model) {
  const lpr = LpRelaxation.build(m);
  return lpr.solve(defaultLpOptions(), new StdClock());
}

test("simple_lp_optimum", () => {
  // min -x - 10y  s.t. x + 7y <= 17.5, 0<=x<=3.5, y>=0.
  // LP optimum: (0, 2.5) -> -25.
  const m = new Model();
  const x = m.addCol(0, 3.5);
  const y = m.addCol(0, INF);
  m.setCost(x, -1);
  m.setCost(y, -10);
  m.addRow(-INF, 17.5, [
    [x, 1],
    [y, 7],
  ]);
  const s = solveModel(m);
  assert.equal(s.status, LpStatus.Optimal, `status ${s.status}`);
  assert.ok(Math.abs(s.objective - -25) < 1e-6, `obj ${s.objective}`);
});

test("equality_and_bounds", () => {
  const m = new Model();
  const x = m.addCol(0, 3);
  const y = m.addCol(0, 3);
  m.setCost(x, 1);
  m.setCost(y, 1);
  m.addRow(4, 4, [
    [x, 1],
    [y, 1],
  ]);
  const s = solveModel(m);
  assert.equal(s.status, LpStatus.Optimal);
  assert.ok(Math.abs(s.objective - 4) < 1e-6);
});

test("detects_infeasible", () => {
  const m = new Model();
  const x = m.addCol(0, 3);
  m.setCost(x, 1);
  m.addRow(5, INF, [[x, 1]]);
  const s = solveModel(m);
  assert.equal(s.status, LpStatus.Infeasible);
});

test("detects_unbounded", () => {
  const m = new Model();
  const x = m.addCol(0, INF);
  m.setCost(x, -1);
  m.addRow(0, INF, [
    [x, 1],
    [x, 1],
  ]);
  const s = solveModel(m);
  assert.equal(s.status, LpStatus.Unbounded);
});

test("two_var_transport_like", () => {
  const m = new Model();
  const a = m.addCol(0, 10);
  const b = m.addCol(0, 10);
  const c = m.addCol(0, 10);
  const d = m.addCol(0, 10);
  m.setCost(a, 2);
  m.setCost(b, 3);
  m.setCost(c, 4);
  m.setCost(d, 1);
  m.addRow(10, 10, [
    [a, 1],
    [b, 1],
  ]);
  m.addRow(10, 10, [
    [c, 1],
    [d, 1],
  ]);
  m.addRow(-INF, 12, [
    [a, 1],
    [c, 1],
  ]);
  m.addRow(-INF, 12, [
    [b, 1],
    [d, 1],
  ]);
  const s = solveModel(m);
  assert.equal(s.status, LpStatus.Optimal, `status ${s.status}`);
  assert.ok(Math.abs(s.objective - 30) < 1e-6, `obj ${s.objective}`);
});

test("narrow_box_no_cycle", () => {
  const m = new Model();
  const x = m.addCol(0, 1e-11);
  m.setCost(x, -1);
  const s = solveModel(m);
  assert.equal(s.status, LpStatus.Optimal);
  assert.ok(s.iterations < 5, `iters ${s.iterations}`);
  assert.ok(Math.abs(s.objective + 1e-11) < 1e-15);
});
