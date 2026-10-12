// Structure-aware repair / local search for coupled planning MILPs.
// (Port of `crates/core/src/local_search.rs`.)
//
// The generic min-conflicts heuristic moves a single variable at a time. On the
// planning model that stalls on the coupled "choose one ring per
// unit/session" (SOS1-like) `onesplit` rows and on the per-(session,ring) load
// coupling: satisfying the last rows needs a *transfer* (move a whole match from
// one ring to another, updating the linked `z` and the session load/peak/
// deviation auxiliaries together), not a single-variable move.
//
// This module is the primal first-aid that runs before/with the LP relaxation:
// it takes an almost-feasible point and tries to close the remaining violation
// with transfer + swap moves. Same algorithms, branch structure, tolerances and
// move-selection order as the Rust source.

import {
  Model,
  INF,
  VAR_BINARY,
  isIntegral,
  checkSolution,
  fmax,
  fmin,
} from "./model.ts";
import type { Clock } from "./clock.ts";
import { improve } from "./block_planner.ts";

const TOL = 1e-6;
const INF_HALF = INF * 0.5;

/** A `(column, delta)` change, mirroring Rust `(usize, f64)`. */
type Change = [number, number];
/** An index pair, mirroring Rust `(usize, usize)`. */
type Pair = [number, number];

/** Rust `f64::clamp`: NaN passes through. */
function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v;
}

/** Rust `v.is_infinite()`. */
function isInf(v: number): boolean {
  return v === Infinity || v === -Infinity;
}

/** A column that behaves like a binary (integral with bounds within [0,1]). */
function isBinaryLike(model: Model, c: number): boolean {
  return (
    isIntegral(model.colType[c]) &&
    model.colLower[c] >= -1e-9 &&
    model.colUpper[c] <= 1.0 + 1e-9
  );
}

/** Constraint matrix in CSC (columns) form. */
class Csc {
  start: number[] = [];
  index: number[] = [];
  value: number[] = [];

  static build(model: Model): Csc {
    const n = model.numCol;
    const start = new Array<number>(n + 1).fill(0);
    for (let k = 0; k < model.aIndex.length; k++) {
      start[model.aIndex[k] + 1] += 1;
    }
    for (let i = 0; i < n; i++) {
      start[i + 1] += start[i];
    }
    const nnz = model.aValue.length;
    const index = new Array<number>(nnz).fill(0);
    const value = new Array<number>(nnz).fill(0.0);
    const cursor = start.slice();
    for (let r = 0; r < model.numRow; r++) {
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const c = model.aIndex[k];
        const p = cursor[c];
        index[p] = r;
        value[p] = model.aValue[k];
        cursor[c!] += 1;
      }
    }
    const out = new Csc();
    out.start = start;
    out.index = index;
    out.value = value;
    return out;
  }

}

// Small seeded xorshift so the whole pass is deterministic for a fixed seed.
// JS numbers cannot hold u64 exactly, so the state is a 64-bit BigInt masked
// after every wrapping operation; the sequence is byte-for-byte the Rust one.
const M64 = (1n << 64n) - 1n;
const SEED_MUL = 0x9e3779b97f4a7c15n;
const SEED_ADD = 0x123456789abcdef0n;
const SEED_FALLBACK = 0xdeadbeefcafe1234n;
const TWO53 = 9007199254740992; // 1 << 53

class Rng {
  private s: bigint;

  constructor(seed: number) {
    const seedBig = BigInt.asUintN(64, BigInt(Math.trunc(seed)));
    let s = (seedBig * SEED_MUL) & M64;
    s = (s + SEED_ADD) & M64;
    this.s = s === 0n ? SEED_FALLBACK : s;
  }

  nextU64(): bigint {
    let x = this.s;
    x = (x ^ (x << 13n)) & M64;
    x = (x ^ (x >> 7n)) & M64;
    x = (x ^ (x << 17n)) & M64;
    this.s = x;
    return x;
  }

  nextF64(): number {
    return Number(this.nextU64() >> 11n) / TWO53;
  }

  below(n: number): number {
    if (n === 0) return 0;
    return Number(this.nextU64() % BigInt(n));
  }
}

function rowViol(model: Model, r: number, lhs: number): number {
  return fmax(fmax(model.rowLower[r] - lhs, lhs - model.rowUpper[r]), 0.0);
}

/** Detected planning structure. `-1` is the "absent" sentinel. */
interface Struct {
  xUnit: number[];
  xLoad: number[];
  xDur: number[];
  xZ: number[];
  units: number[][];
  unitSum: number[];
  loadSession: number[];
  sessionLoads: number[][];
  total: number[];
  peak: number[];
  minv: number[];
  range: number[];
  cap: number[];
  devRows: Pair[];
  devRowsOfCol: number[][];
  devOfSession: number[][];
  feeders: number[][];
  detected: boolean;
}

