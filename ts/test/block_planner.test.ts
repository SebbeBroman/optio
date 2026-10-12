import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { Model, INF, VAR_BINARY, checkSolution } from "../src/model.ts";
import { StdClock } from "../src/clock.ts";
import { improve, rowBound, schedulingBound } from "../src/block_planner.ts";

interface CsrJson {
  colCost: number[];
  colLower: number[];
  colUpper: number[];
  rowLower: number[];
  rowUpper: number[];
  integrality: number[];
  matrix: { starts: number[]; indices: number[]; values: number[] };
}

interface StartJson {
  indices: number[];
  values: number[];
}

function loadFixture(name: string): Model {
  const url = new URL(`../../harness/fixtures/planning-app-${name}.model.json`, import.meta.url);
  const j = JSON.parse(readFileSync(url, "utf8")) as CsrJson;
  const m = new Model();
  m.addCsr(
    j.colCost.length,
    j.colCost,
    j.colLower,
    j.colUpper,
    j.integrality,
    j.rowLower,
    j.rowUpper,
    j.matrix.starts,
    j.matrix.indices,
    j.matrix.values,
  );
  return m;
}

function loadStart(name: string, numCol: number): number[] {
  const url = new URL(`../../harness/fixtures/planning-app-${name}.start.json`, import.meta.url);
  const j = JSON.parse(readFileSync(url, "utf8")) as StartJson;
  const x = new Array<number>(numCol).fill(0);
  for (let i = 0; i < j.indices.length; i++) x[j.indices[i]] = j.values[i];
  return x;
}

function popcount(v: number): number {
  let c = 0;
  let x = v;
  while (x !== 0) {
    c += x & 1;
    x >>= 1;
  }
  return c;
}

// Mirrors `identical_templates_share_patterns_and_medium_plan_is_checked` (the
// pattern-class count needs `detect`, which is not exported; the objective and
// feasibility checks are kept).
test("medium_plan_is_improved_and_checked", () => {
  const m = loadFixture("medium");
  const clock = new StdClock();
  const x = improve(m, null, clock, 10);
  assert.notEqual(x, null, "unseeded medium plan");
  assert.ok(checkSolution(m, x as number[], 1e-6).feasible);
  assert.ok(Math.abs(m.objective(x as number[]) - 3096.8) < 1e-9, `obj ${m.objective(x as number[])}`);
});

// Mirrors `medium_integer_peak_certificate_closes_the_real_gap`.
test("medium_integer_peak_certificate_closes_the_real_gap", () => {
  const m = loadFixture("medium");
  const clock = new StdClock();
  const bound = schedulingBound(m, clock, clock.nowSecs() + 5);
  assert.notEqual(bound, null);
  assert.ok((bound as number) <= 3096.8);
  assert.ok(3096.8 - (bound as number) < 1e-8);
});

// Mirrors `large_plan_respects_capacity_and_ring_order_without_seed`.
test("large_plan_respects_capacity_and_ring_order_without_seed", () => {
  const m = loadFixture("large");
  const clock = new StdClock();
  const x = improve(m, null, clock, 30);
  assert.notEqual(x, null, "unseeded large plan");
  assert.ok(checkSolution(m, x as number[], 1e-6).feasible);
  assert.ok(m.objective(x as number[]) < 10000);
  const rb = rowBound(m);
  assert.notEqual(rb, null);
  assert.ok((rb as number) <= m.objective(x as number[]) + 1e-9);
});

