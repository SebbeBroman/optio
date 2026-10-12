/**
 * Exercises the *browser* loading path of the package over HTTP, headlessly:
 * `init()` is given an http URL and the glue takes its `fetch` +
 * `WebAssembly.instantiateStreaming` branch, exactly as in a browser served
 * with `application/wasm`. Also validates `serve.mjs` MIME handling.
 *
 * Run:  node test-http.mjs   (starts its own ephemeral server)
 */
import assert from 'node:assert/strict';
import { startServer } from './serve.mjs';
import { init, createSolver, ModelStatus, version, statusName } from './index.js';

const server = await startServer(0);
const port = /** @type {import('node:net').AddressInfo} */ (server.address()).port;
const base = `http://127.0.0.1:${port}`;

// The browser path: a URL goes through fetch + instantiateStreaming, and the
// server's application/wasm content type decides streaming vs arrayBuffer.
const exports = await init(new URL(`${base}/wasm/optio_wasm_bg.wasm`));
assert.ok(exports.memory instanceof WebAssembly.Memory);
assert.ok(version().length > 0);

// Full solve through the high-level solver surface.
const solver = createSolver();
const model = solver.createModel();
const x = model.addVar(0, 3.5);
const y = model.addVar(0, 1e30);
model.changeColCost(x, -1);
model.changeColCost(y, -10);
model.changeColIntegrality(y, 1);
model.addRow(-1e30, 17.5, { indices: [x, y], values: [1, 7] });
model.run();
assert.equal(model.getModelStatus(), ModelStatus.Optimal, `status ${statusName(model.getModelStatus())}`);
assert.equal(model.getObjectiveValue(), -23.5);
model.dispose();

server.close();
console.log('browser loading path (http + fetch + instantiateStreaming): ok');
