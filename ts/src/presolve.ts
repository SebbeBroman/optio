// Exact propagation of binary cardinality rows. No objective or row is removed,
// so the original solution coordinates and independent checker remain valid.
// (Port of `crates/core/src/presolve.rs`.)

import { Model, INF, VAR_INTEGER, isIntegral, filled } from "./model.ts";
import { nextUp, nextDown } from "./f64bits.ts";
import type { Clock } from "./clock.ts";

/** Rust `f64::fract`. */
function fract(x: number): number {
  return x - Math.trunc(x);
}

export function propagate(model: Model, clock: Clock, deadline: number): Model | null {
  const reduced = model.clone();
  // Only integer unit-coefficient rows with binary domains are eligible.
  const seen: number[] = filled<number>(model.numCol, -1);
  const rows: number[] = [];
  for (let r = 0; r < model.numRow; r++) {
    let ok = true;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const j = model.aIndex[k];
      if (
        model.aValue[k] === 1.0 &&
        isIntegral(model.colType[j]) &&
        model.colLower[j] >= 0.0 &&
        model.colUpper[j] <= 1.0 &&
        seen[j] !== r
      ) {
        seen[j] = r;
      } else {
        ok = false;
        break;
      }
    }
    if (ok) rows.push(r);
  }
  for (let j = 0; j < model.numCol; j++) {
    if (isIntegral(reduced.colType[j])) {
      reduced.colLower[j] = Math.ceil(reduced.colLower[j]);
      reduced.colUpper[j] = Math.floor(reduced.colUpper[j]);
      if (reduced.colLower[j] > reduced.colUpper[j]) {
        return null;
      }
    }
  }
  for (let iter = 0; iter < 32; iter++) {
    if (clock.nowSecs() >= deadline) {
      break;
    }
    let changed = false;
    for (let ri = 0; ri < rows.length; ri++) {
      const r = rows[ri];
      let min = 0;
      let max = 0;
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const j = model.aIndex[k];
        min += reduced.colLower[j];
        max += reduced.colUpper[j];
      }
      const lo = Math.ceil(model.rowLower[r]);
      const hi = Math.floor(model.rowUpper[r]);
      if (min > hi || max < lo) {
        return null;
      }
      for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
        const j = model.aIndex[k];
        if (reduced.colLower[j] === reduced.colUpper[j]) {
          continue;
        }
        if (min === hi) {
          reduced.colUpper[j] = 0.0;
          changed = true;
        } else if (max === lo) {
          reduced.colLower[j] = 1.0;
          changed = true;
        }
      }
    }
    if (!changed && !arraysEqual(reduced.colLower, reduced.colUpper)) {
      const w = propagateWeighted(reduced);
      if (w.err) {
        return null;
      }
      if (w.changed) {
        changed = true;
      }
    }
    if (!changed) {
      break;
    }
  }
  return reduced;
}

/**
 * Conservative interval propagation. Rows with duplicate entries are skipped
 * before deriving bounds, and outward margins protect floating-point
 * cancellation.
 */
