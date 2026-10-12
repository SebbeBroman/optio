// Feasible pattern search for integer scheduling blocks coupled by load rows.
// Detects constraints, not variable names; every returned point is checked.
// (Port of `crates/core/src/block_planner.rs`.)

import { Model, INF, checkSolution, fmax, fmin, isIntegral, filled } from "./model.ts";
import { VecMap } from "./vecmap.ts";
import { nextUp, nextDown, totalCmp } from "./f64bits.ts";
import type { Clock } from "./clock.ts";

/** Sparse `(index, value)` pair, mirroring Rust's `Vec<(usize, f64)>` entries. */
type Term = [number, number];
/** A map entry kept alongside its reconstructed key array. */
type MapEntry<K, V> = { key: K; val: V };

/** Exact comma-joined key of a number vector (Rust's `Vec<u64>` key, numeric). */
function numKey(a: readonly number[]): string {
  return a.join(",");
}

// f64 bit ops (`nextUp`/`nextDown`/`totalCmp`) come from `f64bits.ts`.

/** Rust `f64::round`: nearest integer, ties away from zero. */
function roundHalfAway(x: number): number {
  if (x === 0) return x;
  return x > 0 ? Math.floor(x + 0.5) : Math.ceil(x - 0.5);
}

// Rust saturating float-to-integer casts (`as u32`, `as usize`).
const U32_MAX = 4294967295;
const USIZE_MAX = Number.MAX_SAFE_INTEGER;
function toU32(v: number): number {
  if (v !== v || v <= 0) return 0;
  return v >= U32_MAX ? U32_MAX : Math.trunc(v);
}
function toUsize(v: number): number {
  if (v !== v || v <= 0) return 0;
  return v >= USIZE_MAX ? USIZE_MAX : Math.trunc(v);
}
function checkedAddU32(a: number, b: number): number | null {
  const s = a + b;
  return s > U32_MAX ? null : s;
}

/** `Option::unwrap` equivalent: these keys are structurally guaranteed. */
function req(v: number | null): number {
  if (v === null) throw new Error("block_planner: unexpected null key");
  return v;
}

function minByTotal(vals: readonly number[]): number | null {
  let best: number | null = null;
  for (const v of vals) {
    if (best === null || totalCmp(v, best) < 0) best = v;
  }
  return best;
}
function cmpNumArr(a: readonly number[], b: readonly number[]): number {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
  }
  return a.length - b.length;
}

// ---------------------------------------------------------------------------
// Detected shape.
// ---------------------------------------------------------------------------
class Group {
  session = 0;
  feeders: Term[] = [];
  cap = INF;
}
class Pattern {
  values: Term[] = [];
  loads: Term[] = [];
  cost = 0;
}
class Block {
  rows: number[];
  cols: number[];
  patterns: Pattern[];
  constructor(rows: number[], cols: number[], patterns: Pattern[]) {
    this.rows = rows;
    this.cols = cols;
    this.patterns = patterns;
  }
}
class Rule {
  target: number;
  lower: boolean;
  constant: number;
  loads: Term[];
  aux: Term[];
  constructor(target: number, lower: boolean, constant: number, loads: Term[], aux: Term[]) {
    this.target = target;
    this.lower = lower;
    this.constant = constant;
    this.loads = loads;
    this.aux = aux;
  }
}
class Aux {
  col: number;
  upperOnly: boolean;
  rules: Rule[];
  constructor(col: number, upperOnly: boolean, rules: Rule[]) {
    this.col = col;
    this.upperOnly = upperOnly;
    this.rules = rules;
  }
}
class Shape {
  groups: Group[] = [];
  sessions: number[][] = [];
  blocks: Block[] = [];
  programs: Aux[][] = [];
  peakCols: number[] = [];
  columnGroups: (number | null)[] = [];
  xrX: (number | null)[] = [];
  xZ: (number | null)[] = [];
}

/** Canonical key of a pattern (cost, then loads sorted by group) for dedup. */
function patternKey(p: Pattern): number[] {
  const key: number[] = [p.cost];
  const loads = p.loads.slice();
  loads.sort((a, b) => a[0] - b[0]);
  for (const [g, l] of loads) {
    key.push(g);
    key.push(l);
  }
  return key;
}

/** Union-find root with path compression. */
function root(parent: number[], j0: number): number {
  let j = j0;
  while (parent[j] !== j) j = parent[j];
  const r = j;
  j = j0;
  while (parent[j] !== j) {
    const nx = parent[j];
    parent[j] = r;
    j = nx;
  }
  return r;
}

/** Collect row `r`'s `(column, coefficient)` terms (Rust's `row` iterator). */
function row(model: Model, r: number): Term[] {
  const out: Term[] = [];
  for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
    out.push([model.aIndex[k], model.aValue[k]]);
  }
  return out;
}

function binSearch(arr: readonly number[], target: number): number {
  let lo = 0;
  let hi = arr.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (arr[mid] < target) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}
/** Rust `slice::binary_search(..).unwrap()`. */
function colIndex(cols: readonly number[], x: number): number {
  const i = binSearch(cols, x);
  if (cols[i] !== x) throw new Error("block_planner: column not in block");
  return i;
}

function gcd(a0: number, b0: number): number {
  let a = a0;
  let b = b0;
  while (b !== 0) {
    const r = a % b;
    a = b;
    b = r;
  }
  return a;
}

