// Three-way comparison: Rust native, Rust wasm (wasm-bindgen), and the
// TypeScript engine on Node (V8). All solve the same detached model JSON. We
// report the solver time each engine measures internally (excluding
// process/startup and model construction), plus an independent feasibility
// check of every returned point via the shared checker.
//
// Usage: node tools/compare.mjs [model.json ...]
//   env TIME=10 GAP=1e-4
// Prereqs:
//   - ../target/release/optio-bench            (cargo build -p optio-bench --release)
//   - ../pkg/optio_wasm.js                     (crates/wasm/build.sh)
import { execFileSync } from "node:child_process";
import { readFileSync, existsSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { performance } from "node:perf_hooks";

const here = dirname(fileURLToPath(import.meta.url));
const tsDir = join(here, "..");
const repo = join(tsDir, "..");
const require = createRequire(import.meta.url);

const { checkModel } = await import(join(repo, "harness", "src", "checker.mjs"));

const TIME = Number(process.env.TIME ?? 10);
const GAP = Number(process.env.GAP ?? 1e-4);
const RUST_NATIVE = process.env.RUST_NATIVE ?? join(repo, "target", "release", "optio-bench");
const RUST_WASM = process.env.RUST_WASM ?? join(repo, "pkg", "optio_wasm.js");

const defaultModels = [
  join(repo, "harness", "fixtures", "planning-app-small.model.json"),
  join(repo, "harness", "fixtures", "planning-app-medium.model.json"),
  join(repo, "harness", "fixtures", "planning-app-large.model.json"),
];
const models = process.argv.slice(2).length ? process.argv.slice(2) : defaultModels;

const pad = (s, n) => String(s).padEnd(n);
const padL = (s, n) => String(s).padStart(n);
const num = (v, d = 1) => (Number.isFinite(v) ? v.toFixed(d) : String(v));

function lastJson(out) {
  const lines = out.split(/\r?\n/).filter((l) => l.trim().startsWith("{"));
  return JSON.parse(lines[lines.length - 1]);
}

function runProcess(cmd, args) {
  const t0 = performance.now();
  const out = execFileSync(cmd, args, { encoding: "utf8", maxBuffer: 1 << 30, cwd: tsDir, stdio: ["ignore", "pipe", "pipe"] });
  return { json: lastJson(out), wallMs: performance.now() - t0 };
}

function rustNative(file) {
  const { json, wallMs } = runProcess(RUST_NATIVE, ["--model-json", file, "--time", String(TIME), "--gap", String(GAP), "--threads", "1"]);
  return { statusCode: json.statusCode, objective: json.objective, ms: json.ms, wallMs, colValue: json.colValue ?? [] };
}

function tsNode(file) {
  const { json, wallMs } = runProcess(process.execPath, [join(tsDir, "src", "cli.ts"), file, "--time", String(TIME), "--gap", String(GAP), "--threads", "1"]);
  return { statusCode: json.statusCode, objective: json.objective, ms: json.ms, wallMs, colValue: json.colValue ?? [] };
}

let rustWasmMod = null;
function rustWasm(detached) {
  if (rustWasmMod === null) rustWasmMod = require(RUST_WASM);
  const mod = rustWasmMod;
  const sign = detached.sense !== undefined && detached.sense < 0 ? -1 : 1;
  const n = detached.numCols;
  const model = new mod.Model();
  try {
    const cost = new Float64Array(n);
    const integ = new Int32Array(n);
    for (let c = 0; c < n; c++) {
      cost[c] = sign * detached.colCost[c];
      integ[c] = detached.integrality[c] | 0;
    }
    const { starts, indices, values } = detached.matrix;
    const tBuild = performance.now();
    model.add_model(
      n,
      cost,
      Float64Array.from(detached.colLower),
      Float64Array.from(detached.colUpper),
      integ,
      Float64Array.from(detached.rowLower),
      Float64Array.from(detached.rowUpper),
      Uint32Array.from(starts),
      Uint32Array.from(indices),
      Float64Array.from(values),
    );
    const buildMs = performance.now() - tBuild;
    model.set_time_limit(TIME);
    if (typeof model.set_mip_rel_gap === "function") model.set_mip_rel_gap(GAP);
    const t0 = performance.now();
    model.run();
    const ms = performance.now() - t0;
    const view = typeof model.get_solution_object === "function" ? model.get_solution_object().colValue : model.get_solution();
    return { statusCode: model.get_model_status(), objective: sign * model.get_objective_value(), ms, wallMs: ms + buildMs, colValue: Array.from(view) };
  } finally {
    model.free?.();
  }
}

console.log(`rust-native: ${RUST_NATIVE}`);
console.log(`rust-wasm:   ${RUST_WASM}`);
console.log(`ts-node:     node src/cli.ts\n`);
console.log(
  pad("model", 22) + pad("engine", 12) +
  padL("status", 7) + padL("objective", 16) + padL("solver_ms", 11) + padL("wall_ms", 10) + padL("feas", 6),
);
console.log("-".repeat(84));

const summary = [];
for (const file of models) {
  if (!existsSync(file)) {
    console.log(pad(file, 22) + "MISSING");
    continue;
  }
  const detached = JSON.parse(readFileSync(file, "utf8"));
  const name = file.split("/").pop().replace(".model.json", "");
  const rows = [];
  const engines = [
    ["rust-native", () => rustNative(file)],
    ["rust-wasm", () => rustWasm(detached)],
    ["ts-node", () => tsNode(file)],
  ];
  for (const [label, fn] of engines) {
    let r;
    try {
      r = fn();
    } catch (e) {
      console.log(pad(name, 22) + pad(label, 12) + "ERROR " + String(e.message).split("\n")[0]);
      continue;
    }
    const feas =
      r.colValue.length >= detached.numCols
        ? checkModel(detached, r.colValue, 1e-6).feasible
          ? "yes"
          : "NO"
        : "n/a";
    rows.push({ label, ...r, feas });
    console.log(
      pad(name, 22) + pad(label, 12) +
      padL(r.statusCode, 7) + padL(num(r.objective, 4), 16) + padL(num(r.ms, 1), 11) + padL(num(r.wallMs, 1), 10) + padL(feas, 6),
    );
  }
  summary.push({ name, rows });
  console.log();
}

console.log("=".repeat(84));
for (const { name, rows } of summary) {
  if (rows.length === 0) continue;
  const objs = rows.filter((r) => Number.isFinite(r.objective)).map((r) => r.objective);
  const spread = objs.length ? Math.max(...objs) - Math.min(...objs) : NaN;
  const feasible = rows.filter((r) => r.feas === "NO").length;
  const fastest = rows.reduce((a, b) => (b.ms < a.ms ? b : a), rows[0]);
  console.log(
    pad(name, 22) +
    `obj-spread=${num(spread, 4)}  infeasible=${feasible}  fastest=${fastest.label} (${num(fastest.ms, 1)}ms)`,
  );
}
