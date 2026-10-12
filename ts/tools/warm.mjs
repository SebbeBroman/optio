// Warm min-of-N solver time for the Rust wasm package, for comparison with the
// warm TS `tools/timing.ts` numbers (the wasm is AOT, so its run() time is
// stable; this just removes the first-run page-fault noise).
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { performance } from "node:perf_hooks";

const require = createRequire(import.meta.url);
const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const mod = require(join(repo, "pkg", "optio_wasm.js"));
const REPS = Number(process.env.REPS ?? 5);
const files = process.argv.slice(2);

for (const file of files) {
  const d = JSON.parse(readFileSync(file, "utf8"));
  const n = d.numCols;
  let best = Infinity;
  let obj = 0;
  for (let rep = 0; rep < REPS; rep++) {
    const model = new mod.Model();
    const cost = new Float64Array(n);
    const integ = new Int32Array(n);
    for (let c = 0; c < n; c++) {
      cost[c] = d.colCost[c];
      integ[c] = d.integrality[c] | 0;
    }
    model.add_model(
      n, cost,
      Float64Array.from(d.colLower), Float64Array.from(d.colUpper), integ,
      Float64Array.from(d.rowLower), Float64Array.from(d.rowUpper),
      Uint32Array.from(d.matrix.starts), Uint32Array.from(d.matrix.indices), Float64Array.from(d.matrix.values),
    );
    model.set_time_limit(10);
    if (typeof model.set_mip_rel_gap === "function") model.set_mip_rel_gap(1e-4);
    const t0 = performance.now();
    model.run();
    best = Math.min(best, performance.now() - t0);
    obj = model.get_objective_value();
    model.free?.();
  }
  console.log(`${file.split("/").pop().replace(".model.json", "")}  rust-wasm-min=${best.toFixed(1)}ms  obj=${obj.toFixed(4)}`);
}