// ---------------------------------------------------------------------------
// Detection.
// ---------------------------------------------------------------------------
function detect(model: Model, clock: Clock, deadline: number, sampleLimit: number): Shape | null {
  const primal = sampleLimit !== 0;
  const n = model.numCol;
  const xZ: (number | null)[] = filled<(number | null)>(n, null);
  for (let r = 0; r < model.numRow; r++) {
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    if (ke - ks !== 2 || model.rowLower[r] > -INF / 2 || model.rowUpper[r] !== 0) {
      continue;
    }
    const x0 = model.aIndex[ks];
    const a0 = model.aValue[ks];
    const x1 = model.aIndex[ks + 1];
    const a1 = model.aValue[ks + 1];
    if (
      a0 > 0 &&
      a1 < 0 &&
      isIntegral(model.colType[x0]) &&
      isIntegral(model.colType[x1]) &&
      model.colLower[x1] >= 0 &&
      model.colUpper[x1] <= 1
    ) {
      xZ[x0] = x1;
    }
    if (
      a1 > 0 &&
      a0 < 0 &&
      isIntegral(model.colType[x1]) &&
      isIntegral(model.colType[x0]) &&
      model.colLower[x0] >= 0 &&
      model.colUpper[x0] <= 1
    ) {
      xZ[x1] = x0;
    }
  }
  const groups: Group[] = [];
  const sessions: number[][] = [];
  const peakSession = new Map<number, number>();
  const xGroup: (number | null)[] = filled<(number | null)>(n, null);
  for (let r = 0; r < model.numRow; r++) {
    if (model.rowUpper[r] !== 0 || model.rowLower[r] > -INF / 2) continue;
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    let contCount = 0;
    let contJ = -1;
    let contA = 0;
    let feedersLen = 0;
    let ok = true;
    for (let k = ks; k < ke; k++) {
      const j = model.aIndex[k];
      const a = model.aValue[k];
      if (!isIntegral(model.colType[j])) {
        contCount += 1;
        contJ = j;
        contA = a;
      } else {
        feedersLen += 1;
        if (xZ[j] === null || a <= 0 || xGroup[j] !== null) ok = false;
      }
    }
    if (contCount !== 1 || contA !== -1 || model.colCost[contJ] <= 0) continue;
    if (feedersLen === 0 || !ok) continue;
    let s = peakSession.get(contJ);
    if (s === undefined) {
      sessions.push([]);
      s = sessions.length - 1;
      peakSession.set(contJ, s);
    }
    const g = groups.length;
    const feeders: Term[] = [];
    for (let k = ks; k < ke; k++) {
      const j = model.aIndex[k];
      if (isIntegral(model.colType[j])) {
        xGroup[j] = g;
        feeders.push([j, model.aValue[k]]);
      }
    }
    sessions[s].push(g);
    const grp = new Group();
    grp.session = s;
    grp.feeders = feeders;
    grp.cap = INF;
    groups.push(grp);
  }
  if (groups.length === 0) return null;
  const duration: number[] = filled<number>(n, 0);
  for (const g of groups) {
    for (const [j, d] of g.feeders) duration[j] = d;
  }
  for (let r = 0; r < model.numRow; r++) {
    if (model.rowUpper[r] >= INF / 2 || model.rowUpper[r] <= 0 || model.rowLower[r] === model.rowUpper[r]) {
      continue;
    }
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    if (ke === ks) continue;
    const g0 = xGroup[model.aIndex[ks]];
    if (g0 !== null) {
      const want = groups[g0].feeders.length;
      let match = ke - ks === want;
      for (let k = ks; k < ke && match; k++) {
        const j = model.aIndex[k];
        if (xGroup[j] !== g0 || Math.abs(model.aValue[k] - duration[j]) >= 1e-9) match = false;
      }
      if (match) {
        groups[g0].cap = fmin(groups[g0].cap, model.rowUpper[r]);
      }
    }
  }
  // Integer equalities define independent scheduling blocks. Capacity rows
  // are inequalities and therefore do not merge otherwise independent units.
  const parent: number[] = [];
  for (let i = 0; i < n; i++) parent.push(i);
  for (let r = 0; r < model.numRow; r++) {
    if (model.rowLower[r] !== model.rowUpper[r]) continue;
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    if (ks === ke) continue;
    let allInt = true;
    for (let k = ks; k < ke; k++) {
      if (!isIntegral(model.colType[model.aIndex[k]])) {
        allInt = false;
        break;
      }
    }
    if (!allInt) continue;
    const first = model.aIndex[ks];
    for (let k = ks; k < ke; k++) {
      const a = root(parent, first);
      const b = root(parent, model.aIndex[k]);
      parent[b] = a;
    }
  }
  for (let x = 0; x < n; x++) {
    const z = xZ[x];
    if (z !== null) {
      const a = root(parent, x);
      const b = root(parent, z);
      parent[b] = a;
    }
  }
  const components = new Map<number, number[]>();
  for (let j = 0; j < n; j++) {
    if (isIntegral(model.colType[j])) {
      const p = root(parent, j);
      const arr = components.get(p);
      if (arr === undefined) components.set(p, [j]);
      else arr.push(j);
    }
  }
  const xrX: (number | null)[] = filled<(number | null)>(n, null);
  for (let r = 0; r < model.numRow; r++) {
    if (model.rowLower[r] !== 0 || model.rowUpper[r] !== 0) continue;
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    let xsCount = 0;
    let xsJ = -1;
    let ok = true;
    for (let k = ks; k < ke; k++) {
      const j = model.aIndex[k];
      const a = model.aValue[k];
      if (xGroup[j] !== null && a === 1) {
        xsCount += 1;
        xsJ = j;
      } else if (!(isIntegral(model.colType[j]) && xGroup[j] === null && a === -1)) {
        ok = false;
      }
    }
    if (xsCount === 1 && ke - ks > 1 && ok) {
      for (let k = ks; k < ke; k++) {
        const j = model.aIndex[k];
        if (j !== xsJ) xrX[j] = xsJ;
      }
    }
  }
  // Index integer-only rows once. Scanning every row for every component
  // repeats the same work for identical blocks and allocates temporary terms.
  const componentOf: number[] = new Array(n);
  for (let j = 0; j < n; j++) componentOf[j] = root(parent, j);
  const componentRows = new Map<number, number[]>();
  for (let r = 0; r < model.numRow; r++) {
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    if (ke === ks) continue;
    const first = model.aIndex[ks];
    const p = componentOf[first];
    let match = isIntegral(model.colType[first]);
    for (let k = ks; k < ke && match; k++) {
      const j = model.aIndex[k];
      if (!isIntegral(model.colType[j]) || componentOf[j] !== p) match = false;
    }
    if (match) {
      const arr = componentRows.get(p);
      if (arr === undefined) componentRows.set(p, [r]);
      else arr.push(r);
    }
  }
  const blocks: Block[] = [];
  const templates = new Map<string, Pattern[]>();
  const point: number[] = filled<number>(n, 0);
  const ordered: number[][] = [...components.values()];
  ordered.sort((c1, c2) => c1[0] - c2[0]);
  for (const cols of ordered) {
    if (!cols.some((j) => xGroup[j] !== null)) return null;
    const found = componentRows.get(componentOf[cols[0]]);
    const localRows: number[] = found === undefined ? [] : found;
    // The certificate derives all alternatives from original demand rows;
    // primal ring permutations and auxiliary projection are unnecessary.
    if (!primal) {
      blocks.push(new Block(localRows, cols, []));
      continue;
    }
    const template: number[] = [cols.length];
    for (const j of cols) {
      template.push(
        model.colCost[j],
        model.colLower[j],
        model.colUpper[j],
        xGroup[j] === null ? 0 : xGroup[j] + 1,
        xrX[j] === null ? 0 : colIndex(cols, xrX[j]) + 1,
      );
    }
    for (const r of localRows) {
      template.push(model.rowLower[r], model.rowUpper[r], model.aStart[r + 1] - model.aStart[r]);
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        template.push(colIndex(cols, model.aIndex[k]), model.aValue[k]);
      }
    }
    const tkey = numKey(template);
    const cached = templates.get(tkey);
    if (cached !== undefined) {
      blocks.push(new Block(localRows, cols, cached));
      continue;
    }
    let choices: number[] | null = null;
    for (const r of localRows) {
      if (model.rowLower[r] !== 1 || model.rowUpper[r] !== 1) continue;
      const ks = model.aStart[r];
      const ke = model.aStart[r + 1];
      let ok = true;
      for (let k = ks; k < ke && ok; k++) {
        const j = model.aIndex[k];
        if (
          !(
            model.aValue[k] === 1 &&
            xGroup[j] === null &&
            xrX[j] === null &&
            model.colLower[j] >= 0 &&
            model.colUpper[j] <= 1
          )
        ) {
          ok = false;
        }
      }
      if (ok) {
        const c: number[] = [];
        for (let k = ks; k < ke; k++) c.push(model.aIndex[k]);
        choices = c;
        break;
      }
    }
    if (choices === null) return null;
    const patterns: Pattern[] = [];
    for (const choice of choices) {
      if (model.colUpper[choice] < 1) continue;
      const demands: { vars: number[]; amount: number; sid: number }[] = [];
      const allowed = new Map<number, number[]>();
      for (const r of localRows) {
        if (model.rowLower[r] !== 0 || model.rowUpper[r] !== 0) continue;
        const ks = model.aStart[r];
        const ke = model.aStart[r + 1];
        let posCount = 0;
        let bad = false;
        for (let k = ks; k < ke; k++) {
          const j = model.aIndex[k];
          const a = model.aValue[k];
          if (a > 0) {
            posCount += 1;
            if (xrX[j] === null || a !== 1) bad = true;
          } else if (a < 0 && !choices.includes(j)) {
            bad = true;
          }
        }
        if (bad || posCount === 0) continue;
        let amount = 0;
        for (let k = ks; k < ke; k++) if (model.aIndex[k] === choice) amount += model.aValue[k];
        amount = -amount;
        if (amount <= 0) continue;
        const sid = demands.length;
        const gs: number[] = [];
        const vars: number[] = [];
        for (let k = ks; k < ke; k++) {
          const j = model.aIndex[k];
          if (model.aValue[k] > 0) {
            gs.push(req(xGroup[req(xrX[j])]));
            vars.push(j);
          }
        }
        const cur = allowed.get(sid);
        if (cur !== undefined) {
          allowed.set(sid, cur.filter((g) => gs.includes(g)));
        } else {
          allowed.set(sid, gs);
        }
        demands.push({ vars, amount, sid });
      }
      if (demands.length === 0) continue;
      const active: [number, number[]][] = [...allowed.entries()];
      active.sort((e1, e2) => e1[0] - e2[0]);
      if (active.some(([, gs]) => gs.length === 0)) continue;
      let count = 1;
      let tooBig = false;
      for (const [, gs] of active) {
        count *= gs.length;
        if (count > 100000) {
          tooBig = true;
          break;
        }
      }
      if (tooBig) return null;
      const samples = Math.min(count, sampleLimit);
      let step = Math.max(1, Math.floor(count / samples));
      while (gcd(step, count) !== 1) step += 1;
      for (let k = 0; k < samples; k++) {
        const baseIndex = (k * step) % count;
        if (patterns.length % 256 === 0 && clock.nowSecs() > deadline) {
          return null;
        }
        for (const balanced of [false, true]) {
          let index = baseIndex;
          for (const j of cols) point[j] = 0;
          point[choice] = 1;
          const selected = new Map<number, number>();
          for (const [s, gs] of active) {
            selected.set(s, gs[index % gs.length]);
            index = Math.floor(index / gs.length);
          }
          for (const { vars, amount, sid } of demands) {
            const g = req(selected.get(sid) ?? null);
            const candidates: number[] = vars.filter((j) => {
              const h = req(xGroup[req(xrX[j])]);
              if (balanced) {
                const ring = sessions[groups[g].session].indexOf(g);
                return sessions[groups[h].session].indexOf(h) === ring;
              }
              return h === g;
            });
            candidates.sort((a, b) => req(xGroup[req(xrX[a])]) - req(xGroup[req(xrX[b])]));
            const offset = candidates.findIndex((j) => xGroup[req(xrX[j])] === g);
            if (offset < 0) return null;
            const reps = toUsize(amount);
            for (let t = 0; t < reps; t++) {
              const j = candidates[(offset + t) % candidates.length];
              point[j!] += 1;
              point[req(xrX[j])!] += 1;
            }
          }
          for (const j of cols) {
            const z = xZ[j];
            if (z !== null) point[z] = point[j] > 0 ? 1 : 0;
          }
          if (
            cols.some(
              (j) => point[j] < model.colLower[j] - 1e-7 || point[j] > model.colUpper[j] + 1e-7,
            )
          ) {
            continue;
          }
          let rowBad = false;
          for (const r of localRows) {
            let v = 0;
            for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
              v += point[model.aIndex[k]] * model.aValue[k];
            }
            if (v < model.rowLower[r] - 1e-7 || v > model.rowUpper[r] + 1e-7) {
              rowBad = true;
              break;
            }
          }
          if (rowBad) continue;
          const occupiedAll: number[] = [];
          for (const j of cols) {
            if (point[j] > 0 && xGroup[j] !== null) occupiedAll.push(xGroup[j] as number);
          }
          occupiedAll.sort((a, b) => a - b);
          const occupied: number[] = [];
          for (const g of occupiedAll) {
            if (occupied.length === 0 || occupied[occupied.length - 1] !== g) occupied.push(g);
          }
          const loads: Term[] = [];
          for (const g of occupied) {
            let l = 0;
            for (const j of cols) if (xGroup[j] === g) l += point[j] * duration[j];
            if (l > 0) loads.push([g, l]);
          }
          if (loads.some(([g, l]) => l > groups[g].cap + 1e-7)) continue;
          let cost = 0;
          for (const j of cols) cost += model.colCost[j] * point[j];
          const values: Term[] = [];
          for (const j of cols) {
            if (point[j] !== 0) values.push([colIndex(cols, j), point[j]]);
          }
          const pat = new Pattern();
          pat.cost = cost;
          pat.values = values;
          pat.loads = loads;
          patterns.push(pat);
        }
      }
    }
    if (patterns.length === 0) return null;
    const decorated = patterns.map((p, i) => ({ p, k: patternKey(p), i }));
    decorated.sort((x, y) => {
      const c = cmpNumArr(x.k, y.k);
      return c !== 0 ? c : x.i - y.i;
    });
    const finalPatterns: Pattern[] = [];
    for (let i = 0; i < decorated.length; i++) {
      if (i > 0 && cmpNumArr(decorated[i].k, decorated[i - 1].k) === 0) continue;
      finalPatterns.push(decorated[i].p);
    }
    templates.set(tkey, finalPatterns);
    blocks.push(new Block(localRows, cols, finalPatterns));
  }
  const programs: Aux[][] = [];
  for (let i = 0; i < sessions.length; i++) programs.push([]);
  if (primal) {
    // Compile continuous auxiliary projection into per-session affine rules.
    // Each integer expression must be a complete scaled group load.
    const rules: Rule[] = [];
    const auxSession: (number | null)[] = filled<(number | null)>(n, null);
    for (let r = 0; r < model.numRow; r++) {
      const t = row(model, r);
      const cont = t.filter(([j]) => !isIntegral(model.colType[j]));
      if (cont.length === 0) continue;
      const coeff = new Map<number, [number, number]>();
      let sid: number | null = null;
      for (const [j, a] of t) {
        if (isIntegral(model.colType[j])) {
          const g = xGroup[j];
          if (g === null) return null;
          const v = a / duration[j];
          let e = coeff.get(g);
          if (e === undefined) {
            e = [v, 0];
            coeff.set(g, e);
          }
          if (Math.abs(e[0] - v) > 1e-8) return null;
          e[1] = e[1] + 1;
          if (sid !== null && sid !== groups[g].session) return null;
          sid = groups[g].session;
        }
      }
      for (const [g, e] of coeff) {
        if (e[1] !== groups[g].feeders.length) return null;
      }
      if (sid !== null) {
        for (const [j] of cont) {
          const cur = auxSession[j];
          if (cur !== null && cur !== sid) return null;
          auxSession[j] = sid;
        }
      }
      const loadKeys = [...coeff.keys()].sort((p, q) => p - q);
      for (const [target, a] of cont) {
        const bounds: [number, boolean][] = [
          [model.rowLower[r], true],
          [model.rowUpper[r], false],
        ];
        for (const [bound, lower] of bounds) {
          if (Math.abs(bound) >= INF / 2) continue;
          const loads: Term[] = loadKeys.map((g) => {
            const e = coeff.get(g);
            if (e === undefined) throw new Error("block_planner: missing coefficient");
            return [g, -e[0] / a] as Term;
          });
          const aux: Term[] = cont.filter(([j]) => j !== target).map(([j, b]) => [j, -b / a]);
          rules.push(new Rule(target, a > 0 ? lower : !lower, bound / a, loads, aux));
        }
      }
    }
    // Propagate session membership through continuous-only definition rows.
    for (let iter = 0; iter < 8; iter++) {
      for (const rl of rules) {
        let s = auxSession[rl.target];
        if (s === null) {
          for (const [j] of rl.aux) {
            const v = auxSession[j];
            if (v !== null) {
              s = v;
              break;
            }
          }
        }
        if (s !== null) {
          auxSession[rl.target] = s;
          for (const [j] of rl.aux) auxSession[j] = s;
        }
      }
    }
    const continuous: number[] = [];
    for (let j = 0; j < n; j++) if (!isIntegral(model.colType[j])) continuous.push(j);
    const rulesByTarget: number[][] = [];
    for (let i = 0; i < n; i++) rulesByTarget.push([]);
    for (let i = 0; i < rules.length; i++) rulesByTarget[rules[i].target].push(i);
    const ready: boolean[] = filled<boolean>(n, false);
    for (let iter = 0; iter < continuous.length; iter++) {
      let progress = false;
      for (const j of continuous) {
        if (ready[j]) continue;
        const usable: number[] = rulesByTarget[j].filter((i) =>
          rules[i].aux.every(([c]) => ready[c]),
        );
        if (usable.length === 0) continue;
        const sj = auxSession[j];
        if (sj === null) return null;
        const upperOnly = usable.every((i) => !rules[i].lower);
        const own: Rule[] = usable.map((i) => {
          const rl = rules[i];
          const made = new Rule(j, rl.lower, rl.constant, rl.loads, rl.aux);
          rl.loads = [];
          rl.aux = [];
          return made;
        });
        programs[sj].push(new Aux(j, upperOnly, own));
        ready[j] = true;
        progress = true;
      }
      if (!progress) break;
    }
    for (let j = 0; j < n; j++) {
      if (!isIntegral(model.colType[j]) && !ready[j]) return null;
    }
  }
  const columnGroups: (number | null)[] = xGroup.slice();
  for (let j = 0; j < n; j++) {
    const g = xGroup[j];
    if (g !== null) {
      const z = xZ[j];
      if (z !== null) columnGroups[z] = g;
    }
    const x = xrX[j];
    if (x !== null) columnGroups[j] = xGroup[x];
  }
  const peakCols: number[] = filled<number>(sessions.length, 0);
  for (const [col, session] of peakSession) peakCols[session] = col;
  const sh = new Shape();
  sh.groups = groups;
  sh.sessions = sessions;
  sh.blocks = blocks;
  sh.programs = programs;
  sh.peakCols = peakCols;
  sh.columnGroups = columnGroups;
  sh.xrX = xrX;
  sh.xZ = xZ;
  return sh;
}