function detect(model: Model, csc: Csc): Struct {
  const n = model.numCol;
  const m = model.numRow;
  const st: Struct = {
    xUnit: new Array<number>(n).fill(-1),
    xLoad: new Array<number>(n).fill(-1),
    xDur: new Array<number>(n).fill(0.0),
    xZ: new Array<number>(n).fill(-1),
    units: [],
    unitSum: [],
    loadSession: new Array<number>(n).fill(-1),
    sessionLoads: [],
    total: new Array<number>(n).fill(-1),
    peak: new Array<number>(n).fill(-1),
    minv: new Array<number>(n).fill(-1),
    range: new Array<number>(n).fill(-1),
    cap: new Array<number>(n).fill(INF),
    devRows: [],
    devRowsOfCol: [],
    devOfSession: [],
    feeders: Array.from({ length: n }, () => [] as number[]),
    detected: false,
  };

  // 1. `tot` rows: equality, all columns integral, all coefficients +1.
  for (let r = 0; r < m; r++) {
    if (model.rowLower[r] !== model.rowUpper[r]) continue;
    let allInt = true;
    let allOne = true;
    let cnt = 0;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      if (!isIntegral(model.colType[c])) {
        allInt = false;
        break;
      }
      if (Math.abs(model.aValue[k] - 1.0) > 1e-9) {
        allOne = false;
      }
      cnt += 1;
    }
    if (allInt && allOne && cnt > 0) {
      const uid = st.units.length;
      const cols: number[] = [];
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const c = model.aIndex[k];
        if (st.xUnit[c] < 0) {
          st.xUnit[c] = uid;
        }
        cols.push(c);
      }
      st.units.push(cols);
      st.unitSum.push(model.rowLower[r]);
    }
  }

  // 2. `link` rows: exactly two nonzeros `(x:+a, z:-b)` with `z` binary.
  for (let c = 0; c < n; c++) {
    if (!isIntegral(model.colType[c])) continue;
    const cs = csc.start[c];
    const ce = csc.start[c + 1];
    for (let kk = cs; kk < ce; kk++) {
      const r = csc.index[kk];
      const a = csc.value[kk];
      if (
        model.aStart[r + 1] - model.aStart[r] !== 2 ||
        model.rowUpper[r] > 0.0 ||
        model.rowLower[r] > -INF * 0.5
      ) {
        continue;
      }
      let zc = -1;
      let b = 0.0;
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const cc = model.aIndex[k];
        if (cc !== c) {
          zc = cc;
          b = model.aValue[k];
        }
      }
      if (zc >= 0 && model.colType[zc] === VAR_BINARY && a > 0.0 && b < 0.0) {
        st.xZ[c] = zc;
      }
    }
  }

  // 3. Load definition: equality with exactly one continuous and >=1 integer.
  for (let r = 0; r < m; r++) {
    if (model.rowLower[r] !== model.rowUpper[r]) continue;
    let contCol = -1;
    let contA = 0.0;
    let ncont = 0;
    const ints: Change[] = [];
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      const a = model.aValue[k];
      if (isIntegral(model.colType[c])) {
        ints.push([c, a]);
      } else {
        ncont += 1;
        contCol = c;
        contA = a;
      }
    }
    if (ncont === 1 && ints.length > 0 && Math.abs(contA) > 1e-12) {
      for (const [c, a] of ints) {
        if (st.xLoad[c] < 0) {
          st.xLoad[c] = contCol;
          st.xDur[c] = -a / contA;
        }
      }
    }
  }

  // 4. peak (`load - peak <= 0`) and min (`load - min >= 0`) rows.
  for (let r = 0; r < m; r++) {
    if (model.aStart[r + 1] - model.aStart[r] !== 2) continue;
    let pos = -1;
    let neg = -1;
    let allCont = true;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      const a = model.aValue[k];
      if (isIntegral(model.colType[c])) {
        allCont = false;
        break;
      }
      if (a > 0.0) {
        pos = c;
      } else {
        neg = c;
      }
    }
    if (!allCont || pos < 0 || neg < 0) continue;
    if (model.rowUpper[r] < INF_HALF && model.rowLower[r] < -INF_HALF) {
      st.peak[pos] = neg;
    } else if (model.rowLower[r] > -INF_HALF && model.rowUpper[r] > INF_HALF) {
      st.minv[pos] = neg;
    }
  }

  // 5. Sessions = loads sharing a `peak`.
  const peakToSession = new Array<number>(n).fill(-1);
  for (let l = 0; l < n; l++) {
    const pk = st.peak[l];
    if (pk >= 0) {
      if (peakToSession[pk] < 0) {
        peakToSession[pk] = st.sessionLoads.length;
        st.sessionLoads.push([]);
      }
      const sid = peakToSession[pk];
      st.loadSession[l] = sid;
      st.sessionLoads[sid].push(l);
    }
  }

  // 6. `total = sum(loads)`: all-continuous equality with one non-load column.
  for (let r = 0; r < m; r++) {
    if (model.rowLower[r] !== model.rowUpper[r]) continue;
    let allCont = true;
    const conts: number[] = [];
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      if (isIntegral(model.colType[c])) {
        allCont = false;
        break;
      }
      conts.push(c);
    }
    if (!allCont || conts.length === 0) continue;
    const loadCols: number[] = [];
    const others: number[] = [];
    for (const c of conts) {
      if (st.peak[c] >= 0) {
        loadCols.push(c);
      } else {
        others.push(c);
      }
    }
    if (loadCols.length > 0 && others.length === 1) {
      for (const lc of loadCols) {
        st.total[lc] = others[0];
      }
    }
  }

  const isPeakCol = new Array<boolean>(n).fill(false);
  const isMinCol = new Array<boolean>(n).fill(false);
  const isTotalCol = new Array<boolean>(n).fill(false);
  for (let l = 0; l < n; l++) {
    if (st.peak[l] >= 0) isPeakCol[st.peak[l]] = true;
    if (st.minv[l] >= 0) isMinCol[st.minv[l]] = true;
    if (st.total[l] >= 0) isTotalCol[st.total[l]] = true;
  }

  // 7. range: 3 continuous columns, peak + min + the range itself.
  for (let r = 0; r < m; r++) {
    if (model.aStart[r + 1] - model.aStart[r] !== 3) continue;
    let allCont = true;
    const conts: number[] = [];
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      if (isIntegral(model.colType[c])) {
        allCont = false;
        break;
      }
      conts.push(c);
    }
    if (!allCont) continue;
    let pk = -1;
    let mn = -1;
    let th = -1;
    for (const c of conts) {
      if (isPeakCol[c]) {
        pk = c;
      } else if (isMinCol[c]) {
        mn = c;
      } else {
        th = c;
      }
    }
    if (pk >= 0 && mn >= 0 && th >= 0) {
      for (let l = 0; l < n; l++) {
        if (st.peak[l] === pk) {
          st.range[l] = th;
        }
      }
    }
  }
  const isRangeCol = new Array<boolean>(n).fill(false);
  for (let l = 0; l < n; l++) {
    if (st.range[l] >= 0) isRangeCol[st.range[l]] = true;
  }

  // 8. Deviation rows: a continuous `<=` row carrying an unclassified `-1`
  //    coefficient (the deviation is the only remaining auxiliary).
  st.devRowsOfCol = Array.from({ length: n }, () => [] as number[]);
  for (let r = 0; r < m; r++) {
    if (model.rowUpper[r] >= INF_HALF) continue;
    let allCont = true;
    let cand = -1;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      const a = model.aValue[k];
      if (isIntegral(model.colType[c])) {
        allCont = false;
        break;
      }
      if (Math.abs(a + 1.0) < 1e-9) {
        const cls =
          st.peak[c] >= 0 ||
          isPeakCol[c] ||
          isMinCol[c] ||
          isTotalCol[c] ||
          isRangeCol[c];
        if (!cls) {
          cand = c;
        }
      }
    }
    if (allCont && cand >= 0) {
      st.devRows.push([cand, r]);
      st.devRowsOfCol[cand].push(r);
    }
  }

  st.devOfSession = Array.from({ length: st.sessionLoads.length }, () => [] as number[]);
  for (const [dc, r] of st.devRows) {
    let sid = -1;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      if (st.peak[c] >= 0) {
        sid = st.loadSession[c];
        break;
      }
    }
    if (sid >= 0) {
      if (!st.devOfSession[sid].includes(dc)) {
        st.devOfSession[sid].push(dc);
      }
    }
  }

  // 9. Finite caps directly on a load column.
  for (let r = 0; r < m; r++) {
    if (model.aStart[r + 1] - model.aStart[r] !== 1) continue;
    const c = model.aIndex[model.aStart[r]];
    if (!isIntegral(model.colType[c]) && model.rowUpper[r] < INF_HALF) {
      if (model.rowUpper[r] < st.cap[c]) {
        st.cap[c] = model.rowUpper[r];
      }
    }
  }

  // Feeders: load column -> x columns feeding it.
  for (let c = 0; c < n; c++) {
    if (st.xLoad[c] >= 0) {
      st.feeders[st.xLoad[c]].push(c);
    }
  }

  st.detected = st.units.length > 0 && st.sessionLoads.length > 0;
  return st;
}

/** Residuals: row activity, per-row violation, total violation. */
function compute(model: Model, x: readonly number[]): [number[], number[], number] {
  const lhs = new Array<number>(model.numRow).fill(0.0);
  for (let r = 0; r < model.numRow; r++) {
    let s = 0.0;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      s += model.aValue[k] * x[model.aIndex[k]];
    }
    lhs[r] = s;
  }
  const viol = new Array<number>(model.numRow);
  let total = 0.0;
  for (let r = 0; r < model.numRow; r++) {
    const v = rowViol(model, r, lhs[r]);
    viol[r] = v;
    total += v;
  }
  return [lhs, viol, total];
}

/** Recompute each load column from the `x` columns feeding it. */
function syncLoads(st: Struct, x: number[]): void {
  for (let l = 0; l < st.feeders.length; l++) {
    if (st.peak[l] < 0) continue;
    let s = 0.0;
    for (const c of st.feeders[l]) {
      s += st.xDur[c] * x[c];
    }
    x[l] = s;
  }
}

/** Snap every auxiliary to its exact implied value given the current loads. */
function syncAll(model: Model, st: Struct, x: number[]): void {
  for (let s = 0; s < st.sessionLoads.length; s++) {
    const loads = st.sessionLoads[s];
    if (loads.length === 0) continue;
    const vals = loads.map((l) => x[l]);
    let total = 0.0;
    for (const v of vals) total += v;
    let peak = -Infinity;
    let min = Infinity;
    for (const v of vals) {
      peak = fmax(peak, v);
      min = fmin(min, v);
    }
    const lc0 = loads[0];
    if (st.total[lc0] >= 0) x[st.total[lc0]] = total;
    if (st.peak[lc0] >= 0) x[st.peak[lc0]] = peak;
    if (st.minv[lc0] >= 0) x[st.minv[lc0]] = min;
    if (st.range[lc0] >= 0) x[st.range[lc0]] = peak - min;
  }
  for (let dc = 0; dc < st.devRowsOfCol.length; dc++) {
    if (st.devRowsOfCol[dc].length === 0) continue;
    let req = 0.0;
    for (const r of st.devRowsOfCol[dc]) {
      let ssum = 0.0;
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const c = model.aIndex[k];
        if (c === dc) continue;
        ssum += model.aValue[k] * x[c];
      }
      if (ssum > req) req = ssum;
    }
    x[dc] = fmax(req, 0.0);
  }
}

/** Required value of deviation column `dc` under `newval`. */
function devRequired(
  model: Model,
  st: Struct,
  dc: number,
  newval: (c: number) => number,
): number {
  let req = 0.0;
  for (const r of st.devRowsOfCol[dc]) {
    let ssum = 0.0;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      if (c === dc) continue;
      ssum += model.aValue[k] * newval(c);
    }
    if (ssum > req) req = ssum;
  }
  return fmax(req, 0.0);
}

