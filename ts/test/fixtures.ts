// Shared test fixtures mirroring the Rust regression models.

import { Model, INF, VAR_INTEGER, VAR_BINARY } from "../src/model.ts";

/**
 * The exact app-shaped model from `crates/core/tests/planning_app_model.rs`
 * (placement + continuous auxiliaries). `total` is the `tot` row RHS; 7 is
 * feasible, 8 is genuinely infeasible.
 */
export function appShapedModel(total: number): Model {
  const m = new Model();
  const y = m.addCol(0, 1);
  const x1 = m.addCol(0, 4);
  const xrQf = m.addCol(0, 4);
  const x2 = m.addCol(0, 3);
  const xrFinal = m.addCol(0, 1);
  const xrSemi = m.addCol(0, 2);
  const peak1 = m.addCol(0, 1e9);
  const min1 = m.addCol(0, 1e9);
  const range1 = m.addCol(0, 1e9);
  const peak2 = m.addCol(0, 1e9);
  const min2 = m.addCol(0, 1e9);
  const range2 = m.addCol(0, 1e9);
  for (const c of [y, x1, xrQf, x2, xrFinal, xrSemi]) m.setIntegrality(c, VAR_INTEGER);

  m.setCost(y, -0.1);
  m.setCost(peak1, 0.05);
  m.setCost(range1, 1.0);
  m.setCost(peak2, 0.05);
  m.setCost(range2, 1.0);

  m.addRow(1, 1, [[y, 1]]);
  m.addRow(0, 0, [
    [x1, 1],
    [xrQf, -1],
  ]);
  m.addRow(0, 0, [
    [x2, 1],
    [xrFinal, -1],
    [xrSemi, -1],
  ]);
  m.addRow(total, total, [
    [x1, 1],
    [x2, 1],
  ]);
  m.addRow(0, 0, [
    [x1, 1],
    [y, -4],
  ]);
  m.addRow(0, 0, [
    [xrQf, 1],
    [y, -4],
  ]);
  m.addRow(0, 0, [
    [x2, 1],
    [y, -3],
  ]);
  m.addRow(0, 0, [
    [xrFinal, 1],
    [y, -1],
  ]);
  m.addRow(0, 0, [
    [xrSemi, 1],
    [y, -2],
  ]);
  m.addRow(0, INF, [
    [range1, 1],
    [peak1, -1],
    [min1, 1],
  ]);
  m.addRow(-INF, 0, [
    [x1, 720],
    [peak1, -1],
  ]);
  m.addRow(0, INF, [
    [x1, 720],
    [min1, -1],
  ]);
  m.addRow(0, INF, [
    [range2, 1],
    [peak2, -1],
    [min2, 1],
  ]);
  m.addRow(-INF, 0, [
    [x2, 720],
    [peak2, -1],
  ]);
  m.addRow(0, INF, [
    [x2, 720],
    [min2, -1],
  ]);
  return m;
}

/**
 * Planning-shaped model mirroring the `planning_like` helper in
 * `crates/core/src/heuristic.rs` tests: integer x[u,p,r], binary z[u,p,r],
 * per-(p,r) load = sum dur*x, cap, and per-session peak/min/range/deviation.
 */
export function planningLike(units: number, sessions: number, rings: number): Model {
  const m = new Model();
  const rng = (i: number, mul: number) => ((i * mul) % 100) / 100;
  const matches: number[] = [];
  const dur: number[] = [];
  for (let u = 0; u < units; u++) {
    matches.push(Math.round(1.0 + 4.0 * rng(u, 37)));
    dur.push(Math.round(600.0 + 600.0 * rng(u, 53)));
  }

  const x: number[][][] = [];
  const z: number[][][] = [];
  for (let u = 0; u < units; u++) {
    x.push([]);
    z.push([]);
    for (let p = 0; p < sessions; p++) {
      x[u].push([]);
      z[u].push([]);
      for (let r = 0; r < rings; r++) {
        const xi = m.addCol(0, matches[u]);
        m.setIntegrality(xi, VAR_INTEGER);
        const zi = m.addCol(0, 1);
        m.setIntegrality(zi, VAR_BINARY);
        x[u][p].push(xi);
        z[u][p].push(zi);
      }
    }
  }

  const wload: number[][] = [];
  for (let p = 0; p < sessions; p++) wload.push(new Array(rings).fill(0));
  for (let u = 0; u < units; u++) {
    wload[u % sessions][u % rings] += matches[u] * dur[u];
  }
  let cap = 0;
  for (let p = 0; p < sessions; p++) {
    for (let r = 0; r < rings; r++) cap = Math.max(cap, wload[p][r]);
  }
  cap = Math.ceil(cap * 1.5);

  for (let u = 0; u < units; u++) {
    const tot: [number, number][] = [];
    for (let p = 0; p < sessions; p++) {
      for (let r = 0; r < rings; r++) tot.push([x[u][p][r], 1.0]);
    }
    m.addRow(matches[u], matches[u], tot);
    for (let p = 0; p < sessions; p++) {
      const zt: [number, number][] = [];
      for (let r = 0; r < rings; r++) {
        m.addRow(-INF, 0, [
          [x[u][p][r], 1.0],
          [z[u][p][r], -matches[u]],
        ]);
        zt.push([z[u][p][r], 1.0]);
      }
      m.addRow(-INF, 1, zt);
    }
  }

  const load: number[][] = [];
  for (let p = 0; p < sessions; p++) {
    load.push([]);
    for (let r = 0; r < rings; r++) {
      const l = m.addCol(0, INF);
      load[p].push(l);
      const terms: [number, number][] = [[l, 1.0]];
      for (let u = 0; u < units; u++) terms.push([x[u][p][r], -dur[u]]);
      m.addRow(0, 0, terms);
      m.addRow(-INF, cap, [[l, 1.0]]);
    }
    const peak = m.addCol(0, INF);
    const minv = m.addCol(0, INF);
    const total = m.addCol(0, INF);
    const range = m.addCol(0, INF);
    const totDef: [number, number][] = [[total, 1.0]];
    for (let r = 0; r < rings; r++) {
      totDef.push([load[p][r], -1.0]);
      m.addRow(-INF, 0, [
        [load[p][r], 1.0],
        [peak, -1.0],
      ]);
      m.addRow(0, INF, [
        [load[p][r], 1.0],
        [minv, -1.0],
      ]);
      const dev = m.addCol(0, INF);
      const hiRow: [number, number][] = [
        [dev, -1.0],
        [total, -1.0],
      ];
      const loRow: [number, number][] = [
        [dev, -1.0],
        [total, 1.0],
      ];
      for (let rr = 0; rr < rings; rr++) {
        hiRow.push([load[p][rr], rings]);
        loRow.push([load[p][rr], -rings]);
      }
      m.addRow(-INF, 0, hiRow);
      m.addRow(-INF, 0, loRow);
    }
    m.addRow(0, 0, totDef);
    m.addRow(0, INF, [
      [range, 1.0],
      [peak, -1.0],
      [minv, 1.0],
    ]);
  }
  return m;
}