// ---------------------------------------------------------------------------
// Session auxiliary projection cost.
// ---------------------------------------------------------------------------
function sessionCost(
  model: Model,
  sh: Shape,
  s: number,
  loads: readonly number[],
  aux: number[],
  respectBox: boolean,
): number {
  let cost = 0;
  for (const a of sh.programs[s]) {
    let lo = respectBox ? model.colLower[a.col] : 0;
    let hi = respectBox ? model.colUpper[a.col] : INF;
    for (const rl of a.rules) {
      let v = rl.constant;
      for (const [g, c] of rl.loads) v += c * loads[g];
      for (const [j, c] of rl.aux) v += c * aux[j];
      if (rl.lower) lo = fmax(lo, v);
      else hi = fmin(hi, v);
    }
    if (lo > hi + 1e-6) return Infinity;
    aux[a.col] = a.upperOnly ? hi : lo;
    cost += aux[a.col] * model.colCost[a.col];
  }
  return cost;
}

// Repack independent round/day demands into balanced session capacities.
// Each demand uses at most one ring per session. The original checker remains
// authoritative for constraints outside this detected scheduling shape.
function rebalance(model: Model, sh: Shape, point: number[], loads: number[]): boolean {
  const days = new Map<string, { sessions: number[]; demands: [number, number[]][] }>();
  for (const block of sh.blocks) {
    for (const r of block.rows) {
      const terms = row(model, r);
      if (model.rowLower[r] !== 0 || model.rowUpper[r] !== 0) continue;
      const vars: number[] = [];
      for (const [j, a] of terms) {
        if (a === 1 && sh.xrX[j] !== null) vars.push(j);
      }
      if (vars.length === 0 || terms.some(([j, a]) => a > 0 && sh.xrX[j] === null)) {
        continue;
      }
      let amount = 0;
      for (const j of vars) amount += point[j];
      if (amount <= 0) continue;
      const sessions: number[] = vars.map(
        (j) => sh.groups[req(sh.columnGroups[j])].session,
      );
      sessions.sort((a, b) => a - b);
      const dedup: number[] = [];
      for (const s of sessions) {
        if (dedup.length === 0 || dedup[dedup.length - 1] !== s) dedup.push(s);
      }
      const key = dedup.join(",");
      const entry = days.get(key);
      if (entry === undefined) days.set(key, { sessions: dedup, demands: [[amount, vars]] });
      else entry.demands.push([amount, vars]);
    }
  }
  if (days.size === 0) return false;
  const entries = [...days.values()];
  entries.sort((a, b) => cmpNumArr(a.sessions, b.sessions));
  for (const entry of entries) {
    const groups: number[] = [];
    for (const s of entry.sessions) for (const g of sh.sessions[s]) groups.push(g);
    const demands = entry.demands;
    let total = 0;
    for (const d of demands) total += d[0];
    const base = Math.floor(total / groups.length);
    const extra = toUsize(roundHalfAway(total - base * groups.length));
    const room: number[] = filled<number>(sh.groups.length, 0);
    for (let k = 0; k < groups.length; k++) {
      room[groups[k]] = base + (k < extra ? 1 : 0);
    }
    demands.sort((a, b) => totalCmp(b[0], a[0]));
    for (const [amount, vars] of demands) {
      for (const j of vars) point[j] = 0;
      // Consume bin residues before whole chunks leave small gaps in
      // every ring. A round may split across the two sessions.
      let residual: [number, number, number] | null = null;
      for (const a of vars) {
        const ga = req(sh.columnGroups[a]);
        const first = room[ga] % amount;
        if (first <= 0) continue;
        let found = false;
        for (const b of vars) {
          const gb = req(sh.columnGroups[b]);
          if (sh.groups[ga].session !== sh.groups[gb].session && room[gb] >= amount - first) {
            residual = [a, b, first];
            found = true;
            break;
          }
        }
        if (found) break;
      }
      if (residual !== null) {
        const [a, b, first] = residual;
        point[a] = first;
        point[b] = amount - first;
        room[req(sh.columnGroups[a])!] -= first;
        room[req(sh.columnGroups[b])!] -= amount - first;
        continue;
      }
      let best: number | null = null;
      for (const j of vars) {
        if (room[req(sh.columnGroups[j])] >= amount) {
          if (
            best === null ||
            totalCmp(room[req(sh.columnGroups[j])], room[req(sh.columnGroups[best])]) < 0
          ) {
            best = j;
          }
        }
      }
      if (best !== null) {
        point[best] = amount;
        room[req(sh.columnGroups[best])!] -= amount;
        continue;
      }
      let pair: [number, number] | null = null;
      for (const a of vars) {
        const ga = req(sh.columnGroups[a]);
        let found = false;
        for (const b of vars) {
          const gb = req(sh.columnGroups[b]);
          if (
            sh.groups[ga].session !== sh.groups[gb].session &&
            room[ga] > 0 &&
            room[gb] > 0 &&
            room[ga] + room[gb] >= amount
          ) {
            pair = [a, b];
            found = true;
            break;
          }
        }
        if (found) break;
      }
      if (pair === null) return false;
      const [a, b] = pair;
      const ga = req(sh.columnGroups[a]);
      const gb = req(sh.columnGroups[b]);
      const first = fmin(amount, room[ga]);
      point[a] = first;
      point[b] = amount - first;
      room[ga!] -= first;
      room[gb!] -= amount - first;
    }
  }
  loads.fill(0);
  for (const g of sh.groups) {
    for (const [j] of g.feeders) point[j] = 0;
  }
  for (let j = 0; j < point.length; j++) {
    const x = sh.xrX[j];
    if (x !== null) point[x!] += point[j];
  }
  for (let g = 0; g < sh.groups.length; g++) {
    for (const [j, d] of sh.groups[g].feeders) {
      const z = sh.xZ[j];
      if (z === null) return false;
      point[z] = point[j] > 0 ? 1 : 0;
      loads[g] += point[j] * d;
    }
  }
  return true;
}

