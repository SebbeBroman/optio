// Loader for the built optio wasm package.
//
// Prefers the publishable npm package (`npm/`, built with `npm/node build.mjs`
// from the repo root) and falls back to the raw wasm-bindgen nodejs output
// (`crates/wasm/build.sh` -> `pkg/`). `OPTIO_WASM` still overrides the path.
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { existsSync } from 'node:fs';
import { performance } from 'node:perf_hooks';

const require = createRequire(import.meta.url);
const candidates = [
  fileURLToPath(new URL('../../npm/index.cjs', import.meta.url)),
  fileURLToPath(new URL('../../pkg/optio_wasm.js', import.meta.url)),
];

export function loadOptio(jsPath = process.env.OPTIO_WASM ?? candidates.find(existsSync)) {
  if (!jsPath) {
    throw new Error(
      'optio wasm package not found; run `node npm/build.mjs` (or `crates/wasm/build.sh`) first',
    );
  }
  if (!existsSync(jsPath)) {
    throw new Error(
      `optio wasm package not found at ${jsPath}; run \`node npm/build.mjs\` (or \`crates/wasm/build.sh\`) first`,
    );
  }
  return require(jsPath);
}

/**
 * Solve a detached model with optio. Returns null until the solver is
 * implemented (the wasm `run()` currently reports NotSet).
 *
 * @param {object} detached model from highs `getModel('csr')`
 * @param {{ seconds?: number }} opts
 */
export async function solveWithOptio(detached, opts = {}) {
  const mod = await loadOptio();
  const model = new mod.Model();
  // The solver minimises; negate costs for a maximisation model and negate the
  // objective back so the caller sees the original sense.
  const sign = detached.sense !== undefined && detached.sense < 0 ? -1 : 1;
  try {
    const n = detached.numCols;
    const { starts, indices, values } = detached.matrix; // csr
    if (typeof model.add_model === 'function') {
      // Bulk CSR load: one call instead of n + rows JS↔wasm calls.
      const cost = new Float64Array(n);
      const integrality = new Int32Array(n);
      for (let c = 0; c < n; c++) {
        cost[c] = sign * detached.colCost[c];
        integrality[c] = detached.integrality[c] | 0;
      }
      model.add_model(
        n,
        cost,
        Float64Array.from(detached.colLower),
        Float64Array.from(detached.colUpper),
        integrality,
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
    // Optional warm start. Accepts a dense vector, or the sparse
    // `{ indices, values }` a highs-js caller would pass to `setSolution`;
    // optio takes a dense vector either way.
    if (opts.warmStart) {
      let dense;
      if (Array.isArray(opts.warmStart) || ArrayBuffer.isView(opts.warmStart)) {
        dense = Float64Array.from(opts.warmStart);
      } else {
        dense = new Float64Array(n);
        const idx = opts.warmStart.indices ?? [];
        const val = opts.warmStart.values ?? [];
        for (let k = 0; k < idx.length; k++) dense[idx[k]] = val[k];
      }
      model.set_solution(dense);
    }
    if (opts.seconds) model.set_time_limit(opts.seconds);
    if (opts.gap != null && typeof model.set_mip_rel_gap === 'function') {
      model.set_mip_rel_gap(opts.gap);
    }
    if (opts.absGap != null && typeof model.set_option === 'function') {
      model.set_option('mip_abs_gap', opts.absGap);
    }
    const t0 = performance.now();
    let firstIncumbentMs = null;
    let incumbents = 0;
    if (opts.onIncumbent && typeof model.run_with_callbacks === 'function') {
      model.run_with_callbacks({
        mipImprovingSolution: (inc) => {
          incumbents += 1;
          if (firstIncumbentMs == null) firstIncumbentMs = performance.now() - t0;
          opts.onIncumbent(inc);
        },
      });
    } else {
      model.run();
    }
    const ms = performance.now() - t0;
    // Zero-copy view; copy once so the caller can retain it.
    const colValue = typeof model.get_solution_object === 'function'
      ? Array.from(model.get_solution_object().colValue)
      : model.get_solution();
    return {
      status: model.get_model_status(),
      objective: sign * model.get_objective_value(),
      colValue,
      ms,
      firstIncumbentMs,
      incumbents,
      dualBound: sign * model.get_info('mip_dual_bound'),
      nodes: model.get_info('mip_node_count'),
      iterations: model.get_info('simplex_iteration_count'),
      timing: Object.fromEntries(['presolve','heuristic','local_search','root_cuts','bound_certificate','branch_and_bound'].map(k => [k, model.get_info(`time_${k}_secs`)])),
    };
  } finally {
    model.free?.();
  }
}