function propagateWeighted(model: Model): { changed: boolean; err: boolean } {
  let changed = false;
  const n = model.numCol;
  // Reusable scratch: a per-column coefficient accumulator plus a generation
  // stamp, so duplicate entries are detected in O(deg) with no per-row
  // allocation and no sort.
  const coef = new Float64Array(n);
  const stamp = new Int32Array(n);
  const rowList: number[] = [];
  let gen = 0;
  for (let r = 0; r < model.numRow; r++) {
    gen += 1;
    rowList.length = 0;
    let dup = false;
    let nz = 0;
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      if (stamp[c] === gen) {
        dup = true;
        break;
      }
      stamp[c!] = gen;
      const a = model.aValue[k];
      coef[c!] = a;
      rowList.push(c);
      if (a !== 0) nz += 1;
    }
    if (dup) {
      continue;
    }
    let min = 0;
    let max = 0;
    let magnitude = 1.0;
    let minInf = 0;
    let maxInf = 0;
    const count = rowList.length;
    for (let t = 0; t < count; t++) {
      const j = rowList[t];
      const a = coef[j];
      if (a === 0) continue;
      const lo = a > 0.0 ? model.colLower[j] : model.colUpper[j];
      const hi = a > 0.0 ? model.colUpper[j] : model.colLower[j];
      const l = a * lo;
      const u = a * hi;
      if (isFinite(l) && Math.abs(l) < INF) {
        min += l;
        magnitude += Math.abs(l);
      } else {
        minInf += 1;
      }
      if (isFinite(u) && Math.abs(u) < INF) {
        max += u;
        magnitude += Math.abs(u);
      } else {
        maxInf += 1;
      }
    }
    const error = 1e-7 + 64.0 * Number.EPSILON * (nz + 1) * magnitude;
    if ((minInf === 0 && min - error > model.rowUpper[r]) || (maxInf === 0 && max + error < model.rowLower[r])) {
      return { changed: false, err: true };
    }
    for (let t = 0; t < count; t++) {
      const j = rowList[t];
      const a = coef[j];
      if (a === 0) continue;
      const l = a * (a > 0.0 ? model.colLower[j] : model.colUpper[j]);
      const u = a * (a > 0.0 ? model.colUpper[j] : model.colLower[j]);
      const lfinite = isFinite(l) && Math.abs(l) < INF;
      const ufinite = isFinite(u) && Math.abs(u) < INF;
      let lower = model.colLower[j];
      let upper = model.colUpper[j];
      if (minInf === (lfinite ? 0 : 1) && model.rowUpper[r] < INF) {
        const rest = min - (lfinite ? l : 0.0);
        const bound = (model.rowUpper[r] - rest + error) / a;
        if (a > 0.0) {
          upper = Math.min(upper, nextUp(bound));
        } else {
          lower = Math.max(lower, nextDown(bound));
        }
      }
      if (maxInf === (ufinite ? 0 : 1) && model.rowLower[r] > -INF) {
        const rest = max - (ufinite ? u : 0.0);
        const bound = (model.rowLower[r] - rest - error) / a;
        if (a > 0.0) {
          lower = Math.max(lower, nextDown(bound));
        } else {
          upper = Math.min(upper, nextUp(bound));
        }
      }
      if (isIntegral(model.colType[j])) {
        lower = Math.ceil(lower);
        upper = Math.floor(upper);
      }
      if (lower > upper) {
        return { changed: false, err: true };
      }
      const tol = isIntegral(model.colType[j]) ? 0.0 : 1e-9 * (1.0 + Math.min(Math.abs(lower), Math.abs(upper)));
      if (lower > model.colLower[j] + tol) {
        model.colLower[j] = lower;
        changed = true;
      }
      if (upper < model.colUpper[j] - tol) {
        model.colUpper[j] = upper;
        changed = true;
      }
    }
  }
  return { changed, err: false };
}