/** Deltas for the auxiliaries of session `s` implied by load changes `ld`. */
function auxDeltas(
  model: Model,
  st: Struct,
  x: readonly number[],
  s: number,
  ld: readonly Change[],
): Change[] {
  const loads = st.sessionLoads[s];
  if (loads.length === 0) return [];
  const getl = (lc: number): number => {
    let v = x[lc];
    for (const [c, d] of ld) {
      if (c === lc) v += d;
    }
    return v;
  };
  const newload = loads.map((l) => getl(l));
  let newTotal = 0.0;
  for (const v of newload) newTotal += v;
  let newPeak = -Infinity;
  let newMin = Infinity;
  for (const v of newload) {
    newPeak = fmax(newPeak, v);
    newMin = fmin(newMin, v);
  }
  const newRange = newPeak - newMin;
  const lc0 = loads[0];
  const out: Change[] = [];

  const tc = st.total[lc0];
  const tcCol = tc >= 0 ? tc : null;
  if (tcCol !== null) {
    if (Math.abs(x[tcCol] - newTotal) > 1e-12) {
      out.push([tcCol, newTotal - x[tcCol]]);
    }
  }
  const pc = st.peak[lc0];
  if (pc >= 0) {
    if (Math.abs(x[pc] - newPeak) > 1e-12) {
      out.push([pc, newPeak - x[pc]]);
    }
  }
  const mc = st.minv[lc0];
  if (mc >= 0) {
    if (Math.abs(x[mc] - newMin) > 1e-12) {
      out.push([mc, newMin - x[mc]]);
    }
  }
  const rc = st.range[lc0];
  if (rc >= 0) {
    if (Math.abs(x[rc] - newRange) > 1e-12) {
      out.push([rc, newRange - x[rc]]);
    }
  }

  for (const dc of st.devOfSession[s]) {
    const newv = (c: number): number => {
      if (c === tcCol) {
        return newTotal;
      } else if (loads.includes(c)) {
        return getl(c);
      } else {
        return x[c];
      }
    };
    const req = devRequired(model, st, dc, newv);
    if (Math.abs(x[dc] - req) > 1e-12) {
      out.push([dc, req - x[dc]]);
    }
  }
  return out;
}

/** Full column-change list for a move. */
function buildFull(
  model: Model,
  st: Struct,
  x: readonly number[],
  decision: readonly Change[],
): Change[] {
  const ch: Change[] = [];
  const ldel: Change[] = [];
  for (const [c, d] of decision) {
    ch.push([c, d]);
    if (c < st.xLoad.length) {
      const lc = st.xLoad[c];
      if (lc >= 0) {
        ldel.push([lc, st.xDur[c] * d]);
      }
    }
  }
  // Combine repeated load deltas.
  const combined: Change[] = [];
  for (const [lc, d] of ldel) {
    let found = false;
    for (const e of combined) {
      if (e[0] === lc) {
        e[1] = e[1] + d;
        found = true;
        break;
      }
    }
    if (!found) combined.push([lc, d]);
  }
  const affected: number[] = [];
  for (const [lc] of combined) {
    const s = st.loadSession[lc];
    if (s >= 0 && !affected.includes(s)) {
      affected.push(s);
    }
  }
  for (const [lc, d] of combined) {
    if (d !== 0.0) ch.push([lc, d]);
  }
  for (const s of affected) {
    const ld: Change[] = combined.filter(([lc]) => st.loadSession[lc] === s);
    for (const a of auxDeltas(model, st, x, s, ld)) {
      ch.push(a);
    }
  }
  return ch;
}

/** Exact change in total row violation for a set of column changes. */
function evalDelta(
  model: Model,
  csc: Csc,
  lhs: readonly number[],
  viol: readonly number[],
  changes: readonly Change[],
  scratch: Change[],
): number {
  scratch.length = 0;
  for (const [c, d] of changes) {
    if (d === 0.0) continue;
    const cs = csc.start[c];
    const ce = csc.start[c + 1];
    for (let kk = cs; kk < ce; kk++) {
      const r = csc.index[kk];
      const a = csc.value[kk];
      let found = false;
      for (const e of scratch) {
        if (e[0] === r) {
          e[1] = e[1] + a * d;
          found = true;
          break;
        }
      }
      if (!found) scratch.push([r, a * d]);
    }
  }
  let delta = 0.0;
  for (const [r, ch] of scratch) {
    delta += rowViol(model, r, lhs[r] + ch) - viol[r];
  }
  return delta;
}

function applyChanges(
  model: Model,
  csc: Csc,
  x: number[],
  lhs: number[],
  viol: number[],
  changes: readonly Change[],
): void {
  for (const [c, d] of changes) {
    if (d === 0.0) continue;
    x[c] += d;
    const cs = csc.start[c];
    const ce = csc.start[c + 1];
    for (let kk = cs; kk < ce; kk++) {
      const r = csc.index[kk];
      const a = csc.value[kk];
      viol[r!] -= rowViol(model, r, lhs[r]);
      lhs[r!] += a * d;
      viol[r!] += rowViol(model, r, lhs[r]);
    }
  }
}

function transferDecision(
  st: Struct,
  x: readonly number[],
  c1: number,
  c2: number,
  d: number,
): Change[] {
  const dec: Change[] = [];
  dec.push([c1, -d]);
  const z1 = st.xZ[c1];
  if (z1 >= 0 && x[c1] - d <= 1e-9) {
    dec.push([z1, -1.0]);
  }
  dec.push([c2, d]);
  const z2 = st.xZ[c2];
  if (z2 >= 0 && x[z2] < 0.5) {
    dec.push([z2, 1.0]);
  }
  return dec;
}

interface Best {
  delta: number;
  changes: Change[];
}

function consider(
  model: Model,
  csc: Csc,
  st: Struct,
  x: readonly number[],
  lhs: readonly number[],
  viol: readonly number[],
  dec: readonly Change[],
  scratch: Change[],
  best: Best | null,
  rng: Rng,
): Best | null {
  const full = buildFull(model, st, x, dec);
  const d = evalDelta(model, csc, lhs, viol, full, scratch);
  const better =
    best === null ||
    d < best.delta - 1e-12 ||
    (d <= best.delta + 1e-12 && rng.nextF64() < 0.15);
  if (better) {
    return { delta: d, changes: full };
  }
  return best;
}

/** Generate the best improving (or least-worsening) move for the state. */
function findBest(
  model: Model,
  csc: Csc,
  st: Struct,
  x: readonly number[],
  lhs: readonly number[],
  viol: readonly number[],
  feeders: readonly number[][],
  rng: Rng,
  scratch: Change[],
): Best | null {
  const n = model.numCol;
  let best: Best | null = null;

  // Per-unit deficit (positive => unplaced matches).
  const need = new Array<number>(st.units.length).fill(0.0);
  for (let u = 0; u < st.units.length; u++) {
    let s = 0.0;
    for (const c of st.units[u]) {
      s += x[c];
    }
    need[u] = st.unitSum[u] - s;
  }

  // A) Single-variable create / drop.
  for (let u = 0; u < st.units.length; u++) {
    const cols = st.units[u];
    if (need[u] > 0.5) {
      for (const c of cols) {
        let d = fmin(need[u], model.colUpper[c] - x[c]);
        if (st.xLoad[c] >= 0) {
          const lc = st.xLoad[c];
          if (st.cap[lc] < INF_HALF && st.xDur[c] > 1e-12) {
            const room = fmax(st.cap[lc] - x[lc], 0.0) / st.xDur[c];
            d = fmin(d, room);
          }
        }
        d = Math.floor(d);
        if (d < 1.0) continue;
        const dec: Change[] = [[c, d]];
        const z = st.xZ[c];
        if (z >= 0 && x[z] < 0.5) {
          dec.push([z, 1.0]);
        }
        best = consider(model, csc, st, x, lhs, viol, dec, scratch, best, rng);
      }
    } else if (need[u] < -0.5) {
      for (const c of cols) {
        if (x[c] < 0.5) continue;
        const d = Math.floor(fmin(-need[u], x[c]));
        if (d < 1.0) continue;
        const dec: Change[] = [[c, -d]];
        const z = st.xZ[c];
        if (z >= 0 && x[c] - d <= 1e-9 && x[z] > 0.5) {
          dec.push([z, -1.0]);
        }
        best = consider(model, csc, st, x, lhs, viol, dec, scratch, best, rng);
      }
    }
  }

  // B) Coupled transfers, only once every unit's total is placed.
  let totOk = true;
  for (const d of need) {
    if (!(Math.abs(d) < 0.5)) {
      totOk = false;
      break;
    }
  }
  if (totOk) {
    const xSess = (c: number): number => {
      const l = st.xLoad[c];
      if (l >= 0) return st.loadSession[l];
      return -1;
    };

    const sources: number[] = [];
    // Overloaded (s,r) loads: push everything feeding them.
    for (let l = 0; l < n; l++) {
      if (st.peak[l] >= 0 && st.cap[l] < INF_HALF && x[l] > st.cap[l] + TOL) {
        for (const c of feeders[l]) {
          if (x[c] >= 1.0) {
            sources.push(c);
          }
        }
      }
    }
    // `onesplit` conflicts: a unit/session with more than one positive ring.
    for (const cols of st.units) {
      const sessions: number[] = [];
      for (const c of cols) {
        const s = xSess(c);
        if (x[c] >= 1.0 && !sessions.includes(s)) {
          sessions.push(s);
        }
      }
      for (const s of sessions) {
        const pos = cols.filter((c) => x[c] >= 1.0 && xSess(c) === s);
        if (pos.length > 1) {
          for (const c of pos) sources.push(c);
        }
      }
    }
    sources.sort((a, b) => a - b);
    // dedup
    let w = 0;
    for (let i = 0; i < sources.length; i++) {
      if (i === 0 || sources[i] !== sources[w - 1]) {
        sources[w++] = sources[i];
      }
    }
    sources.length = w;

    for (const c1 of sources) {
      const u = st.xUnit[c1];
      if (u < 0) continue;
      for (const c2 of st.units[u]) {
        if (c2 === c1) continue;
        const full = x[c1];
        if (full >= 1.0) {
          const dec = transferDecision(st, x, c1, c2, full);
          best = consider(model, csc, st, x, lhs, viol, dec, scratch, best, rng);
        }
        if (x[c1] > 1.5) {
          const dec = transferDecision(st, x, c1, c2, 1.0);
          best = consider(model, csc, st, x, lhs, viol, dec, scratch, best, rng);
        }
      }
    }
  }

  return best;
}

