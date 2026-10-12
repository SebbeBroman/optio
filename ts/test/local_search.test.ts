import { test } from "node:test";
import assert from "node:assert/strict";
import { Model, INF, VAR_INTEGER, VAR_BINARY, checkSolution } from "../src/model.ts";
import { StdClock } from "../src/clock.ts";
import {
  repair,
  constructInlinedPlanning,
  improvePlanning,
  improvePlanningAdaptive,
} from "../src/local_search.ts";

/** Bounds-clamped zero point (port of `crate::initial_point`). */
function initialPoint(model: Model): number[] {
  const x: number[] = [];
  for (let c = 0; c < model.numCol; c++) {
    x.push(0.0 < model.colLower[c] ? model.colLower[c] : 0.0 > model.colUpper[c] ? model.colUpper[c] : 0.0);
  }
  return x;
}

interface PlanningBuild {
  m: Model;
  x: number[][][];
  z: number[][][];
  matches: number[];
}

/**
 * Build a planning-shaped model natively, mirroring the `planning_build` helper
 * in `local_search.rs`: integer x[u,p,r], binary z[u,p,r], per-(p,r)
 * load = sum dur*x, cap, and per-session peak/min/range/deviation auxiliaries.
 */
function planningBuild(
  units: number,
  sessions: number,
  rings: number,
  capOverride: number | null,
): PlanningBuild {
  const m = new Model();
  const rng = (i: number, mul: number): number => ((i * mul) % 100) / 100.0;
  const matches: number[] = [];
  for (let u = 0; u < units; u++) matches.push(Math.round(1.0 + 4.0 * rng(u, 37)));
  const dur: number[] = [];
  for (let u = 0; u < units; u++) dur.push(Math.round(600.0 + 600.0 * rng(u, 53)));

  const x: number[][][] = [];
  const z: number[][][] = [];
  for (let u = 0; u < units; u++) {
    const xu: number[][] = [];
    const zu: number[][] = [];
    for (let p = 0; p < sessions; p++) {
      const xp: number[] = [];
      const zp: number[] = [];
      for (let r = 0; r < rings; r++) {
        const xi = m.addCol(0.0, matches[u]);
        m.setIntegrality(xi, VAR_INTEGER);
        const zi = m.addCol(0.0, 1.0);
        m.setIntegrality(zi, VAR_BINARY);
        xp.push(xi);
        zp.push(zi);
      }
      xu.push(xp);
      zu.push(zp);
    }
    x.push(xu);
    z.push(zu);
  }

  const wload: number[][] = [];
  for (let p = 0; p < sessions; p++) wload.push(new Array<number>(rings).fill(0.0));
  for (let u = 0; u < units; u++) {
    wload[u % sessions][u % rings] += matches[u] * dur[u];
  }
  let witnessMax = 0.0;
  for (const row of wload) for (const v of row) witnessMax = v > witnessMax ? v : witnessMax;
  const cap = capOverride !== null ? capOverride : Math.ceil(witnessMax * 1.5);

  for (let u = 0; u < units; u++) {
    const tot: Array<[number, number]> = [];
    for (let p = 0; p < sessions; p++) {
      for (let r = 0; r < rings; r++) tot.push([x[u][p][r], 1.0]);
    }
    m.addRow(matches[u], matches[u], tot);
    for (let p = 0; p < sessions; p++) {
      const zt: Array<[number, number]> = [];
      for (let r = 0; r < rings; r++) {
        m.addRow(-INF, 0.0, [
          [x[u][p][r], 1.0],
          [z[u][p][r], -matches[u]],
        ]);
        zt.push([z[u][p][r], 1.0]);
      }
      m.addRow(-INF, 1.0, zt);
    }
  }

  const load: number[][] = [];
  for (let p = 0; p < sessions; p++) load.push(new Array<number>(rings).fill(0));
  for (let p = 0; p < sessions; p++) {
    for (let r = 0; r < rings; r++) {
      const l = m.addCol(0.0, INF);
      load[p][r] = l;
      const terms: Array<[number, number]> = [[l, 1.0]];
      for (let u = 0; u < units; u++) terms.push([x[u][p][r], -dur[u]]);
      m.addRow(0.0, 0.0, terms); // load = sum dur*x
      m.addRow(-INF, cap, [[l, 1.0]]); // cap
    }
    const peak = m.addCol(0.0, INF);
    const minv = m.addCol(0.0, INF);
    const total = m.addCol(0.0, INF);
    const range = m.addCol(0.0, INF);
    const totDef: Array<[number, number]> = [[total, 1.0]];
    for (let r = 0; r < rings; r++) {
      totDef.push([load[p][r], -1.0]);
      m.addRow(-INF, 0.0, [
        [load[p][r], 1.0],
        [peak, -1.0],
      ]); // peak >= load
      m.addRow(0.0, INF, [
        [load[p][r], 1.0],
        [minv, -1.0],
      ]); // min <= load
      const dev = m.addCol(0.0, INF);
      const hi: Array<[number, number]> = [
        [dev, -1.0],
        [total, -1.0],
      ];
      const lo: Array<[number, number]> = [
        [dev, -1.0],
        [total, 1.0],
      ];
      for (let rr = 0; rr < rings; rr++) {
        hi.push([load[p][rr], rings]);
        lo.push([load[p][rr], -rings]);
      }
      m.addRow(-INF, 0.0, hi);
      m.addRow(-INF, 0.0, lo);
    }
    m.addRow(0.0, 0.0, totDef);
    m.addRow(0.0, INF, [
      [range, 1.0],
      [peak, -1.0],
      [minv, 1.0],
    ]);
  }
  return { m, x, z, matches };
}