function arraysEqual(a: readonly number[], b: readonly number[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

/**
 * A checked incumbent permits objective-domain bounds: every improving point
 * satisfies c*x <= incumbent.
 */
function objectiveBounds(model: Model, incumbent: number): void {
  const terms: number[] = filled<number>(model.numCol, 0);
  let min = 0;
  let magnitude = 1.0;
  for (let j = 0; j < model.numCol; j++) {
    const c = model.colCost[j];
    if (c === 0.0) continue;
    const b = c > 0.0 ? model.colLower[j] : model.colUpper[j];
    if (Math.abs(b) >= INF || !isFinite(b)) {
      return;
    }
    terms[j] = c * b;
    min += terms[j];
    magnitude += Math.abs(terms[j]);
  }
  const error = 64.0 * Number.EPSILON * (model.numCol + 1) * (magnitude + Math.abs(incumbent));
  for (let j = 0; j < model.numCol; j++) {
    const c = model.colCost[j];
    if (c === 0.0) continue;
    const b = (incumbent - min + terms[j] + error) / c;
    if (c > 0.0) {
      model.colUpper[j] = Math.min(model.colUpper[j], nextUp(b));
    } else {
      model.colLower[j] = Math.max(model.colLower[j], nextDown(b));
    }
  }
}

/**
 * Promote integer definitions and objective auxiliaries whose favorable
 * rounding direction preserves feasibility.
 */
function inferIntegrality(model: Model, original: Model): void {
  const rowsR: number[][] = new Array(model.numCol);
  const rowsA: number[][] = new Array(model.numCol);
  for (let j = 0; j < model.numCol; j++) {
    rowsR[j] = [];
    rowsA[j] = [];
  }
  for (let r = 0; r < model.numRow; r++) {
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      rowsR[c].push(r);
      rowsA[c].push(model.aValue[k]);
    }
  }

  const integerRest = (r: number, j: number): boolean => {
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      const c = model.aIndex[k];
      if (
        c === j ||
        (fract(model.aValue[k]) === 0.0 &&
          Math.abs(model.aValue[k]) <= 1e9 &&
          (isIntegral(model.colType[c]) ||
            (model.colLower[c] === model.colUpper[c] && fract(model.colLower[c]) === 0.0)))
      ) {
        continue;
      }
      return false;
    }
    return true;
  };

  for (let iter = 0; iter < 8; iter++) {
    let changed = false;
    for (let j = 0; j < model.numCol; j++) {
      const rr = rowsR[j];
      const ra = rowsA[j];
      if (isIntegral(model.colType[j]) || rr.length === 0 || hasDuplicateRows(rr)) {
        continue;
      }
      let defined = false;
      for (let t = 0; t < rr.length; t++) {
        const r = rr[t];
        const a = ra[t];
        if (
          Math.abs(a) === 1.0 &&
          model.rowLower[r] === model.rowUpper[r] &&
          fract(model.rowLower[r]) === 0.0 &&
          integerRest(r, j)
        ) {
          defined = true;
          break;
        }
      }
      const roundingSafe = (down: boolean): boolean => {
        const base = down ? original.colLower[j] : original.colUpper[j];
        if (fract(base) !== 0.0) return false;
        for (let t = 0; t < rr.length; t++) {
          const r = rr[t];
          const a = ra[t];
          if (a === 0.0) continue;
          const delta = down ? -a : a;
          const hurtsUpper = delta > 0.0 && model.rowUpper[r] < INF;
          const hurtsLower = delta < 0.0 && model.rowLower[r] > -INF;
          if (!hurtsUpper && !hurtsLower) continue;
          if (
            !(
              Math.abs(a) === 1.0 &&
              integerRest(r, j) &&
              (!hurtsUpper || fract(model.rowUpper[r]) === 0.0) &&
              (!hurtsLower || fract(model.rowLower[r]) === 0.0)
            )
          ) {
            return false;
          }
        }
        return true;
      };
      if (defined || (model.colCost[j] >= 0.0 && roundingSafe(true)) || (model.colCost[j] <= 0.0 && roundingSafe(false))) {
        model.colType[j] = VAR_INTEGER;
        model.colLower[j] = Math.ceil(model.colLower[j]);
        model.colUpper[j] = Math.floor(model.colUpper[j]);
        changed = true;
      }
    }
    if (!changed) break;
  }
}

function hasDuplicateRows(rr: number[]): boolean {
  for (let i = 0; i + 1 < rr.length; i++) {
    if (rr[i] === rr[i + 1]) return true;
  }
  return false;
}

function gcdNum(aIn: number, bIn: number): number {
  let a = aIn;
  let b = bIn;
  while (b !== 0) {
    const t = a % b;
    a = b;
    b = t;
  }
  return a;
}

/**
 * Small root-only integer rounding/cover cut pool. All coefficients used for
 * rounding are exact small integers; nonnegative domains make floor rounding
 * valid. Returns the strengthened model and the number of cuts added.
 */