/** Random transfer kick to escape a plateau. Returns false when nothing moves. */
function kick(
  model: Model,
  csc: Csc,
  st: Struct,
  x: number[],
  lhs: number[],
  viol: number[],
  rng: Rng,
): boolean {
  const nu = st.units.length;
  if (nu === 0) return false;
  for (let iter = 0; iter < 64; iter++) {
    const u = rng.below(nu);
    const cols = st.units[u];
    if (cols.length === 0) continue;
    const c1 = cols[rng.below(cols.length)];
    if (x[c1] < 1.0) continue;
    const c2 = cols[rng.below(cols.length)];
    if (c2 === c1) continue;
    const maxd = Math.trunc(x[c1]);
    if (maxd === 0) continue;
    const d = 1.0 + rng.below(maxd);
    const dec = transferDecision(st, x, c1, c2, d);
    const full = buildFull(model, st, x, dec);
    applyChanges(model, csc, x, lhs, viol, full);
    return true;
  }
  return false;
}

/** Result of a repair pass. */
export interface Repair {
  x: number[];
  moves: number;
}

function repairStructured(
  model: Model,
  csc: Csc,
  st: Struct,
  x: number[],
  maxMoves: number,
  rng: Rng,
): Repair | null {
  const n = model.numCol;
  // Clean the fixed-charge bits to match the current `x`.
  for (let c = 0; c < n; c++) {
    const z = st.xZ[c];
    if (z >= 0) {
      if (x[c] <= 1e-9) {
        x[z] = 0.0;
      } else if (x[z] < 0.5) {
        x[z] = 1.0;
      }
    }
  }
  syncLoads(st, x);
  syncAll(model, st, x);
  let [lhs, viol, total] = compute(model, x);
  let moves = 0;
  let bestTotal = total;
  let stall = 0;
  const stallLimit = 20000;
  const scratch: Change[] = [];

  for (;;) {
    if (total <= TOL) {
      const chk = checkSolution(model, x, TOL);
      if (chk.feasible) {
        return { x, moves };
      }
      // Bookkeeping drifted; resynchronise from scratch.
      syncAll(model, st, x);
      const [l, v, t] = compute(model, x);
      lhs = l;
      viol = v;
      total = t;
      if (total <= TOL) {
        // `compute` and `checkSolution` disagree and cannot be reconciled.
        break;
      }
    }
    if (moves >= maxMoves || stall > stallLimit) {
      break;
    }

    const best = findBest(model, csc, st, x, lhs, viol, st.feeders, rng, scratch);
    if (best !== null && best.delta < -1e-12) {
      applyChanges(model, csc, x, lhs, viol, best.changes);
      moves += 1;
      const [l, v, t] = compute(model, x);
      lhs = l;
      viol = v;
      total = t;
    } else {
      if (!kick(model, csc, st, x, lhs, viol, rng)) {
        break;
      }
      moves += 1;
      const [l, v, t] = compute(model, x);
      lhs = l;
      viol = v;
      total = t;
    }

    if (total < bestTotal - 1e-9) {
      bestTotal = total;
      stall = 0;
    } else {
      stall += 1;
    }
  }

  if (checkSolution(model, x, TOL).feasible) {
    return { x, moves };
  }
  return null;
}

/**
 * Try to turn `x0` into a fully feasible point using structure-aware moves.
 * Returns a repair only when `checkSolution(model, &x, tol)` agrees.
 */
export function repair(
  model: Model,
  x0: readonly number[],
  maxMoves: number,
  seed: number,
): Repair | null {
  if (model.numCol === 0) {
    return { x: [], moves: 0 };
  }
  if (x0.length !== model.numCol) {
    return null;
  }
  const csc = Csc.build(model);
  const x = x0.slice();
  for (let c = 0; c < model.numCol; c++) {
    x[c] = clamp(x[c], model.colLower[c], model.colUpper[c]);
    if (isIntegral(model.colType[c])) {
      x[c] = Math.round(x[c]);
    }
    x[c] = clamp(x[c], model.colLower[c], model.colUpper[c]);
  }
  if (checkSolution(model, x, TOL).feasible) {
    return { x, moves: 0 };
  }

  const rng = new Rng(seed);
  const st = detect(model, csc);
  if (st.detected) {
    const r = repairStructured(model, csc, st, x, maxMoves, rng);
    if (r !== null) {
      return r;
    }
  }
  return null;
}

/**
 * Construct a feasible plan for the **inlined** planning shape (`sum dur*x`
 * appears directly in the cap/peak/dev rows and there is no explicit `load`
 * column). Greedy, cap-aware placement plus an exact projection of the
 * continuous auxiliaries.
 */