function planningLikeCap(units: number, sessions: number, rings: number, cap: number | null): Model {
  return planningBuild(units, sessions, rings, cap).m;
}

function planningLike(units: number, sessions: number, rings: number): Model {
  return planningLikeCap(units, sessions, rings, null);
}

/**
 * Inlined planning model (`sum dur*x` directly in cap/peak/dev rows, no `load`
 * column) — the shape of the real `planning-*.lp` fixtures.
 */
function planningInlined(units: number, sessions: number, rings: number): Model {
  const m = new Model();
  const matches: number[] = [];
  for (let u = 0; u < units; u++) matches.push(1 + ((u * 37) % 4));
  const dur: number[] = [];
  for (let u = 0; u < units; u++) dur.push(100 + ((u * 53) % 9) * 100);
  const x: number[][][] = [];
  const z: number[][][] = [];
  for (let u = 0; u < units; u++) {
    const xu: number[][] = [];
    const zu: number[][] = [];
    for (let p = 0; p < sessions; p++) {
      const xp: number[] = [];
      const zp: number[] = [];
      for (let r = 0; r < rings; r++) {
        const xi = m.addCol(0.0, matches[u]);
        m.setIntegrality(xi, VAR_INTEGER);
        const zi = m.addCol(0.0, 1.0);
        m.setIntegrality(zi, VAR_INTEGER);
        xp.push(xi);
        zp.push(zi);
      }
      xu.push(xp);
      zu.push(zp);
    }
    x.push(xu);
    z.push(zu);
  }
  for (let u = 0; u < units; u++) {
    const terms: Array<[number, number]> = [];
    for (let p = 0; p < sessions; p++) {
      for (let r = 0; r < rings; r++) terms.push([x[u][p][r], 1.0]);
    }
    m.addRow(matches[u], matches[u], terms);
    for (let p = 0; p < sessions; p++) {
      for (let r = 0; r < rings; r++) {
        m.addRow(-INF, 0.0, [
          [x[u][p][r], 1.0],
          [z[u][p][r], -matches[u]],
        ]);
      }
      const zt: Array<[number, number]> = [];
      for (let r = 0; r < rings; r++) zt.push([z[u][p][r], 1.0]);
      m.addRow(-INF, 1.0, zt);
    }
  }
  const wload: number[][] = [];
  for (let p = 0; p < sessions; p++) wload.push(new Array<number>(rings).fill(0.0));
  for (let u = 0; u < units; u++) {
    wload[u % sessions][u % rings] += matches[u] * dur[u];
  }
  let wmax = 0.0;
  for (const row of wload) for (const v of row) wmax = v > wmax ? v : wmax;
  const cap = Math.ceil(wmax * 1.2) + 1.0;
  for (let p = 0; p < sessions; p++) {
    const total = m.addCol(0.0, INF);
    const tt: Array<[number, number]> = [[total, 1.0]];
    for (let u = 0; u < units; u++) {
      for (let r = 0; r < rings; r++) tt.push([x[u][p][r], -dur[u]]);
    }
    m.addRow(0.0, 0.0, tt);
    const peak = m.addCol(0.0, INF);
    const minv = m.addCol(0.0, INF);
    m.setCost(peak, 0.05);
    for (let r = 0; r < rings; r++) {
      const loadr: Array<[number, number]> = [];
      for (let u = 0; u < units; u++) loadr.push([x[u][p][r], dur[u]]);
      m.addRow(-INF, cap, loadr);
      const pk: Array<[number, number]> = loadr.slice();
      pk.push([peak, -1.0]);
      m.addRow(-INF, 0.0, pk);
      const mn: Array<[number, number]> = loadr.slice();
      mn.push([minv, -1.0]);
      m.addRow(0.0, INF, mn);
      const dev = m.addCol(0.0, INF);
      m.setCost(dev, 0.01);
      const hi: Array<[number, number]> = [
        [dev, -1.0],
        [total, -1.0],
      ];
      const lo: Array<[number, number]> = [
        [dev, -1.0],
        [total, 1.0],
      ];
      for (let u = 0; u < units; u++) {
        hi.push([x[u][p][r], rings * dur[u]]);
        lo.push([x[u][p][r], -rings * dur[u]]);
      }
      m.addRow(-INF, 0.0, hi);
      m.addRow(-INF, 0.0, lo);
    }
    const range = m.addCol(0.0, INF);
    m.setCost(range, 1.0);
    m.addRow(0.0, INF, [
      [range, 1.0],
      [peak, -1.0],
      [minv, 1.0],
    ]);
  }
  return m;
}

