/**
 * Shared runtime state for the optio package.
 *
 * `index.js` (the loader) flips `ready` once the wasm module is instantiated;
 * `solver.js` (the adapter) reads it to fail with a friendly message when the
 * solver is used before `await init()`.
 */
export const state = { ready: false };