function constructWith(model: Model, useLpt: boolean): number[] | null {
  const n = model.numCol;
  const m = model.numRow;
  const csc = Csc.build(model);

  // 1. Units: `tot` equality rows, all-integer, all coefficients +1.
  const xUnit = new Array<number>(n).fill(-1);
  const units: Array<[number[], number]> = [];
  for (let r = 0; r < m; r++) {
    if (model.rowLower[r] !== model.rowUpper[r]) continue;
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    if (ks === ke) continue;
    let allInt = true;
    let allOne = true;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      if (!isIntegral(model.colType[c])) {
        allInt = false;
        break;
      }
      if (Math.abs(model.aValue[k] - 1.0) > 1e-9) {
        allOne = false;
      }
    }
    if (allInt && allOne) {
      const uid = units.length;
      const cols: number[] = [];
      for (let k = ks; k < ke; k++) {
        const c = model.aIndex[k];
        if (xUnit[c] < 0) {
          xUnit[c] = uid;
        }
        cols.push(c);
      }
      units.push([cols, model.rowLower[r]]);
    }
  }
  if (units.length === 0) {
    return null;
  }

  // 2. Sessions: equality with exactly one positive continuous and integer
  //    columns with negative coefficients. Gives `total_p` and `dur_u`.
  const xDur = new Array<number>(n).fill(0.0);
  const xSess = new Array<number>(n).fill(-1);
  let nsess = 0;
  for (let r = 0; r < m; r++) {
    if (model.rowLower[r] !== model.rowUpper[r]) continue;
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    let ncont = 0;
    let contA = 0.0;
    let ok = true;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      if (isIntegral(model.colType[c])) {
        if (model.aValue[k] > 0.0) {
          ok = false;
        }
      } else {
        ncont += 1;
        contA = model.aValue[k];
      }
    }
    if (ncont === 1 && ok && contA > 1e-12) {
      for (let k = ks; k < ke; k++) {
        const c = model.aIndex[k];
        if (isIntegral(model.colType[c])) {
          xDur[c] = -model.aValue[k] / contA;
          xSess[c] = nsess;
        }
      }
      nsess += 1;
    }
  }
  if (nsess === 0) {
    return null;
  }

  // 3. Load groups: finite-upper all-integer rows (the `cap_p_r` rows).
  const xGroup = new Array<number>(n).fill(-1);
  const groupSess: number[] = [];
  const groupRing: number[] = [];
  const groupCap: number[] = [];
  const ringCount = new Array<number>(nsess).fill(0);
  for (let r = 0; r < m; r++) {
    if (model.rowLower[r] === model.rowUpper[r]) continue;
    if (!Number.isFinite(model.rowUpper[r]) || model.rowUpper[r] <= 0.0) continue;
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    if (ks === ke) continue;
    let allInt = true;
    let allBin = true;
    let allOne = true;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      if (!isIntegral(model.colType[c])) {
        allInt = false;
        break;
      }
      if (model.colType[c] !== VAR_BINARY && !isBinaryLike(model, c)) {
        allBin = false;
      }
      if (Math.abs(model.aValue[k] - 1.0) > 1e-9) {
        allOne = false;
      }
    }
    if (!allInt || (allBin && allOne)) {
      continue; // skip `onesplit` and non-integer rows
    }
    // Require a single consistent session.
    let sess = -1;
    let good = true;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      if (xSess[c] < 0) {
        good = false;
        break;
      }
      if (sess < 0) {
        sess = xSess[c];
      } else if (sess !== xSess[c]) {
        good = false;
        break;
      }
    }
    if (!good) continue;
    const gid = groupCap.length;
    const ring = ringCount[sess];
    ringCount[sess] += 1;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      if (xGroup[c] < 0) {
        xGroup[c] = gid;
      }
    }
    groupSess.push(sess);
    groupRing.push(ring);
    groupCap.push(model.rowUpper[r]);
  }
  const ng = groupCap.length;
  if (ng === 0) {
    return null;
  }

  // 4. Fixed-charge links `x - M z <= 0`.
  const xZ = new Array<number>(n).fill(-1);
  for (let c = 0; c < n; c++) {
    if (!isIntegral(model.colType[c])) continue;
    const cs = csc.start[c];
    const ce = csc.start[c + 1];
    for (let kk = cs; kk < ce; kk++) {
      const r = csc.index[kk];
      const a = csc.value[kk];
      if (
        model.aStart[r + 1] - model.aStart[r] !== 2 ||
        model.rowUpper[r] > 0.0 ||
        model.rowLower[r] > -INF * 0.5
      ) {
        continue;
      }
      let zc = -1;
      let b = 0.0;
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const cc = model.aIndex[k];
        if (cc !== c) {
          zc = cc;
          b = model.aValue[k];
        }
      }
      if (zc >= 0 && isBinaryLike(model, zc) && a > 0.0 && b < 0.0) {
        xZ[c] = zc;
      }
    }
  }

  // 5. For each unit and group, the (unique) x column.
  const unitGroupCol: number[][] = [];
  for (let u = 0; u < units.length; u++) {
    unitGroupCol.push(new Array<number>(ng).fill(-1));
  }
  for (let u = 0; u < units.length; u++) {
    for (const c of units[u][0]) {
      const g = xGroup[c];
      if (g >= 0) {
        unitGroupCol[u][g] = c;
      }
    }
  }

  // 6. Greedy cap-aware placement.
  const x = new Array<number>(n).fill(0.0);
  const load = new Array<number>(ng).fill(0.0);
  const durU = units.map(([cols]) => xDur[cols[0]]);
  if (useLpt) {
    // LPT: even session split, then longest block first onto the least-loaded
    // ring.
    const sessionBlocks: Array<Array<[number, number]>> = Array.from(
      { length: nsess },
      () => [] as Array<[number, number]>,
    );
    for (let u = 0; u < units.length; u++) {
      const matches = units[u][1];
      const mm = Math.round(matches);
      if (mm <= 0 || nsess === 0) continue;
      const base = Math.trunc(mm / nsess);
      let extra = mm % nsess;
      for (let s = 0; s < nsess; s++) {
        let c = base;
        if (extra > 0) {
          c += 1;
          extra -= 1;
        }
        if (c > 0) {
          sessionBlocks[s].push([u, c]);
        }
      }
    }
    for (let s = 0; s < nsess; s++) {
      const bl = sessionBlocks[s];
      bl.sort((a, b) => {
        const da = durU[a[0]] * a[1];
        const db = durU[b[0]] * b[1];
        return db < da ? -1 : db > da ? 1 : 0;
      });
      for (const [u, c] of bl) {
        const blk = durU[u] * c;
        let best = -1;
        let bestl = Infinity;
        let fallback = -1;
        let fallbackRem = -Infinity;
        for (let g = 0; g < ng; g++) {
          if (groupSess[g] !== s || unitGroupCol[u][g] < 0) continue;
          if (load[g] + blk <= groupCap[g] + 1e-9 && load[g] < bestl) {
            bestl = load[g];
            best = g;
          }
          const rem = groupCap[g] - load[g];
          if (rem > fallbackRem) {
            fallbackRem = rem;
            fallback = g;
          }
        }
        if (best < 0) best = fallback;
        if (best < 0) continue;
        const g = best;
        const col = unitGroupCol[u][g];
        x[col] = c;
        load[g] += durU[u] * c;
      }
    }
  } else {
    // Cap-greedy: place each match in the ring with the most remaining cap,
    // keeping one ring per (unit, session).
    for (let u = 0; u < units.length; u++) {
      const [cols, matchCount] = units[u];
      const need = Math.round(matchCount);
      if (need <= 0) continue;
      const lock = new Array<number>(nsess).fill(-1);
      const count = new Array<number>(ng).fill(0);
      for (let it = 0; it < need; it++) {
        let best = -1;
        let bestRem = -Infinity;
        for (let g = 0; g < ng; g++) {
          const col = unitGroupCol[u][g];
          if (col < 0) continue;
          const p = groupSess[g];
          const rr = groupRing[g];
          if (lock[p] !== -1 && lock[p] !== rr) continue;
          const rem = groupCap[g] - load[g];
          if (rem > bestRem) {
            bestRem = rem;
            best = g;
          }
        }
        if (best < 0) {
          return null;
        }
        const g = best;
        lock[groupSess[g]] = groupRing[g];
        const col = unitGroupCol[u][g];
        load[g] += xDur[col];
        count[g] += 1;
      }
      for (const c of cols) {
        const g = xGroup[c];
        if (g >= 0) {
          x[c] = count[g];
        }
      }
    }
  }

  // 7. Set the fixed-charge binaries and project the continuous auxiliaries.
  for (let c = 0; c < n; c++) {
    if (x[c] > 0.5 && xZ[c] >= 0) {
      x[xZ[c]] = 1.0;
    }
  }
  projectContinuous(model, csc, x);

  // Balance ring loads per session.
  for (let round = 0; round < 60; round++) {
    const loads = new Array<number>(ng).fill(0.0);
    for (let c = 0; c < n; c++) {
      const g = xGroup[c];
      if (g >= 0) {
        loads[g!] += xDur[c] * x[c];
      }
    }
    let moved = false;
    for (let p = 0; p < nsess; p++) {
      const gs: number[] = [];
      for (let g = 0; g < ng; g++) {
        if (groupSess[g] === p) gs.push(g);
      }
      if (gs.length < 2) continue;
      let gmax = gs[0];
      let gmin = gs[0];
      for (const g of gs) {
        if (loads[g] > loads[gmax]) gmax = g;
        if (loads[g] < loads[gmin]) gmin = g;
      }
      const spread = loads[gmax] - loads[gmin];
      if (spread <= 1e-6) continue;
      for (let u = 0; u < units.length; u++) {
        const cuRow = unitGroupCol[u][gmax];
        const cmRow = unitGroupCol[u][gmin];
        if (cuRow < 0 || cmRow < 0) continue;
        const cu = cuRow;
        const cm = cmRow;
        const mv = x[cu];
        if (mv <= 0.0) continue;
        const d = xDur[cu] * mv;
        if (2.0 * d <= spread + 1e-9 && loads[gmin] + d <= groupCap[gmin] + 1e-9) {
          x[cu] = 0.0;
          x[cm!] += mv;
          if (xZ[cu] >= 0) {
            x[xZ[cu]] = 0.0;
          }
          if (xZ[cm] >= 0 && x[cm] > 0.0) {
            x[xZ[cm]] = 1.0;
          }
          loads[gmax!] -= d;
          loads[gmin!] += d;
          moved = true;
          break;
        }
      }
    }
    if (!moved) break;
  }
  projectContinuous(model, csc, x);

  if (checkSolution(model, x, TOL).feasible) {
    return x;
  }
  return null;
}

/**
 * Construct a feasible plan for the inlined planning shape. Tries two
 * assignments (LPT load-balancing and a cap-greedy) and returns the better
 * feasible one.
 */
export function constructInlinedPlanning(model: Model): number[] | null {
  const a = constructWith(model, true);
  const b = constructWith(model, false);
  if (a !== null && b !== null) {
    return model.objective(a) <= model.objective(b) ? a : b;
  }
  if (a !== null) return a;
  if (b !== null) return b;
  return null;
}

// ---------------------------------------------------------------------------
// Objective local search for the planning shape
// ---------------------------------------------------------------------------

/** Detected planning structure, in terms of the inlined shape. */
interface PShape {
  nsess: number;
  gFeeder: Array<Array<[number, number]>>;
  gSess: number[];
  gCap: number[];
  groups: number[][];
  uDur: number[];
  xcol: number[][];
  zcol: number[][];
  ycol: number[][];
  xrOf: number[];
  aTotal: number[];
  aPeak: number[];
  aMin: number[];
  aRange: number[];
  aDev: number[][];
  devCoef: number[][];
  cost: number[];
}

/** Objective contribution of the auxiliaries of session `s`. */
function pSessCost(sh: PShape, s: number, load: (g: number) => number): number {
  const gs = sh.groups[s];
  if (gs.length === 0) return 0.0;
  let total = 0.0;
  let peak = -Infinity;
  let mn = Infinity;
  for (const g of gs) {
    const v = load(g);
    total += v;
    peak = fmax(peak, v);
    mn = fmin(mn, v);
  }
  const range = peak - mn;
  let c = 0.0;
  if (sh.aTotal[s] >= 0) {
    c += sh.cost[sh.aTotal[s]] * total;
  }
  if (sh.aPeak[s] >= 0) {
    c += sh.cost[sh.aPeak[s]] * peak;
  }
  if (sh.aMin[s] >= 0) {
    c += sh.cost[sh.aMin[s]] * mn;
  }
  if (sh.aRange[s] >= 0) {
    c += sh.cost[sh.aRange[s]] * range;
  }
  for (let ring = 0; ring < gs.length; ring++) {
    const dc = sh.aDev[s][ring];
    if (dc >= 0) {
      const v = load(gs[ring]);
      c += sh.cost[dc] * Math.abs(sh.devCoef[s][ring] * v - total);
    }
  }
  return c;
}