// Mirrors `row_certificate_never_exceeds_exhaustive_optimum`.
test("row_certificate_never_exceeds_exhaustive_optimum", () => {
  for (let seed = 0; seed < 64; seed++) {
    const m = new Model();
    const vars: number[] = [];
    for (let j = 0; j < 4; j++) {
      const v = m.addCol(0, 1);
      m.setIntegrality(v, VAR_BINARY);
      m.setCost(v, ((seed * 17 + j * 7) % 11) - 5);
      vars.push(v);
    }
    const p = m.addCol(0, 1e9);
    m.setCost(p, 0.05);
    const durations: number[] = [];
    for (let j = 0; j < 4; j++) {
      const d = ((seed * 3 + j * 5) % 13) + 1;
      durations.push(d);
      m.addRow(-INF, 0, [
        [vars[j], d],
        [p, -1],
      ]);
    }
    m.addRow(2, 2, vars.map((v) => [v, 1] as [number, number]));
    let optimum = Infinity;
    for (let mask = 0; mask < 16; mask++) {
      if (popcount(mask) !== 2) continue;
      const x = [0, 0, 0, 0, 0];
      for (let j = 0; j < 4; j++) x[j] = (mask >> j) & 1;
      x[4] = Math.max(...durations.map((d, j) => x[j] * d));
      assert.ok(checkSolution(m, x, 1e-9).feasible);
      optimum = Math.min(optimum, m.objective(x));
    }
    const bound = rowBound(m);
    assert.notEqual(bound, null, `seed ${seed}`);
    assert.ok((bound as number) <= optimum, `seed ${seed}: ${bound} > ${optimum}`);
  }
});

// Mirrors `seeded_real_app_plan_is_improved_and_proved` (without the solver:
// the seed is feasible, improvement reaches the certified objective).
test("seeded_medium_plan_is_improved", () => {
  const m = loadFixture("medium");
  const start = loadStart("medium", m.numCol);
  assert.ok(checkSolution(m, start, 1e-9).feasible);
  const clock = new StdClock();
  const x = improve(m, start, clock, 10);
  assert.notEqual(x, null);
  assert.ok(checkSolution(m, x as number[], 1e-6).feasible);
  assert.ok(Math.abs(m.objective(x as number[]) - 3096.8) < 1e-9);
});

// Mirrors `a_checked_start_is_never_replaced_by_a_worse_plan`.
test("a_checked_start_is_never_replaced_by_a_worse_plan", () => {
  const m = loadFixture("medium");
  const clock = new StdClock();
  const start = improve(m, null, clock, 10);
  assert.notEqual(start, null);
  const x = improve(m, start, clock, 10);
  if (x !== null) {
    assert.ok(checkSolution(m, x, 1e-6).feasible);
    assert.ok(m.objective(x) <= m.objective(start as number[]) + 1e-7);
  }
});

// Mirrors `integer_certificate_rejects_incomplete_round_definitions`: breaking a
// round-definition coefficient makes the integer peak certificate unavailable.
test("integer_certificate_rejects_incomplete_round_definitions", () => {
  const m = loadFixture("medium");
  const clock = new StdClock();
  const before = schedulingBound(m, clock, clock.nowSecs() + 5);
  assert.notEqual(before, null);
  let k = -1;
  for (let r = 0; r < m.numRow && k < 0; r++) {
    if (m.rowLower[r] !== 0 || m.rowUpper[r] !== 0) continue;
    for (let i = m.aStart[r]; i < m.aStart[r + 1]; i++) {
      if (m.aValue[i] === -1) {
        k = i;
        break;
      }
    }
  }
  assert.ok(k >= 0, "found a round-definition coefficient to break");
  m.aValue[k] = -2;
  assert.equal(schedulingBound(m, clock, clock.nowSecs() + 5), null);
});

// No detectable scheduling structure -> `detect` bails out and `improve` is null.
test("no_detectable_structure_returns_null", () => {
  const m = new Model();
  const a = m.addCol(0, 10);
  const b = m.addCol(0, 10);
  m.setCost(a, 1);
  m.setCost(b, 2);
  m.addRow(0, 5, [
    [a, 1],
    [b, 1],
  ]);
  const clock = new StdClock();
  assert.equal(improve(m, null, clock, 1), null);
});

// Any non-null point from `improve` must pass the independent oracle.
test("improve_never_returns_an_infeasible_point", () => {
  const clock = new StdClock();
  const m = loadFixture("medium");
  const x = improve(m, null, clock, 10);
  assert.notEqual(x, null);
  const chk = checkSolution(m, x as number[], 1e-9);
  assert.ok(chk.feasible, JSON.stringify(chk));
});
