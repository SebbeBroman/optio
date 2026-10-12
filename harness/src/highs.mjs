// Load the highs-js module (the oracle baseline).
//
// Defaults to the sibling checkout at ../../highs-js/build/highs.mjs; override
// with HIGHS_JS=/abs/path/to/highs.mjs (or .../highs.wasm's loader).
import { fileURLToPath, pathToFileURL } from 'node:url';

const defaultUrl = new URL('../../../highs-js/build/highs.mjs', import.meta.url);
const highsUrl = process.env.HIGHS_JS
  ? pathToFileURL(process.env.HIGHS_JS)
  : defaultUrl;

let promise = null;

export function loadHighs() {
  if (!promise) {
    promise = import(highsUrl.href).then((m) => m.default());
  }
  return promise;
}

export const highsSource = process.env.HIGHS_JS ?? fileURLToPath(defaultUrl);