/** Detect the planning structure from the constraint matrix. */
function detectPlan(model: Model, csc: Csc): PShape | null {
  const n = model.numCol;
  const m = model.numRow;
  const negInf = -INF * 0.5;

  // x columns are integral and have a fixed-charge link row `x - M z <= 0`.
  const isX = new Array<boolean>(n).fill(false);
  for (let c = 0; c < n; c++) {
    if (!isIntegral(model.colType[c])) continue;
    const cs = csc.start[c];
    const ce = csc.start[c + 1];
    for (let kk = cs; kk < ce; kk++) {
      const r = csc.index[kk];
      const a = csc.value[kk];
      if (
        model.aStart[r + 1] - model.aStart[r] !== 2 ||
        model.rowUpper[r] > 0.0 ||
        model.rowLower[r] > -INF * 0.5
      ) {
        continue;
      }
      let zc = -1;
      let b = 0.0;
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const cc = model.aIndex[k];
        if (cc !== c) {
          zc = cc;
          b = model.aValue[k];
        }
      }
      if (zc >= 0 && isBinaryLike(model, zc) && a > 0.0 && b < 0.0) {
        isX[c] = true;
      }
    }
  }
  if (!isX.some((b) => b)) {
    return null;
  }

  // Units: equality rows whose columns are all `x` with coefficient +1.
  const xUnit = new Array<number>(n).fill(-1);
  const units: number[][] = [];
  for (let r = 0; r < m; r++) {
    if (model.rowLower[r] !== model.rowUpper[r]) continue;
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    if (ks === ke) continue;
    let ok = true;
    const cols: number[] = [];
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      if (!isX[c] || Math.abs(model.aValue[k] - 1.0) > 1e-9) {
        ok = false;
        break;
      }
      cols.push(c);
    }
    if (ok) {
      const uid = units.length;
      for (const c of cols) {
        if (xUnit[c] < 0) {
          xUnit[c] = uid;
        }
      }
      units.push(cols);
    }
  }
  if (units.length === 0) {
    return null;
  }
  const nu = units.length;

  // Groups `(session, ring)` from the `peakge` rows (`sum dur x - peak <= 0`).
  const xGroup = new Array<number>(n).fill(-1);
  const uDur = new Array<number>(nu).fill(0.0);
  const gFeeder: Array<Array<[number, number]>> = [];
  const gSess: number[] = [];
  const gRing: number[] = [];
  const groups: number[][] = [];
  const sessPeak: number[] = [];
  const peakIndex = new Map<number, number>();
  for (let r = 0; r < m; r++) {
    if (!Number.isFinite(model.rowUpper[r]) || model.rowUpper[r] > 0.0) {
      continue;
    }
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    let cont = -1;
    let ncont = 0;
    const colx: Array<[number, number]> = [];
    let ok = true;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      const a = model.aValue[k];
      if (isIntegral(model.colType[c])) {
        if (!isX[c]) {
          ok = false;
          break;
        }
        colx.push([c, a]);
      } else {
        ncont += 1;
        cont = c;
        if (a >= 0.0) {
          ok = false;
        }
      }
    }
    if (!ok || ncont !== 1 || colx.length === 0) {
      continue;
    }
    let sid: number;
    const found = peakIndex.get(cont);
    if (found !== undefined) {
      sid = found;
    } else {
      sid = sessPeak.length;
      sessPeak.push(cont);
      groups.push([]);
      peakIndex.set(cont, sid);
    }
    const gid = gFeeder.length;
    for (const [c, a] of colx) {
      if (xGroup[c] < 0) {
        xGroup[c] = gid;
      }
      const u = xUnit[c];
      if (u >= 0 && uDur[u] === 0.0) {
        uDur[u] = a;
      }
    }
    gRing.push(groups[sid].length);
    groups[sid].push(gid);
    gSess.push(sid);
    gFeeder.push(colx);
  }
  const ng = gFeeder.length;
  const nsess = groups.length;
  if (ng === 0 || nsess === 0) {
    return null;
  }

  // Caps: rows of all-`x` columns with a finite positive upper bound.
  const gCap = new Array<number>(ng).fill(Infinity);
  for (let r = 0; r < m; r++) {
    if (!Number.isFinite(model.rowUpper[r]) || model.rowUpper[r] <= 0.0) {
      continue;
    }
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    let g = -1;
    let ok = true;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      if (!isX[c]) {
        ok = false;
        break;
      }
      const cg = xGroup[c];
      if (g < 0) {
        g = cg;
      } else if (cg !== g) {
        ok = false;
        break;
      }
    }
    if (ok && g >= 0) {
      gCap[g] = fmin(gCap[g], model.rowUpper[r]);
    }
  }

  // x/z/y/xr column maps.
  const zcol: number[][] = [];
  const xcol: number[][] = [];
  for (let u = 0; u < nu; u++) {
    xcol.push(new Array<number>(ng).fill(-1));
    zcol.push(new Array<number>(ng).fill(-1));
  }
  for (let u = 0; u < nu; u++) {
    for (const c of units[u]) {
      const g = xGroup[c];
      if (g >= 0) {
        xcol[u][g] = c;
      }
    }
  }
  // z from the link rows.
  for (let u = 0; u < nu; u++) {
    for (let g = 0; g < ng; g++) {
      const c0 = xcol[u][g];
      if (c0 < 0) continue;
      const c = c0;
      const cs = csc.start[c];
      const ce = csc.start[c + 1];
      for (let kk = cs; kk < ce; kk++) {
        const r = csc.index[kk];
        const a = csc.value[kk];
        if (
          model.aStart[r + 1] - model.aStart[r] !== 2 ||
          model.rowUpper[r] > 0.0 ||
          model.rowLower[r] > -INF * 0.5
        ) {
          continue;
        }
        let zc = -1;
        let b = 0.0;
        for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
          const cc = model.aIndex[k];
          if (cc !== c) {
            zc = cc;
            b = model.aValue[k];
          }
        }
        if (zc >= 0 && isBinaryLike(model, zc) && a > 0.0 && b < 0.0) {
          zcol[u][g] = zc;
        }
      }
    }
  }
  const ycol: number[][] = [];
  for (let u = 0; u < nu; u++) {
    ycol.push(new Array<number>(nsess).fill(-1));
  }
  for (let r = 0; r < m; r++) {
    if (model.rowLower[r] !== model.rowUpper[r]) continue;
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    let yc = -1;
    const xs: number[] = [];
    let ok = true;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      if (isX[c]) {
        xs.push(c);
      } else if (isIntegral(model.colType[c])) {
        if (yc >= 0) {
          ok = false;
          break;
        }
        yc = c;
      } else {
        ok = false;
        break;
      }
    }
    if (ok && yc >= 0 && xs.length > 0) {
      const u = xUnit[xs[0]];
      const g = xGroup[xs[0]];
      if (u >= 0 && g >= 0) {
        ycol[u][gSess[g]] = yc;
      }
    }
  }
  const xrOf = new Array<number>(n).fill(-1);
  for (let r = 0; r < m; r++) {
    if (model.rowLower[r] !== model.rowUpper[r]) continue;
    if (model.aStart[r + 1] - model.aStart[r] !== 2) continue;
    const c0 = model.aIndex[model.aStart[r]];
    const c1 = model.aIndex[model.aStart[r] + 1];
    if (isX[c0] && !isX[c1] && isIntegral(model.colType[c1])) {
      xrOf[c0] = c1;
    } else if (isX[c1] && !isX[c0] && isIntegral(model.colType[c0])) {
      xrOf[c1] = c0;
    }
  }

  // Auxiliaries per session.
  const aTotal = new Array<number>(nsess).fill(-1);
  const aMin = new Array<number>(nsess).fill(-1);
  const aRange = new Array<number>(nsess).fill(-1);
  // total = sum loads (equality: one continuous `+`, the rest `x` with `-`).
  for (let r = 0; r < m; r++) {
    if (model.rowLower[r] !== model.rowUpper[r]) continue;
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    let cont = -1;
    let ncont = 0;
    let sid = -1;
    let ok = true;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      const a = model.aValue[k];
      if (isIntegral(model.colType[c])) {
        if (!isX[c] || a >= 0.0) {
          ok = false;
          break;
        }
        const g = xGroup[c];
        if (g >= 0) {
          sid = gSess[g];
        }
      } else {
        ncont += 1;
        cont = c;
        if (a <= 0.0) {
          ok = false;
        }
      }
    }
    if (ok && ncont === 1 && sid !== -1) {
      aTotal[sid] = cont;
    }
  }
  // min (finite lower, one continuous `-`, the rest `x` with `+`).
  for (let r = 0; r < m; r++) {
    if (!Number.isFinite(model.rowLower[r]) || model.rowLower[r] <= negInf) {
      continue;
    }
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    let cont = -1;
    let ncont = 0;
    let sid = -1;
    let ok = true;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      const a = model.aValue[k];
      if (isIntegral(model.colType[c])) {
        if (!isX[c] || a <= 0.0) {
          ok = false;
          break;
        }
        const g = xGroup[c];
        if (g >= 0) {
          sid = gSess[g];
        }
      } else {
        ncont += 1;
        cont = c;
        if (a >= 0.0) {
          ok = false;
        }
      }
    }
    if (ok && ncont === 1 && sid !== -1) {
      aMin[sid] = cont;
    }
  }
  // range (finite lower, three continuous: peak, min, range).
  for (let r = 0; r < m; r++) {
    if (!Number.isFinite(model.rowLower[r]) || model.rowLower[r] <= negInf) {
      continue;
    }
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    const conts: number[] = [];
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      if (!isIntegral(model.colType[c])) {
        conts.push(c);
      }
    }
    if (conts.length !== 3) {
      continue;
    }
    // Identify peak (has a `peakge` row) and min; the third is range.
    let sid = -1;
    for (const c of conts) {
      const found = peakIndex.get(c);
      if (found !== undefined) {
        sid = found;
        break;
      }
    }
    if (sid === -1) {
      continue;
    }
    const peak = sessPeak[sid];
    const mn = aMin[sid];
    for (const c of conts) {
      if (c !== peak && c !== mn) {
        aRange[sid] = c;
      }
    }
  }
  // dev (finite upper, continuous `dev` (`-`) + `total`, the rest `x`).
  const aDev: number[][] = groups.map((gs) => new Array<number>(gs.length).fill(-1));
  const devCoef: number[][] = groups.map((gs) => new Array<number>(gs.length).fill(0.0));
  for (let r = 0; r < m; r++) {
    if (!Number.isFinite(model.rowUpper[r]) || model.rowUpper[r] > 0.0 + 1e-9) {
      continue;
    }
    const ks = model.aStart[r];
    const ke = model.aStart[r + 1];
    let dv = -1;
    let tc = -1;
    let ncont = 0;
    let sid = -1;
    let ring = -1;
    let coef = 0.0;
    let ok = true;
    for (let k = ks; k < ke; k++) {
      const c = model.aIndex[k];
      const a = model.aValue[k];
      if (isIntegral(model.colType[c])) {
        if (!isX[c]) {
          ok = false;
          break;
        }
        const u = xUnit[c];
        if (u < 0) {
          ok = false;
          break;
        }
        const g = xGroup[c];
        if (g >= 0) {
          sid = gSess[g];
          ring = gRing[g];
          if (Math.abs(uDur[u]) > 1e-12) {
            coef = Math.abs(a / uDur[u]);
          }
        }
      } else {
        ncont += 1;
        if (a < 0.0) {
          // the one with `-` is `dev` unless it is the total column
          const isTotal = aTotal.some((t) => t === c);
          if (!isTotal) {
            dv = c;
          } else {
            tc = c;
          }
        } else {
          tc = c;
        }
      }
    }
    if (!ok || ncont !== 2 || dv < 0 || sid === -1 || ring === -1) {
      continue;
    }
    void tc;
    aDev[sid][ring] = dv;
    devCoef[sid][ring] = coef;
  }

  return {
    nsess,
    gFeeder,
    gSess,
    gCap,
    groups,
    uDur,
    xcol,
    zcol,
    ycol,
    xrOf,
    aTotal,
    aPeak: sessPeak,
    aMin,
    aRange,
    aDev,
    devCoef,
    cost: model.colCost.slice(),
  };
}

