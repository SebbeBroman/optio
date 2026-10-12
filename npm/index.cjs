/**
 * optio — CommonJS entry.
 *
 * The wasm-bindgen nodejs-target glue reads the .wasm from disk and
 * instantiates it synchronously at require time, so the raw surface
 * (`Model`, `version`) is ready immediately:
 *
 * ```js
 * const { Model, version } = require('optio');
 * ```
 *
 * The full API (async `init` with auto-loading, the high-level
 * `createSolver`, status names) lives in the ESM entry: `import('optio')`.
 */
module.exports = require('./wasm/optio_wasm.cjs');