/// A dual row-combination certificate. Multipliers are heuristic; the final
/// bound uses outward-rounded interval arithmetic over every original column.
/// It remains valid even when pattern detection or symmetry assumptions fail.
export function rowBound(model: Model): number | null {
  const reduced = model.colCost.slice();
  const weights: number[] = filled<number>(model.numRow, 0);
  const peaks = new Map<number, number[]>();
  for (let r = 0; r < model.numRow; r++) {
    if (Math.abs(model.rowUpper[r]) >= INF / 2) continue;
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    let contCount = 0;
    let contJ = -1;
    let contA = 0;
    let ok = true;
    for (let k = ks; k < ke; k++) {
      const j = model.aIndex[k];
      const a = model.aValue[k];
      if (!isIntegral(model.colType[j])) {
        contCount += 1;
        contJ = j;
        contA = a;
      } else if (!(a > 0)) {
        ok = false;
      }
    }
    if (contCount === 1 && contA === -1 && model.colCost[contJ] > 0 && ok) {
      const arr = peaks.get(contJ);
      if (arr === undefined) peaks.set(contJ, [r]);
      else arr.push(r);
    }
  }
  const peakKeys = [...peaks.keys()].sort((a, b) => a - b);
  for (const j of peakKeys) {
    const rows = peaks.get(j) as number[];
    const w = model.colCost[j] / rows.length;
    for (const r of rows) {
      weights[r!] += w;
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        reduced[model.aIndex[k]!] += w * model.aValue[k];
      }
    }
  }
  for (let r = 0; r < model.numRow; r++) {
    if (model.rowLower[r] !== model.rowUpper[r] || Math.abs(model.rowLower[r]) >= INF / 2) {
      continue;
    }
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    if (ke === ks) continue;
    let ok = true;
    for (let k = ks; k < ke; k++) {
      if (!isIntegral(model.colType[model.aIndex[k]]) || model.aValue[k] <= 0) {
        ok = false;
        break;
      }
    }
    if (!ok) continue;
    let shift: number | null = null;
    for (let k = ks; k < ke; k++) {
      const v = reduced[model.aIndex[k]] / model.aValue[k];
      if (shift === null || totalCmp(v, shift) < 0) shift = v;
    }
    if (shift === null) return null;
    weights[r] -= shift;
    for (let k = ks; k < ke; k++) {
      reduced[model.aIndex[k]!] -= shift * model.aValue[k];
    }
  }
  const lo = model.colCost.slice();
  const hi = model.colCost.slice();
  let bound = 0;
  for (let r = 0; r < weights.length; r++) {
    const w = weights[r];
    if (w === 0) continue;
    const rhs = w > 0 ? model.rowUpper[r] : model.rowLower[r];
    if (Math.abs(rhs) >= INF / 2 || !Number.isFinite(w)) return null;
    if (rhs !== 0) bound = nextDown(bound + nextDown(-w * rhs));
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const j = model.aIndex[k];
      const a = model.aValue[k];
      if (a === 0) continue;
      const v = w * a;
      lo[j] = nextDown(lo[j] + nextDown(v));
      hi[j] = nextUp(hi[j] + nextUp(v));
    }
  }
  for (let j = 0; j < model.numCol; j++) {
    if (lo[j] === 0 && hi[j] === 0) continue;
    const l = model.colLower[j];
    const u = model.colUpper[j];
    if ((l <= -INF / 2 && hi[j] > 0) || (u >= INF / 2 && lo[j] < 0)) return null;
    const term = minByTotal([lo[j] * l, lo[j] * u, hi[j] * l, hi[j] * u]);
    if (term === null) return null;
    bound = nextDown(bound + nextDown(term));
  }
  return Number.isFinite(bound) ? bound : null;
}