test("repairs_small_planning_like", () => {
  const m = planningLike(8, 3, 5);
  const x0 = initialPoint(m);
  const r = repair(m, x0, 200000, 42);
  assert.ok(r !== null, "repair should find a feasible plan");
  const chk = checkSolution(m, r.x, 1e-6);
  assert.ok(chk.feasible, `checkSolution: ${JSON.stringify(chk)}`);
  assert.ok(r.moves < 200000);
});

test("repairs_large_planning_like", () => {
  const m = planningLike(40, 3, 5);
  const x0 = initialPoint(m);
  const r = repair(m, x0, 500000, 7);
  assert.ok(r !== null, "repair should find a feasible plan");
  const chk = checkSolution(m, r.x, 1e-6);
  assert.ok(chk.feasible, `checkSolution: ${JSON.stringify(chk)}`);
});

test("infeasible_instance_returns_none", () => {
  // cap = 0 while every unit must place >= 1 match: no feasible point.
  const m = planningLikeCap(6, 2, 3, 0.0);
  const x0 = initialPoint(m);
  const r = repair(m, x0, 50000, 3);
  assert.ok(r === null, "infeasible instance must not report a repair");
});

test("returned_solution_passes_check_solution", () => {
  for (const [units, seed] of [
    [8, 1],
    [16, 2],
    [24, 5],
  ] as Array<[number, number]>) {
    const m = planningLike(units, 3, 5);
    const x0 = initialPoint(m);
    const r = repair(m, x0, 300000, seed);
    assert.ok(r !== null, `units=${units}: repair returned None`);
    const chk = checkSolution(m, r.x, 1e-6);
    assert.ok(chk.feasible, `units=${units}: ${JSON.stringify(chk)}`);
  }
});