/** Build a full `x` from an assignment, then project the auxiliaries. */
function pFinalize(model: Model, sh: PShape, assign: readonly number[][]): number[] {
  const x = new Array<number>(model.numCol).fill(0.0);
  for (let u = 0; u < assign.length; u++) {
    for (let g = 0; g < sh.gFeeder.length; g++) {
      const a = assign[u][g];
      const xc = sh.xcol[u][g];
      if (xc < 0) continue;
      x[xc] = a;
      const zc = sh.zcol[u][g];
      if (zc >= 0) {
        x[zc] = a > 0.5 ? 1.0 : 0.0;
      }
      const r = sh.xrOf[xc];
      if (r >= 0) {
        x[r] = a;
      }
    }
    for (let s = 0; s < sh.nsess; s++) {
      const yc = sh.ycol[u][s];
      if (yc >= 0) {
        let yv = 0.0;
        for (const g of sh.groups[s]) {
          yv += assign[u][g];
        }
        x[yc] = yv;
      }
    }
  }
  const csc = Csc.build(model);
  projectContinuous(model, csc, x);
  return x;
}

/** Objective local search over the planning assignment. */
export function improvePlanning(
  model: Model,
  x0: readonly number[],
  clock: Clock,
  deadlineSecs: number,
  seed: number,
): [number, number[]] | null {
  return improvePlanningAdaptive(model, x0, clock, deadlineSecs, seed, true);
}