/// Integer peak certificate for models whose one-hot choices fix each
/// session's total match count. All options come from original equalities;
/// other constraints are relaxed, never used to exclude certificate states.
export function schedulingBound(model: Model, clock: Clock, deadline: number): number | null {
  if (model.numCol > 2000) return null;
  const seen: number[] = filled<number>(model.numCol, 0);
  for (let r = 0; r < model.numRow; r++) {
    for (const [j] of row(model, r)) {
      if (seen[j] === r + 1) return null;
      seen[j] = r + 1;
    }
  }
  const sh = detect(model, clock, deadline, 0);
  if (sh === null) return null;
  if (sh.blocks.length > 40 || sh.sessions.length > 8) return null;
  const unit: number[] = filled<number>(8, 0);
  for (let session = 0; session < sh.sessions.length; session++) {
    const groups = sh.sessions[session];
    unit[session] = sh.groups[groups[0]].feeders[0][1];
    for (const g of groups) {
      for (const [, d] of sh.groups[g].feeders) {
        if (d !== unit[session]) return null;
      }
    }
  }
  const choiceCols = new Set<number>();
  const options: VecMap<number>[] = [];
  for (const block of sh.blocks) {
    let choices: number[] | null = null;
    for (const r of block.rows) {
      const t = row(model, r);
      if (
        model.rowLower[r] === 1 &&
        model.rowUpper[r] === 1 &&
        t.length > 0 &&
        t.every(
          ([j, a]) =>
            a === 1 &&
            sh.columnGroups[j] === null &&
            isIntegral(model.colType[j]) &&
            model.colLower[j] >= 0 &&
            model.colUpper[j] <= 1,
        )
      ) {
        choices = t.map(([j]) => j);
        break;
      }
    }
    if (choices === null) return null;
    for (const c of choices) choiceCols.add(c);
    const expected = new Set<number>();
    for (const group of sh.groups) {
      for (const [x] of group.feeders) {
        if (!block.cols.includes(x)) continue;
        if (!isIntegral(model.colType[x]) || model.colLower[x] < 0) return null;
        const xr: number[] = block.cols.filter((j) => sh.xrX[j] === x);
        if (
          xr.length === 0 ||
          !block.rows.some((r) => {
            const t = row(model, r);
            return (
              model.rowLower[r] === 0 &&
              model.rowUpper[r] === 0 &&
              t.length === xr.length + 1 &&
              t.every(([j, a]) => (j === x && a === 1) || (xr.includes(j) && a === -1))
            );
          })
        ) {
          return null;
        }
        for (const v of xr) expected.add(v);
      }
    }
    const demandRows: [number, number][] = [];
    const covered = new Set<number>();
    for (const r of block.rows) {
      if (model.rowLower[r] !== 0 || model.rowUpper[r] !== 0) continue;
      const t = row(model, r);
      const vars: number[] = [];
      for (const [j, a] of t) if (a > 0) vars.push(j);
      if (
        vars.length === 0 ||
        t.some(([j, a]) => {
          if (a > 0) {
            return a !== 1 || !expected.has(j) || model.colLower[j] < 0;
          }
          return a !== 0 && (!choices.includes(j) || a - Math.trunc(a) !== 0);
        })
      ) {
        continue;
      }
      const cg = sh.columnGroups[vars[0]];
      if (cg === null) return null;
      const session = sh.groups[cg].session;
      for (const j of vars) {
        if (sh.groups[req(sh.columnGroups[j])].session !== session) return null;
      }
      for (const j of vars) {
        if (covered.has(j)) return null;
        covered.add(j);
      }
      demandRows.push([r, session]);
    }
    if (covered.size !== expected.size) return null;
    for (const v of covered) if (!expected.has(v)) return null;
    const alternatives = new VecMap<number>();
    for (const choice of choices) {
      if (
        model.colUpper[choice] < 1 ||
        choices.some((j) => j !== choice && model.colLower[j] > 0)
      ) {
        continue;
      }
      const profile: number[] = filled<number>(8, 0);
      for (const [r, session] of demandRows) {
        let amount = 0;
        for (const [j, a] of row(model, r)) if (j === choice) amount += a;
        amount = -amount;
        if (amount < 0 || amount - Math.trunc(amount) !== 0 || amount > U32_MAX) return null;
        const add = checkedAddU32(profile[session], toU32(amount));
        if (add === null) return null;
        profile[session] = add;
      }
      const cost = model.colCost[choice];
      const id = alternatives.find(profile);
      if (id < 0) alternatives.insert(profile, cost);
      else if (cost < alternatives.val(id)) alternatives.setVal(id, cost);
    }
    if (alternatives.size() === 0) return null;
    options.push(alternatives);
  }
  let states = new VecMap<number>();
  const zeroKey: number[] = filled<number>(8, 0);
  states.insert(zeroKey, 0);
  for (const alternatives of options) {
    const next = new VecMap<number>();
    const nStates = states.size();
    const nAlts = alternatives.size();
    for (let s = 0; s < nStates; s++) {
      const key = states.key(s);
      const base = states.val(s);
      for (let a = 0; a < nAlts; a++) {
        const profile = alternatives.key(a);
        const total: number[] = filled<number>(8, 0);
        let overflow = false;
        for (let k = 0; k < 8; k++) {
          const add = checkedAddU32(key[k], profile[k]);
          if (add === null) {
            overflow = true;
            break;
          }
          total[k] = add;
        }
        if (overflow) continue;
        const value = nextDown(base + alternatives.val(a));
        const id = next.find(total);
        if (id < 0) next.insert(total, value);
        else if (value < next.val(id)) next.setVal(id, value);
      }
    }
    if (next.size() > 50000 || clock.nowSecs() >= deadline) return null;
    states = next;
  }
  let constant = 0;
  for (let j = 0; j < model.numCol; j++) {
    if (choiceCols.has(j) || sh.peakCols.includes(j) || model.colCost[j] === 0) continue;
    const cost = model.colCost[j];
    const x = cost >= 0 ? model.colLower[j] : model.colUpper[j];
    if (Math.abs(x) >= INF / 2) return null;
    constant = nextDown(constant + nextDown(cost * x));
  }
  let best: number | null = null;
  for (let s = 0; s < states.size(); s++) {
    const key = states.key(s);
    let bound = nextDown(constant + states.val(s));
    for (let session = 0; session < sh.sessions.length; session++) {
      const groups = sh.sessions[session];
      const count = toUsize(key[session]);
      const peak = Math.floor(count / groups.length) + (count % groups.length !== 0 ? 1 : 0);
      const term = nextDown(nextDown(peak * unit[session]) * model.colCost[sh.peakCols[session]]);
      bound = nextDown(bound + term);
    }
    if (best === null || totalCmp(bound, best) < 0) best = bound;
  }
  return best;
}