export function strengthen(model: Model, original: Model, incumbent: number | null): [Model, number] {
  const out = model.clone();
  if (incumbent !== null) {
    objectiveBounds(out, incumbent);
  }
  inferIntegrality(out, original);
  let count = 0;
  for (let r = 0; r < model.numRow; r++) {
    if (count >= 32) break;
    const eIdx: number[] = [];
    const eVal: number[] = [];
    for (let k = model.aStart[r]; k < model.aStart[r + 1]; k++) {
      eIdx.push(model.aIndex[k]);
      eVal.push(model.aValue[k]);
    }
    let allInt = eIdx.length > 0;
    for (let t = 0; t < eIdx.length; t++) {
      const j = eIdx[t];
      const a = eVal[t];
      if (!(isIntegral(out.colType[j]) && fract(a) === 0.0 && Math.abs(a) <= 1e9 && a !== 0.0)) {
        allInt = false;
        break;
      }
    }
    if (!allInt) {
      continue;
    }
    let g = 0;
    for (let t = 0; t < eIdx.length; t++) {
      g = gcdNum(g, Math.abs(eVal[t]));
    }
    const gd = g;
    if (out.rowLower[r] > -INF) {
      out.rowLower[r] = Math.ceil(nextDown(out.rowLower[r] / gd)) * gd;
    }
    if (out.rowUpper[r] < INF) {
      out.rowUpper[r] = Math.floor(nextUp(out.rowUpper[r] / gd)) * gd;
    }
    const rhs = model.rowUpper[r];
    let allPosNonneg = true;
    for (let t = 0; t < eIdx.length; t++) {
      if (!(eVal[t] > 0.0 && model.colLower[eIdx[t]] >= 0.0)) {
        allPosNonneg = false;
        break;
      }
    }
    if (rhs < 0.0 || rhs >= INF || !allPosNonneg) {
      continue;
    }
    let minA = Infinity;
    for (let t = 0; t < eIdx.length; t++) {
      minA = Math.min(minA, eVal[t]);
    }
    const divisors = [2, Math.max(minA, 2)];
    for (let di = 0; di < divisors.length; di++) {
      const divisor = divisors[di];
      const rowIdx: number[] = [];
      const rowVal: number[] = [];
      for (let t = 0; t < eIdx.length; t++) {
        const v = Math.floor(eVal[t] / divisor);
        if (v > 0.0) {
          rowIdx.push(eIdx[t]);
          rowVal.push(v);
        }
      }
      const bound = Math.floor(nextUp(rhs / divisor));
      let allDivisible = true;
      for (let t = 0; t < eIdx.length; t++) {
        if (eVal[t] % divisor !== 0) {
          allDivisible = false;
          break;
        }
      }
      if (rowIdx.length === 0 || allDivisible) {
        continue;
      }
      const entries: [number, number][] = [];
      for (let t = 0; t < rowIdx.length; t++) entries.push([rowIdx[t], rowVal[t]]);
      out.addRow(-INF, bound, entries);
      count += 1;
    }
    let allBinary = true;
    for (let t = 0; t < eIdx.length; t++) {
      if (model.colUpper[eIdx[t]] > 1.0) {
        allBinary = false;
        break;
      }
    }
    if (allBinary) {
      const order: number[] = [];
      for (let t = 0; t < eIdx.length; t++) order.push(t);
      order.sort((x, y) => eVal[y] - eVal[x]);
      let weight = 0.0;
      const covIdx: number[] = [];
      for (let oi = 0; oi < order.length; oi++) {
        const t = order[oi];
        weight += eVal[t];
        covIdx.push(eIdx[t]);
        if (weight > rhs) break;
      }
      const ids = covIdx.slice().sort((a, b) => a - b);
      let unique = true;
      for (let i = 0; i + 1 < ids.length; i++) {
        if (ids[i] === ids[i + 1]) {
          unique = false;
          break;
        }
      }
      if (weight > rhs && unique && ids.length === covIdx.length) {
        const entries: [number, number][] = [];
        for (let t = 0; t < covIdx.length; t++) entries.push([covIdx[t], 1.0]);
        out.addRow(-INF, covIdx.length - 1.0, entries);
        count += 1;
      }
    }
  }
  return [out, count];
}
