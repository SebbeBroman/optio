#!/usr/bin/env node
// Differential harness: generate random models, solve each with the Rust
// `optio-bench` oracle and the TypeScript port (`src/cli.ts`), and compare the
// reported status / objective / dual bound and feasibility.
//
// Usage:
//   node tools/differential.mjs [--count 40] [--seed 1] [--mode lp|mip|both] \
//        [--rust target/release/optio-bench] [--time 5]
//
// The TS side is run with the same Node that runs this script; the Rust side is
// the prebuilt bench binary (build with `cargo build -p optio-bench --release`).

import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const argv = process.argv.slice(2);
function opt(name, dflt) {
  const i = argv.indexOf(name);
  return i >= 0 && i + 1 < argv.length ? argv[i + 1] : dflt;
}
const COUNT = Number(opt("--count", "40"));
const SEED = Number(opt("--seed", "1"));
const MODE = opt("--mode", "both");
const RUST = opt("--rust", "target/release/optio-bench");
const TIME = opt("--time", "5");
const TOL_ABS = Number(opt("--tol-abs", "1e-4"));
const TOL_REL = Number(opt("--tol-rel", "1e-5"));

// xorshift64* PRNG -> deterministic float in [0,1)
let s = (BigInt(SEED) * 0x2545f4914f6cdd1dn + 0x9e3779b97f4a7c15n) & ((1n << 64n) - 1n);
function rnd() {
  s ^= (s << 13n) & ((1n << 64n) - 1n);
  s ^= s >> 7n;
  s ^= (s << 17n) & ((1n << 64n) - 1n);
  s &= (1n << 64n) - 1n;
  return Number(s >> 11n) / 9007199254740992;
}
function ri(lo, hi) {
  return lo + Math.floor(rnd() * (hi - lo + 1));
}
function pick(arr) {
  return arr[ri(0, arr.length - 1)];
}

const INF = 1e30;

function genModel(milp) {
  const numCols = ri(3, 10);
  const numRows = ri(2, 8);
  const colCost = [];
  const colLower = [];
  const colUpper = [];
  const integrality = [];
  for (let c = 0; c < numCols; c++) {
    colCost.push(Math.round((rnd() * 10 - 5) * 10) / 10);
    const lo = pick([0, 0, -2, -5]);
    const hi = pick([1, 3, 5, 10, INF]);
    colLower.push(lo);
    colUpper.push(hi);
    integrality.push(milp ? pick([0, 0, 1, 2, 2]) : 0);
  }
  const starts = [0];
  const indices = [];
  const values = [];
  const rowLower = [];
  const rowUpper = [];
  for (let r = 0; r < numRows; r++) {
    const k = ri(1, Math.min(4, numCols));
    const used = new Set();
    for (let t = 0; t < k; t++) {
      const j = ri(0, numCols - 1);
      if (used.has(j)) continue;
      used.add(j);
      indices.push(j);
      values.push(Math.round((rnd() * 10 - 5) * 10) / 10);
    }
    starts.push(indices.length);
    const rhs = Math.round(rnd() * 20 * 10) / 10;
    const kind = pick(["le", "ge", "eq", "range"]);
    if (kind === "le") {
      rowLower.push(-INF);
      rowUpper.push(rhs);
    } else if (kind === "ge") {
      rowLower.push(rhs);
      rowUpper.push(INF);
    } else if (kind === "eq") {
      rowLower.push(rhs);
      rowUpper.push(rhs);
    } else {
      rowLower.push(rhs - 2);
      rowUpper.push(rhs + 2);
    }
  }
  return {
    numCols,
    numRows,
    sense: 1,
    offset: 0,
    colCost,
    colLower,
    colUpper,
    rowLower,
    rowUpper,
    integrality,
    matrix: { starts, indices, values },
  };
}

const dir = mkdtempSync(join(tmpdir(), "optio-diff-"));
let failures = 0;
let compared = 0;

function runRust(file, lp) {
  const args = ["--model-json", file, "--time", TIME, "--threads", "1"];
  if (lp) args.push("--lp");
  const out = execFileSync(resolve(RUST), args, { encoding: "utf8" });
  return JSON.parse(out.trim());
}
function runTs(file, lp) {
  const args = ["src/cli.ts", file, "--time", TIME, "--threads", "1"];
  if (lp) args.push("--lp");
  const out = execFileSync(process.execPath, args, { encoding: "utf8", cwd: new URL("..", import.meta.url).pathname });
  return JSON.parse(out.trim());
}

function close(a, b) {
  const d = Math.abs(a - b);
  return d <= TOL_ABS + TOL_REL * Math.abs(b);
}

const modes = MODE === "both" ? ["lp", "mip"] : [MODE];
for (const mode of modes) {
  for (let i = 0; i < COUNT; i++) {
    const model = genModel(mode === "mip");
    const file = join(dir, `${mode}-${i}.json`);
    writeFileSync(file, JSON.stringify(model));
    let rust;
    let ts;
    try {
      rust = runRust(file, mode === "lp");
      ts = runTs(file, mode === "lp");
    } catch (e) {
      console.log(`#${i} ${mode} ERROR ${e.message}`);
      failures += 1;
      continue;
    }
    compared += 1;
    const problems = [];
    if (rust.statusCode !== ts.statusCode) {
      problems.push(`status ${rust.statusCode} vs ${ts.statusCode}`);
    }
    if (rust.objective !== null && ts.objective !== null) {
      if (!close(ts.objective, rust.objective)) {
        problems.push(`objective ${ts.objective} vs ${rust.objective}`);
      }
    } else if ((rust.objective === null) !== (ts.objective === null)) {
      problems.push(`objective presence ${ts.objective} vs ${rust.objective}`);
    }
    if (rust.dualBound !== null && ts.dualBound !== null) {
      // The TS bound must not be materially weaker (higher, for minimisation).
      if (ts.dualBound > rust.dualBound + TOL_ABS + TOL_REL * Math.abs(rust.dualBound)) {
        problems.push(`dualBound ${ts.dualBound} vs ${rust.dualBound}`);
      }
    }
    if (ts.maxRowViolation !== null && ts.maxRowViolation > 1e-5) {
      problems.push(`ts infeasible: maxRowViolation ${ts.maxRowViolation}`);
    }
    if (problems.length > 0) {
      failures += 1;
      console.log(`#${i} ${mode} MISMATCH (${file}): ${problems.join("; ")}`);
    }
  }
}

console.log(`differential: ${compared} compared, ${failures} failures (seed ${SEED}, mode ${MODE})`);
process.exit(failures === 0 ? 0 : 1);
