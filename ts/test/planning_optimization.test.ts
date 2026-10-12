import { test } from "node:test";
import assert from "node:assert/strict";
import { Model, INF, checkSolution, VAR_INTEGER } from "../src/model.ts";
import { StdClock } from "../src/clock.ts";
import { defaultOptions } from "../src/options.ts";
import { LpRelaxation, LpStatus, defaultLpOptions } from "../src/lp.ts";
import { solve, solveWithStart } from "../src/solve.ts";
import { ModelStatus } from "../src/status.ts";

test("child_basis_reoptimization_matches_cold_for_changed_bounds", () => {
  const m = new Model();
  const x = m.addCol(0, 3);
  const y = m.addCol(0, 3);
  m.setCost(x, -1);
  m.setCost(y, -2);
  m.addRow(-INF, 4, [
    [x, 1],
    [y, 1],
  ]);
  const lp = LpRelaxation.build(m);
  const root = lp.solve(defaultLpOptions(), new StdClock());
  assert.equal(root.status, LpStatus.Optimal);
  const basis = root.basis;
  assert.notEqual(basis, null, "clean optimal basis");
  let reused = 0;
  const cases: [number[], number[]][] = [
    [[0, 0], [0, 3]],
    [[0, 0], [3, 1]],
    [[2, 2], [3, 3]],
    [[3, 3], [3, 3]],
    [[3, 0], [2, 3]],
  ];
  for (const [lo, hi] of cases) {
    const warm = lp.solveWithBasis(lo, hi, defaultLpOptions(), new StdClock(), basis);
    const cold = lp.solveWithBounds(lo, hi, defaultLpOptions(), new StdClock());
    assert.equal(warm.status, cold.status);
    if (warm.status === LpStatus.Optimal) {
      assert.ok(Math.abs(warm.objective - cold.objective) < 1e-6);
      const child = m.clone();
      child.colLower = lo.slice();
      child.colUpper = hi.slice();
      assert.ok(checkSolution(child, warm.colValue, 1e-6).feasible);
      assert.ok((warm.dualBound as number) <= warm.objective + 1e-8);
    }
    reused += warm.stats.warmSolves;
  }
  assert.ok(reused >= 2, "warm path must be exercised, not only its fallback");
});

test("narrow_box_does_not_cycle_on_zero_length_bound_flips", () => {
  const m = new Model();
  const x = m.addCol(0, 1e-11);
  m.setCost(x, -1);
  const s = LpRelaxation.build(m).solve(defaultLpOptions(), new StdClock());
  assert.equal(s.status, LpStatus.Optimal);
  assert.ok(s.iterations < 5);
  assert.ok(Math.abs(s.objective + 1e-11) < 1e-15);
});

test("optimized_mip_matches_exhaustive_binary_optima", () => {
  const MASK = (1n << 64n) - 1n;
  let seed = 17n;
  const rnd = () => {
    seed = (seed * 6364136223846793005n + 1n) & MASK;
    return Number((seed >> 32n) & 0xffffffffn);
  };
  for (let trial = 0; trial < 30; trial++) {
    const m = new Model();
    for (let j = 0; j < 6; j++) {
      const x = m.addCol(0, 1);
      m.setIntegrality(x, VAR_INTEGER);
      m.setCost(x, -((rnd() % 10) + 1));
    }
    for (let r = 0; r < 3; r++) {
      const row: [number, number][] = [];
      for (let j = 0; j < 6; j++) row.push([j, (rnd() % 9) + 1]);
      m.addRow(-INF, (rnd() % 15) + 5, row);
    }
    let best = Infinity;
    for (let bits = 0; bits < 64; bits++) {
      const x: number[] = [];
      for (let j = 0; j < 6; j++) x.push((bits >> j) & 1);
      const c = checkSolution(m, x, 1e-8);
      if (c.feasible) best = Math.min(best, c.objective);
    }
    for (const threads of [1, 2]) {
      const opts = defaultOptions();
      opts.threads = threads;
      opts.timeLimitSecs = 3;
      opts.mipRelGap = 0;
      const s = solve(m, opts, new StdClock());
      assert.equal(s.status, ModelStatus.Optimal, `trial ${trial} threads ${threads}`);
      assert.ok(Math.abs(s.objective - best) < 1e-6, `wrong optimum ${s.objective} vs ${best}`);
      assert.ok((s.dualBound as number) <= best + 1e-7);
      assert.ok(checkSolution(m, s.colValue, 1e-6).feasible);
    }
  }
});

test("tolerance_feasible_seed_does_not_make_objective_domains_empty", () => {
  const m = new Model();
  const b = m.addCol(1, 1);
  m.setIntegrality(b, VAR_INTEGER);
  const y = m.addCol(0, 2);
  m.setCost(y, 1);
  m.addRow(0, INF, [
    [y, 1],
    [b, -1],
  ]);
  const s = solveWithStart(m, defaultOptions(), new StdClock(), {}, [1.0, 1.0 - 5e-7]);
  assert.notEqual(s.status, ModelStatus.Infeasible);
  assert.ok(checkSolution(m, s.colValue, 1e-6).feasible);
});