export function improvePlanningAdaptive(
  model: Model,
  x0: readonly number[],
  clock: Clock,
  deadlineSecs: number,
  seed: number,
  adaptive: boolean,
): [number, number[]] | null {
  if (x0.length !== model.numCol) {
    return null;
  }
  const blockStarted = clock.nowSecs();
  const blockX = improve(model, Array.from(x0), clock, deadlineSecs);
  if (blockX !== null) {
    return [model.objective(blockX), blockX];
  }
  const budget = deadlineSecs - (clock.nowSecs() - blockStarted);
  if (budget <= 0.0) {
    return null;
  }
  const csc = Csc.build(model);
  const sh = detectPlan(model, csc);
  if (sh === null) {
    return null;
  }
  const nu = sh.xcol.length;
  const ng = sh.gFeeder.length;

  // Seed the assignment from x0 (integers).
  const assign: number[][] = [];
  for (let u = 0; u < nu; u++) {
    assign.push(new Array<number>(ng).fill(0.0));
  }
  const load = new Array<number>(ng).fill(0.0);
  for (let u = 0; u < nu; u++) {
    for (let g = 0; g < ng; g++) {
      const xc = sh.xcol[u][g];
      if (xc >= 0) {
        assign[u][g] = fmax(Math.round(x0[xc]), 0.0);
      }
    }
  }
  for (let g = 0; g < ng; g++) {
    for (const [c, d] of sh.gFeeder[g]) {
      load[g] += d * fmax(Math.round(x0[c]), 0.0);
    }
  }

  // Objective of an assignment (x/z/y cost + per-session auxiliary cost).
  const assignmentObj = (assignA: readonly number[][], loadA: readonly number[]): number => {
    let obj = 0.0;
    for (let u = 0; u < nu; u++) {
      for (let g = 0; g < ng; g++) {
        const a = assignA[u][g];
        if (a === 0.0) continue;
        if (sh.xcol[u][g] >= 0) {
          obj += sh.cost[sh.xcol[u][g]] * a;
        }
        if (sh.zcol[u][g] >= 0 && a > 0.5) {
          obj += sh.cost[sh.zcol[u][g]];
        }
      }
      for (let s = 0; s < sh.nsess; s++) {
        const y = sh.ycol[u][s];
        if (y >= 0) {
          let yv = 0.0;
          for (const g of sh.groups[s]) {
            yv += assignA[u][g];
          }
          obj += sh.cost[y] * yv;
        }
      }
    }
    for (let s = 0; s < sh.nsess; s++) {
      obj += pSessCost(sh, s, (g) => loadA[g]);
    }
    return obj;
  };

  // Delta of moving `d` matches of unit `u` from group `g1` to `g2`.
  const moveDelta = (
    assignA: readonly number[][],
    loadA: readonly number[],
    u: number,
    g1: number,
    g2: number,
    d: number,
  ): number | null => {
    if (d <= 0.0 || g1 === g2) {
      return null;
    }
    const du = sh.uDur[u];
    if (loadA[g2] + du * d > sh.gCap[g2] + 1e-6) {
      return null;
    }
    // One ring per (unit, session).
    const s1 = sh.gSess[g1];
    const s2 = sh.gSess[g2];
    const a1 = assignA[u][g1];
    if (s1 === s2) {
      if (d < a1 - 1e-9) {
        return null;
      }
    } else {
      for (const gg of sh.groups[s2]) {
        if (gg !== g2 && assignA[u][gg] > 1e-9) {
          return null;
        }
      }
    }
    const x1 = sh.xcol[u][g1];
    const x2 = sh.xcol[u][g2];
    if (x1 < 0 || x2 < 0) {
      return null;
    }
    let delta = sh.cost[x1] * -d + sh.cost[x2] * d;
    const a1b = assignA[u][g1];
    const a2 = assignA[u][g2];
    const z1 = sh.zcol[u][g1];
    const z2 = sh.zcol[u][g2];
    if (z1 >= 0 && a1b - d <= 1e-9 && a1b > 1e-9) {
      delta -= sh.cost[z1];
    }
    if (z2 >= 0 && a2 <= 1e-9) {
      delta += sh.cost[z2];
    }
    const s1b = sh.gSess[g1];
    const s2b = sh.gSess[g2];
    if (s1b !== s2b) {
      const y1 = sh.ycol[u][s1b];
      const y2 = sh.ycol[u][s2b];
      if (y1 >= 0) {
        delta -= sh.cost[y1] * d;
      }
      if (y2 >= 0) {
        delta += sh.cost[y2] * d;
      }
    }
    const old1 = pSessCost(sh, s1, (g) => loadA[g]);
    const old2 = s1 !== s2 ? pSessCost(sh, s2, (g) => loadA[g]) : 0.0;
    const nl = (g: number): number => {
      if (g === g1) {
        return loadA[g1] - du * d;
      } else if (g === g2) {
        return loadA[g2] + du * d;
      } else {
        return loadA[g];
      }
    };
    const new1 = pSessCost(sh, s1, nl);
    const new2 = s1 !== s2 ? pSessCost(sh, s2, nl) : 0.0;
    delta += new1 - old1 + (new2 - old2);
    return delta;
  };

  let obj = assignmentObj(assign, load);
  const obj0 = obj;
  let bestObj = obj;
  let bestAssign = assign.map((row) => row.slice());
  const rng = new Rng(seed);
  const start = clock.nowSecs();
  let lastImprovement = start;
  const stallSeconds = clamp(budget * 0.15, 0.05, 1.0);
  let stall = 0;
  const stallLimit = 400;

  const order: number[] = [];
  for (let u = 0; u < nu; u++) order.push(u);
  while (clock.nowSecs() - start < budget) {
    if (adaptive && clock.nowSecs() - lastImprovement > stallSeconds) {
      break;
    }
    // Fisher-Yates shuffle for a randomized first-improvement scan.
    for (let i = order.length - 1; i >= 1; i--) {
      const j = Math.trunc(rng.nextF64() * (i + 1)) % (i + 1);
      const tmp = order[i];
      order[i] = order[j];
      order[j] = tmp;
    }
    let improved = false;
    for (const u of order) {
      const du = sh.uDur[u];
      // Candidate source groups (occupied) and destinations.
      const src: number[] = [];
      for (let g = 0; g < ng; g++) {
        if (assign[u][g] > 1e-9) {
          src.push(g);
        }
      }
      if (src.length === 0) {
        continue;
      }
      let best: [number, number, number, number] | null = null;
      for (const g1 of src) {
        const a1 = assign[u][g1];
        for (let g2 = 0; g2 < ng; g2++) {
          if (g2 === g1) {
            continue;
          }
          if (load[g2] + du * a1 > sh.gCap[g2] + 1e-6 && a1 > 0.0) {
            // full move may not fit; still try d=1 below
          }
          const ds: [number, number] = [a1, 1.0];
          for (const d of ds) {
            if (d <= 0.0 || d > a1 + 1e-9) {
              continue;
            }
            const delta = moveDelta(assign, load, u, g1, g2, d);
            if (
              delta !== null &&
              delta < -1e-9 &&
              (best === null || delta < best[3])
            ) {
              best = [g1, g2, d, delta];
            }
          }
        }
      }
      if (best !== null) {
        const [g1, g2, d, delta] = best;
        const du2 = sh.uDur[u];
        assign[u][g1] -= d;
        assign[u][g2] += d;
        load[g1] -= du2 * d;
        load[g2] += du2 * d;
        obj += delta;
        improved = true;
      }
    }
    if (improved) {
      if (obj < bestObj - 1e-9) {
        bestObj = obj;
        bestAssign = assign.map((row) => row.slice());
        lastImprovement = clock.nowSecs();
      }
      stall = 0;
    } else {
      // Kick: move a random unit with some matches to a random feasible group.
      let moved = false;
      for (let it = 0; it < 32; it++) {
        const u = Math.trunc(rng.nextF64() * nu) % Math.max(nu, 1);
        const src: number[] = [];
        for (let g = 0; g < ng; g++) {
          if (assign[u][g] > 1e-9) {
            src.push(g);
          }
        }
        if (src.length === 0) {
          continue;
        }
        const g1 = src[Math.trunc(rng.nextF64() * src.length) % src.length];
        const g2 = Math.trunc(rng.nextF64() * ng) % Math.max(ng, 1);
        if (g2 === g1) {
          continue;
        }
        const d = assign[u][g1];
        const delta = moveDelta(assign, load, u, g1, g2, d);
        if (delta !== null) {
          const du2 = sh.uDur[u];
          assign[u][g1!] -= d;
          assign[u][g2] += d;
          load[g1!] -= du2 * d;
          load[g2] += du2 * d;
          obj += delta;
          moved = true;
          break;
        }
      }
      if (moved) {
        if (obj < bestObj - 1e-9) {
          bestObj = obj;
          bestAssign = assign.map((row) => row.slice());
          lastImprovement = clock.nowSecs();
        }
      } else {
        break;
      }
      stall += 1;
      if (stall > stallLimit) {
        break;
      }
    }
  }

  void obj0;
  void bestObj;
  const x = pFinalize(model, sh, bestAssign);
  const chk = checkSolution(model, x, TOL);
  if (chk.feasible) {
    return [chk.objective, x];
  }
  return null;
}

/**
 * Set each continuous variable to the smallest value satisfying its rows.
 */
function projectContinuous(model: Model, csc: Csc, x: number[]): void {
  const n = model.numCol;
  // `hasPure[c]`: c appears in some row that contains no other continuous col.
  const hasPure = new Array<boolean>(n).fill(false);
  for (let c = 0; c < n; c++) {
    if (isIntegral(model.colType[c])) continue;
    const cs = csc.start[c];
    const ce = csc.start[c + 1];
    for (let kk = cs; kk < ce; kk++) {
      const r = csc.index[kk];
      let otherCont = false;
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const cc = model.aIndex[k];
        if (cc !== c && !isIntegral(model.colType[cc])) {
          otherCont = true;
        }
      }
      if (!otherCont) {
        hasPure[c] = true;
      }
    }
  }
  // Phase A: definition aux (total/peak/min) from their pure rows.
  for (let pass = 0; pass < 3; pass++) {
    for (let c = 0; c < n; c++) {
      if (!isIntegral(model.colType[c]) && hasPure[c]) {
        projectOne(model, csc, x, c, true);
      }
    }
  }
  // Phase B: dependent aux (range/dev) from all rows.
  for (let pass = 0; pass < 3; pass++) {
    for (let c = 0; c < n; c++) {
      if (!isIntegral(model.colType[c]) && !hasPure[c]) {
        projectOne(model, csc, x, c, false);
      }
    }
  }
}

function projectOne(
  model: Model,
  csc: Csc,
  x: number[],
  c: number,
  onlyPure: boolean,
): void {
  let lo = model.colLower[c];
  let hi = model.colUpper[c];
  if (lo <= -INF_HALF) lo = -Infinity;
  if (hi >= INF_HALF) hi = Infinity;
  let needLo = lo;
  let needHi = hi;
  const cs = csc.start[c];
  const ce = csc.start[c + 1];
  for (let kk = cs; kk < ce; kk++) {
    const r = csc.index[kk];
    const a = csc.value[kk];
    if (a === 0.0) continue;
    let other = 0.0;
    let hasOtherCont = false;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const cc = model.aIndex[k];
      if (cc !== c) {
        other += model.aValue[k] * x[cc];
        if (!isIntegral(model.colType[cc])) {
          hasOtherCont = true;
        }
      }
    }
    if (onlyPure && hasOtherCont) {
      continue;
    }
    // Row bounds carry the same +/-`INF` sentinel as column bounds.
    const rl = model.rowLower[r] <= -INF_HALF ? -Infinity : model.rowLower[r];
    const ru = model.rowUpper[r] >= INF_HALF ? Infinity : model.rowUpper[r];
    let l: number;
    let h: number;
    if (a > 0.0) {
      l = (rl - other) / a;
      h = (ru - other) / a;
    } else {
      l = (ru - other) / a;
      h = (rl - other) / a;
    }
    if (Number.isFinite(l)) {
      needLo = fmax(needLo, l);
    }
    if (Number.isFinite(h)) {
      needHi = fmin(needHi, h);
    }
  }
  // `needHi` starts at the column's own upper bound; it is only a "row" upper
  // bound when a row actually tightened it below that.
  const rowUpperOnly =
    Number.isFinite(needHi) && (isInf(hi) || needHi < hi - 1e-9 * (1.0 + Math.abs(hi)));
  if (Number.isFinite(needLo)) {
    const target = rowUpperOnly && needLo <= lo + 1e-9 ? needHi : needLo;
    let v = fmin(fmax(target, lo), hi);
    if (Number.isFinite(needHi)) {
      v = fmin(v, needHi);
    }
    x[c] = v;
  }
}