test("repair_is_deterministic", () => {
  const m = planningLike(10, 3, 4);
  const x0 = initialPoint(m);
  const a = repair(m, x0, 200000, 99);
  const b = repair(m, x0, 200000, 99);
  assert.ok(a !== null && b !== null);
  assert.deepEqual(a.x, b.x);
  assert.equal(a.moves, b.moves);
});

test("repairs_transfers_from_cap_congested_start", () => {
  // A start where `tot` is met but every unit is stacked on session 0 / ring 0,
  // blowing the cap. Only the coupled transfer move can fix this.
  for (const units of [8, 40]) {
    const { m, x, z, matches } = planningBuild(units, 3, 5, null);
    const x0 = new Array<number>(m.numCol).fill(0.0);
    for (let u = 0; u < units; u++) {
      x0[x[u][0][0]] = matches[u];
      x0[z[u][0][0]] = 1.0;
    }
    const r = repair(m, x0, 2000000, units + 1);
    assert.ok(r !== null, `units=${units}: transfer repair failed`);
    const chk = checkSolution(m, r.x, 1e-6);
    assert.ok(chk.feasible, `units=${units}: ${JSON.stringify(chk)}`);
  }
});

test("construct_inlined_planning_is_feasible", () => {
  for (const [units, sessions, rings] of [
    [8, 2, 3],
    [24, 3, 5],
    [60, 3, 5],
  ] as Array<[number, number, number]>) {
    const m = planningInlined(units, sessions, rings);
    const x = constructInlinedPlanning(m);
    assert.ok(x !== null, `units=${units}: construct returned None`);
    const chk = checkSolution(m, x, 1e-6);
    assert.ok(chk.feasible, `units=${units}: ${JSON.stringify(chk)}`);
    assert.ok(m.objective(x) > 0.0);
  }
});

test("improve_planning_returns_feasible_no_worse_objective", () => {
  for (const [units, sessions, rings] of [
    [24, 3, 5],
    [60, 3, 5],
  ] as Array<[number, number, number]>) {
    const m = planningInlined(units, sessions, rings);
    const x0 = constructInlinedPlanning(m);
    assert.ok(x0 !== null, `units=${units}: construct returned None`);
    const o0 = m.objective(x0);
    const res = improvePlanning(m, x0, new StdClock(), 2.0, 1);
    assert.ok(res !== null, `units=${units}: improve returned None`);
    const [o1, x1] = res;
    const chk = checkSolution(m, x1, 1e-6);
    assert.ok(chk.feasible, `units=${units}: ${JSON.stringify(chk)}`);
    assert.ok(o1 <= o0 + 1e-6, `units=${units}: objective ${o1} worse than start ${o0}`);
  }
});

test("smoke_repair_construct_improve_null_or_feasible", () => {
  // repair on a planning-shaped model with the all-zero start.
  const rm = planningLike(8, 3, 5);
  const rx = repair(rm, initialPoint(rm), 200000, 123);
  if (rx !== null) {
    assert.ok(checkSolution(rm, rx.x, 1e-6).feasible);
  }

  // construct + improve on the inlined shape; a feasible start is derived from
  // the constructor (or the zero point) and the search must never return an
  // infeasible point.
  const m = planningInlined(8, 2, 3);
  const constructed = constructInlinedPlanning(m);
  if (constructed !== null) {
    assert.ok(checkSolution(m, constructed, 1e-6).feasible);
  }
  const x0 = constructed !== null ? constructed : initialPoint(m);
  const improved = improvePlanningAdaptive(m, x0, new StdClock(), 0.5, 5, true);
  if (improved !== null) {
    assert.ok(checkSolution(m, improved[1], 1e-6).feasible);
  }
});
