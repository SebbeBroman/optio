import { test } from "node:test";
import assert from "node:assert/strict";
import { Model, INF, checkSolution, VAR_INTEGER } from "../src/model.ts";
import { StdClock } from "../src/clock.ts";
import { propagate, strengthen } from "../src/presolve.ts";
import { solve } from "../src/solve.ts";
import { defaultOptions } from "../src/options.ts";
import { ModelStatus } from "../src/status.ts";

test("cascades_and_detects_contradiction", () => {
  const m = new Model();
  const a = m.addCol(1, 1);
  const b = m.addCol(0, 1);
  const c = m.addCol(0, 1);
  for (const j of [a, b, c]) m.setIntegrality(j, VAR_INTEGER);
  m.addRow(1, 1, [
    [b, 1],
    [c, 1],
  ]);
  m.addRow(1, 1, [
    [a, 1],
    [b, 1],
  ]);
  const p = propagate(m, new StdClock(), 10) as Model;
  assert.deepEqual(p.colLower, [1, 0, 1]);
  assert.deepEqual(p.colUpper, p.colLower);
  m.addRow(-INF, 0, [[c, 1]]);
  assert.equal(propagate(m, new StdClock(), 10), null);
});

test("duplicate_and_nonunit_entries_are_not_cardinality_rows", () => {
  const m = new Model();
  const a = m.addCol(0, 1);
  m.setIntegrality(a, VAR_INTEGER);
  m.addRow(1, 1, [
    [a, 1],
    [a, 1],
  ]);
  const p = propagate(m, new StdClock(), 10) as Model;
  assert.deepEqual(p.colLower, [0]);
  assert.deepEqual(p.colUpper, [1]);
});

test("rounded_redundant_equalities_do_not_claim_infeasible", () => {
  const m = new Model();
  const x = m.addCol(0, 1000);
  const y = m.addCol(0, 1000);
  m.addRow(222.34033333, 222.34033333, [[x, 1]]);
  m.addRow(222.340333331, 222.340333331, [[y, 1]]);
  m.addRow(0, 0, [
    [x, 1],
    [y, -1],
  ]);
  assert.notEqual(propagate(m, new StdClock(), 10), null);
});

test("integer_cuts_preserve_every_feasible_binary_assignment", () => {
  const m = new Model();
  for (let i = 0; i < 5; i++) {
    const v = m.addCol(0, 1);
    m.setIntegrality(v, VAR_INTEGER);
  }
  m.addRow(-INF, 13.5, [
    [0, 7],
    [1, 6],
    [2, 5],
    [3, 4],
    [4, 3],
  ]);
  const [cut, n] = strengthen(m, m, null);
  assert.ok(n > 0);
  for (let mask = 0; mask < 32; mask++) {
    const x = [0, 1, 2, 3, 4].map((j) => (mask >> j) & 1);
    if (checkSolution(m, x, 1e-9).feasible) {
      assert.ok(checkSolution(cut, x, 1e-9).feasible);
    }
  }
});

test("weighted_propagation_preserves_feasible_mixed_domain_witnesses", () => {
  const MASK = (1n << 64n) - 1n;
  let seed = 41n;
  const rnd = () => {
    seed = (seed * 6364136223846793005n + 1n) & MASK;
    return Number((seed >> 32n) & 0xffffffffn);
  };
  for (let iter = 0; iter < 100; iter++) {
    const m = new Model();
    const x: number[] = [];
    for (let j = 0; j < 5; j++) {
      const v = m.addCol(-10, 10);
      if (j < 3) {
        m.setIntegrality(v, VAR_INTEGER);
        x.push((rnd() % 7) - 3);
      } else {
        x.push((rnd() % 70) / 10 - 3);
      }
    }
    for (let rr = 0; rr < 5; rr++) {
      const row: [number, number][] = [];
      for (let j = 0; j < 5; j++) row.push([j, (rnd() % 31) / 10 - 1.5]);
      let activity = 0;
      for (const [j, a] of row) activity += a * x[j];
      m.addRow(activity - 0.01, activity + 0.01, row);
    }
    const reduced = propagate(m, new StdClock(), 2);
    assert.notEqual(reduced, null, "feasible witness cannot become infeasible");
    assert.ok(checkSolution(reduced as Model, x, 1e-9).feasible);
  }
});

test("integer_load_auxiliaries_preserve_the_original_optimum", () => {
  const m = new Model();
  const x = m.addCol(0, 3);
  const y = m.addCol(0, 3);
  m.setIntegrality(x, VAR_INTEGER);
  m.setIntegrality(y, VAR_INTEGER);
  const peak = m.addCol(0, INF);
  const minv = m.addCol(0, INF);
  const range = m.addCol(0, INF);
  const total = m.addCol(0, INF);
  const dev = m.addCol(0, INF);
  m.setCost(peak, 0.05);
  m.setCost(range, 1.0);
  m.setCost(dev, 0.01);
  m.addRow(3, 3, [
    [x, 1],
    [y, 1],
  ]);
  m.addRow(-INF, 0, [
    [x, 2],
    [peak, -1],
  ]);
  m.addRow(-INF, 0, [
    [y, 3],
    [peak, -1],
  ]);
  m.addRow(0, INF, [
    [x, 2],
    [minv, -1],
  ]);
  m.addRow(0, INF, [
    [y, 3],
    [minv, -1],
  ]);
  m.addRow(0, INF, [
    [range, 1],
    [peak, -1],
    [minv, 1],
  ]);
  m.addRow(0, 0, [
    [total, 1],
    [x, -2],
    [y, -3],
  ]);
  m.addRow(-INF, 0, [
    [x, 4],
    [total, -1],
    [dev, -1],
  ]);
  m.addRow(-INF, 0, [
    [x, -4],
    [total, 1],
    [dev, -1],
  ]);
  const [cut] = strengthen(m, m, null);
  assert.ok(cut.colType.every((t) => t !== 0));
  const opts = defaultOptions();
  opts.timeLimitSecs = 3;
  opts.mipRelGap = 0;
  const s = solve(m, opts, new StdClock());
  assert.equal(s.status, ModelStatus.Optimal, `status ${s.status}`);
  assert.ok(Math.abs(s.objective - 1.21) < 1e-6, `obj ${s.objective}`);
  assert.ok(checkSolution(m, s.colValue, 1e-6).feasible);
});
