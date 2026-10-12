// Time optio-wasm vs highs-js (both WebAssembly, same Node runtime) on the
// planning + LP fixtures, with an independent feasibility check of optio's
// solution. Reports the solver time (`run()`) only, so model construction and
// the checker are excluded.
//
// Prereqs:  crates/wasm/build.sh   (produces pkg/optio_wasm.js)
// Usage:    node tools/wasm-bench.mjs [file.lp ...]
// Env:      TIME_LIMIT=<secs>   override the per-fixture limit
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { performance } from 'node:perf_hooks';

import { loadHighs, highsSource } from '../src/highs.mjs';
import { checkModel, statusName } from '../src/checker.mjs';
import { loadOptio } from '../src/optio.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const fixtures = join(here, '..', 'fixtures');
const TIME = Number(process.env.TIME_LIMIT ?? 0);

const argFiles = process.argv.slice(2);
const DEFAULT = [
  'lp/lp-11.lp',
  'planning-s.lp',
  'planning-faithful.lp',
  'planning-tight.lp',
  'planning-m.lp',
  'planning-l.lp',
];
const files = argFiles.length ? argFiles : DEFAULT;
const pad = (s, n) => String(s).padEnd(n);
const padL = (s, n) => String(s).padStart(n);
const num = (v, d = 2) => (Number.isFinite(v) ? v.toFixed(d) : 'n/a');

/** Build + solve `detached` (a highs CSR model) with optio-wasm. */
function runOptio(mod, detached, seconds) {
  const sign = detached.sense !== undefined && detached.sense < 0 ? -1 : 1;
  const model = new mod.Model();
  try {
    const n = detached.numCols;
    const { starts, indices, values } = detached.matrix;
    const tBuild = performance.now();
    if (typeof model.add_model === 'function') {
      // Bulk CSR load: one call instead of n + rows JS↔wasm calls.
      const cost = new Float64Array(n);
      const integ = new Int32Array(n);
      for (let c = 0; c < n; c++) {
        cost[c] = sign * detached.colCost[c];
        integ[c] = detached.integrality[c] | 0;
      }
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
    } else {
      for (let c = 0; c < n; c++) model.add_var(detached.colLower[c], detached.colUpper[c]);
      for (let r = 0; r < detached.numRows; r++) {
        const idx = [];
        const val = [];
        for (let k = starts[r]; k < starts[r + 1]; k++) {
          idx.push(indices[k]);
          val.push(values[k]);
        }
        model.add_row(detached.rowLower[r], detached.rowUpper[r], Int32Array.from(idx), Float64Array.from(val));
      }
      for (let c = 0; c < n; c++) {
        if (detached.colCost[c] !== 0) model.change_col_cost(c, sign * detached.colCost[c]);
        if (detached.integrality[c] !== 0) model.change_col_integrality(c, detached.integrality[c]);
      }
    }
    const buildMs = performance.now() - tBuild;
    model.set_time_limit(seconds);
    const t0 = performance.now();
    model.run();
    const ms = performance.now() - t0;
    // Zero-copy view (aliases wasm memory); copy once for the JS checker.
    const view = typeof model.get_solution_object === 'function'
      ? model.get_solution_object().colValue
      : model.get_solution();
    return {
      status: statusName(model.get_model_status()),
      objective: sign * model.get_objective_value(),
      colValue: Array.from(view),
      ms,
      buildMs,
      profile: typeof model.get_info === 'function'
        ? {
            heur: model.get_info('time_heuristic_secs'),
            local: model.get_info('time_local_search_secs'),
            bb: model.get_info('time_branch_and_bound_secs'),
          }
        : null,
    };
  } finally {
    model.free?.();
  }
}

async function main() {
  const highs = await loadHighs();
  const optio = await loadOptio();
  console.log(`highs-js: ${highsSource}`);
  console.log(`optio-wasm loaded\n`);
  console.log(
    pad('fixture', 20) + pad('kind', 5) +
    padL('hi_ms', 9) + padL('optio_ms', 10) + padL('build_ms', 10) + padL('ratio', 8) +
    padL('hi_obj', 13) + padL('optio_obj', 13) + padL('rel', 8) + '  feas/check  profile(ms h/l/bb)',
  );
  console.log('-'.repeat(110));

  for (const f of files) {
    const path = join(fixtures, f);
    if (!existsSync(path)) {
      console.log(pad(f, 20) + 'MISSING');
      continue;
    }
    const lp = readFileSync(path, 'utf8');
    const seconds = TIME > 0 ? TIME : 20;
    // Reference: highs-js. Time run() only.
    const hm = highs.createModel({ format: 'lp', data: lp });
    hm.options.set({ output_flag: false, time_limit: seconds });
    const t0 = performance.now();
    hm.run();
    const hiMs = performance.now() - t0;
    const detached = hm.getModel('csr');
    const hiObj = hm.getObjectiveValue();
    const hi = statusName(hm.getModelStatus());
    hm.dispose?.();

    const o = runOptio(optio, detached, seconds);
    const chk = o.colValue.length === detached.numCols ? checkModel(detached, o.colValue, 1e-6) : null;
    const maxViol = chk
      ? Math.max(chk.maxRowViolation ?? 0, chk.maxBoundViolation ?? 0, chk.maxIntegralityViolation ?? 0)
      : NaN;
    const rel = Number.isFinite(hiObj) && Number.isFinite(o.objective)
      ? Math.abs(o.objective - hiObj) / Math.max(1, Math.abs(hiObj))
      : NaN;
    const kind = detached.integrality.some?.((t) => t !== 0) ? 'mip' : 'lp';
    console.log(
      pad(f, 20) + pad(kind, 5) +
      padL(num(hiMs, 1), 9) + padL(num(o.ms, 1), 10) + padL(num(o.buildMs, 1), 10) +
      padL(num(o.ms / hiMs, 2) + 'x', 8) +
      padL(num(hiObj, 2), 13) + padL(num(o.objective, 2), 13) + padL(num(rel, 4), 8) +
      '  ' + (o.status) + (chk ? (chk.feasible ? '/yes' : '/NO') : '/n/a') +
      (chk ? ` (v${maxViol.toExponential(0)})` : '') +
      (o.profile ? `  ${(o.profile.heur * 1000).toFixed(0)}/${(o.profile.local * 1000).toFixed(0)}/${(o.profile.bb * 1000).toFixed(0)}` : ''),
    );
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