function packedCandidate(model: Model, sh: Shape, raw: readonly number[]): [number[], number[]] | null {
  const point = raw.slice();
  const loads: number[] = filled<number>(sh.groups.length, 0);
  if (!rebalance(model, sh, point, loads)) return null;
  for (let session = 0; session < sh.sessions.length; session++) {
    sessionCost(model, sh, session, loads, point, true);
  }
  return checkSolution(model, point, 1e-9).feasible ? [point, loads] : null;
}

function dayDp(model: Model, sh: Shape, clock: Clock, deadline: number): number[] | null {
  if (sh.blocks.length > 40 || sh.sessions.length > 8) return null;
  let states = new VecMap<{ cost: number; path: number[] }>();
  const zeroKey: number[] = filled<number>(8, 0);
  states.insert(zeroKey, { cost: 0, path: filled<number>(40, 0) });
  for (let blockIndex = 0; blockIndex < sh.blocks.length; blockIndex++) {
    const block = sh.blocks[blockIndex];
    const alternatives = new VecMap<[number, number]>();
    for (let i = 0; i < block.patterns.length; i++) {
      const p = block.patterns[i];
      const totals: number[] = filled<number>(8, 0);
      for (const [g, l] of p.loads) totals[sh.groups[g].session] += l;
      const id = alternatives.find(totals);
      if (id < 0) alternatives.insert(totals, [p.cost, i]);
      else if (p.cost < alternatives.val(id)[0]) alternatives.setVal(id, [p.cost, i]);
    }
    const next = new VecMap<{ cost: number; path: number[] }>();
    const nStates = states.size();
    const nAlts = alternatives.size();
    for (let s = 0; s < nStates; s++) {
      const key = states.key(s);
      const stv = states.val(s);
      for (let a = 0; a < nAlts; a++) {
        const profile = alternatives.key(a);
        const altv = alternatives.val(a);
        const total: number[] = filled<number>(8, 0);
        for (let k = 0; k < 8; k++) {
          total[k] = key[k] + profile[k];
        }
        const value = stv.cost + altv[0];
        const i = altv[1];
        const id = next.find(total);
        if (id < 0) {
          if (i > 65535) return null;
          const choices = stv.path.slice();
          choices[blockIndex] = i;
          next.insert(total, { cost: value, path: choices });
        } else if (value < next.val(id).cost) {
          if (i > 65535) return null;
          const choices = stv.path.slice();
          choices[blockIndex] = i;
          next.setVal(id, { cost: value, path: choices });
        }
      }
    }
    if (next.size() > 50000 || clock.nowSecs() >= deadline) return null;
    states = next;
  }
  const allDurations: number[] = [];
  for (const g of sh.groups) for (const [, d] of g.feeders) allDurations.push(d);
  const unit = minByTotal(allDurations);
  if (unit === null) return null;
  for (const g of sh.groups) {
    for (const [, d] of g.feeders) {
      const ratio = d / unit;
      if (Math.abs(ratio - roundHalfAway(ratio)) > 1e-9) return null;
    }
  }
  const scored: { value: number; path: number[] }[] = [];
  const aux: number[] = filled<number>(model.numCol, 0);
  const loads: number[] = filled<number>(sh.groups.length, 0);
  for (let s = 0; s < states.size(); s++) {
    const key = states.key(s);
    const stv = states.val(s);
    for (let session = 0; session < sh.sessions.length; session++) {
      const groups = sh.sessions[session];
      const count = toUsize(roundHalfAway(key[session] / unit));
      for (let k = 0; k < groups.length; k++) {
        loads[groups[k]] =
          (Math.floor(count / groups.length) + (k < count % groups.length ? 1 : 0)) * unit;
      }
    }
    let value = stv.cost;
    for (let session = 0; session < sh.sessions.length; session++) {
      value += sessionCost(model, sh, session, loads, aux, true);
    }
    if (Number.isFinite(value)) scored.push({ value, path: stv.path });
  }
  scored.sort((a, b) => totalCmp(a.value, b.value));
  let best: number[] | null = null;
  const limit = Math.min(64, scored.length);
  for (let s = 0; s < limit; s++) {
    const path = scored[s].path;
    const raw: number[] = filled<number>(model.numCol, 0);
    for (let b = 0; b < sh.blocks.length; b++) {
      const block = sh.blocks[b];
      const i = path[b];
      for (const [j, v] of block.patterns[i].values) raw[block.cols[j]] = v;
    }
    const cand = packedCandidate(model, sh, raw);
    if (cand !== null) {
      if (best === null || model.objective(cand[0]) < model.objective(best)) best = cand[0];
    }
    if (clock.nowSecs() >= deadline) break;
  }
  return best;
}

