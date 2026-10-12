/**
 * optio — a data-oriented LP/MIP solver compiled to WebAssembly.
 *
 * Single universal entry: browsers (and bundlers), Node and Bun all import
 * this file. The wasm binary ships next to the code and is loaded
 * automatically:
 *
 * * browsers / bundlers — `new URL('…/optio_wasm_bg.wasm', import.meta.url)`
 *   plus `fetch` (bundler builds rewrite the URL into an asset reference);
 * * Node / Bun — the file is read from disk with `node:fs`, no fetch involved.
 *
 * Pass an explicit source to `init()` to override the default (see
 * `InitInput`), e.g. bytes you fetched yourself, a `WebAssembly.Module`, or a
 * URL in a bundler where the automatic reference does not apply.
 *
 * ```js
 * import { init, createSolver } from 'optio';
 *
 * await init();
 * const solver = createSolver();
 * const model = solver.createModel();
 * model.addVar(0, 3.5);
 * model.addVar(0, Infinity);
 * model.changeColCost(0, -1);
 * model.changeColIntegrality(1, 1);
 * model.addRow(-Infinity, 17.5, { indices: [0, 1], values: [1, 7] });
 * model.run();
 * model.getObjectiveValue(); // -23.5
 * model.dispose();
 * ```
 */

import {
  Model,
  version,
  initSync as glueInitSync,
  default as glueInit,
} from './wasm/optio_wasm.js';
import { state } from './state.js';

export { Model, version } from './wasm/optio_wasm.js';
export { createSolver } from './solver.js';

/** Model status codes (stable numeric IDs; see README for the table). */
export const ModelStatus = /** @type {const} */ ({
  NotSet: 0,
  LoadError: 1,
  ModelError: 2,
  PresolveError: 3,
  SolveError: 4,
  PostsolveError: 5,
  Empty: 6,
  Optimal: 7,
  Infeasible: 8,
  UnboundedOrInfeasible: 9,
  Unbounded: 10,
  ObjectiveBound: 11,
  ObjectiveTarget: 12,
  TimeLimit: 13,
  IterationLimit: 14,
  Unknown: 15,
  SolutionLimit: 16,
  Interrupt: 17,
});

const STATUS_NAMES = [
  'NotSet',
  'LoadError',
  'ModelError',
  'PresolveError',
  'SolveError',
  'PostsolveError',
  'Empty',
  'Optimal',
  'Infeasible',
  'UnboundedOrInfeasible',
  'Unbounded',
  'ObjectiveBound',
  'ObjectiveTarget',
  'TimeLimit',
  'IterationLimit',
  'Unknown',
  'SolutionLimit',
  'Interrupt',
];

/** Name of a model status code, e.g. `statusName(ModelStatus.Optimal)` → `"Optimal"`. */
export function statusName(status) {
  return STATUS_NAMES[status] ?? `Unknown(${status})`;
}

const WASM_PATH = './wasm/optio_wasm_bg.wasm';

function wasmUrl() {
  return new URL(WASM_PATH, import.meta.url);
}

function isNodeLike() {
  return (
    typeof process !== 'undefined' &&
    typeof process.versions === 'object' &&
    process.versions !== null &&
    typeof process.versions.node === 'string'
  );
}

/** Read the wasm file from disk (Node / Bun), bypassing fetch entirely. */
async function readWasmFile(url) {
  try {
    const { readFile } = await import('node:fs/promises');
    const { fileURLToPath } = await import('node:url');
    return new Uint8Array(await readFile(fileURLToPath(url)));
  } catch (err) {
    // Sandboxed runtimes may block fs. Fall back to the URL and let the
    // environment's own loader attempt it.
    console.warn(`optio: could not read ${url.href} from disk, falling back to fetch`, err);
    return url;
  }
}

/**
 * Default wasm source: bytes on disk for Node/Bun, the URL for browsers and
 * bundlers (the URL expression above is what bundlers rewrite into an asset).
 */
async function defaultWasmSource() {
  const url = wasmUrl();
  if (url.protocol === 'file:' && isNodeLike()) {
    return await readWasmFile(url);
  }
  return url;
}

/** Synchronous disk read for `initSync()` on Node/Bun (no dynamic imports). */
function defaultSyncSource() {
  if (isNodeLike() && typeof process.getBuiltinModule === 'function') {
    const url = wasmUrl();
    if (url.protocol === 'file:') {
      const fs = process.getBuiltinModule('fs');
      const { fileURLToPath } = process.getBuiltinModule('url');
      return new Uint8Array(fs.readFileSync(fileURLToPath(url)));
    }
  }
  throw new Error(
    'optio: initSync() without arguments needs the .wasm file on disk (Node/Bun). ' +
      'In the browser, pass bytes or a WebAssembly.Module, or use `await init()`.',
  );
}

/** Wrap raw inputs so the generated glue does not warn about legacy parameters. */
function asModuleOrPath(input) {
  return input !== null && typeof input === 'object' && Object.getPrototypeOf(input) === Object.prototype
    ? input
    : { module_or_path: input };
}

function asModule(input) {
  return input !== null && typeof input === 'object' && Object.getPrototypeOf(input) === Object.prototype
    ? input
    : { module: input };
}

let initPromise = null;
let initResult = null;

/**
 * Instantiate the wasm module. Idempotent; concurrent callers share one load.
 *
 * @param {InitInput | { module_or_path?: InitInput }} [input]
 *   `URL`/`Response`/`Request`, `BufferSource` (bytes), a precompiled
 *   `WebAssembly.Module`, or `{ module_or_path }`. Omit for auto-detection.
 * @returns {Promise<InitOutput>}
 */
export function init(input) {
  if (initResult) return Promise.resolve(initResult);
  if (!initPromise) {
    initPromise = (async () => {
      const source =
        input === undefined || input === null
          ? asModuleOrPath(await defaultWasmSource())
          : asModuleOrPath(input);
      initResult = await glueInit(source);
      state.ready = true;
      return initResult;
    })();
    // Allow a retry after a failed load.
    initPromise.catch(() => {
      initPromise = null;
    });
  }
  return initPromise;
}

/**
 * Synchronous variant of {@link init}. Without arguments it reads the shipped
 * .wasm from disk (Node/Bun only); otherwise pass `BufferSource` or a
 * precompiled `WebAssembly.Module`.
 *
 * @param {SyncInitInput | { module?: SyncInitInput }} [input]
 * @returns {InitOutput}
 */
export function initSync(input) {
  if (initResult) return initResult;
  const source =
    input === undefined || input === null ? defaultSyncSource() : asModule(input);
  initResult = glueInitSync(/** @type {any} */ (source));
  state.ready = true;
  return initResult;
}

/** True once the wasm module has been instantiated (by `init` or `initSync`). */
export function isReady() {
  return state.ready;
}

/** `import init from 'optio'` — the wasm-bindgen style default export. */
export default init;