export function improve(
  model: Model,
  start: number[] | null,
  clock: Clock,
  budget: number,
): number[] | null {
  const deadline = clock.nowSecs() + budget;
  // Try one representative ring assignment per day/round choice first on
  // larger models. Packing removes ring-label permutations. Accept this
  // shortcut only when the original-row certificate proves its optimum;
  // otherwise retain the full sampled-pattern search.
  if (model.numCol > 2000) {
    const point = improvePatterns(model, start, clock, deadline, 1);
    if (point !== null) {
      const bound = rowBound(model);
      if (bound !== null && model.objective(point) - bound <= 1e-6) {
        return point;
      }
    }
  }
  return improvePatterns(model, start, clock, deadline, 512);
}

function improvePatterns(
  model: Model,
  start: number[] | null,
  clock: Clock,
  deadline: number,
  sampleLimit: number,
): number[] | null {
  const sh = detect(model, clock, deadline, sampleLimit);
  if (sh === null) return null;
  const raw: number[] = filled<number>(model.numCol, 0);
  for (const block of sh.blocks) {
    let pattern: Pattern | null = null;
    for (const p of block.patterns) {
      if (pattern === null || totalCmp(p.cost, pattern.cost) < 0) pattern = p;
    }
    if (pattern === null) return null;
    for (const [j, v] of pattern.values) raw[block.cols[j]] = v;
  }
  let best: number[] | null = null;
  const cand = packedCandidate(model, sh, raw);
  if (cand !== null) best = cand[0];
  if (best !== null) {
    const bound = rowBound(model);
    if (bound !== null && model.objective(best) - bound <= 1e-6) {
      return start === null || model.objective(best) <= model.objective(start) + 1e-9
        ? best.slice()
        : null;
    }
  }
  const dpPoint = dayDp(model, sh, clock, deadline);
  if (dpPoint !== null) {
    if (best === null || model.objective(dpPoint) < model.objective(best)) best = dpPoint;
  }
  if (best === null) return null;
  return start === null || model.objective(best) <= model.objective(start) + 1e-9 ? best : null;
}
