/**
 * Smoke tests for the optio npm package. Plain Node assertions; also runs
 * under Bun (`bun test.mjs`). Requires `node build.mjs` first.
 */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const pkg = JSON.parse(readFileSync(join(here, 'package.json'), 'utf8'));

const { init, initSync, isReady, Model, version, createSolver, ModelStatus, statusName } =
  await import('./index.js');

// --- init (auto-detect: disk read in Node/Bun, fetch in browsers) ----------
assert.equal(isReady(), false);
assert.throws(
  () => new Model(),
  /Cannot read properties of undefined|undefined is not an object/,
  'using the solver before init should fail loudly',
);
const first = await init();
assert.equal(isReady(), true);
// Repeat and concurrent calls share the same load — no double instantiation.
const [second, third] = await Promise.all([init(), init()]);
assert.equal(second, first);
assert.equal(third, first);
const v = version();
assert.equal(v, pkg.version, `wasm version ${v} must match package.json ${pkg.version}`);
console.log(`init: ok (optio ${v}, idempotent)`);

// --- demo MIP through the raw surface --------------------------------------
// min -x - 10y  s.t.  x + 7y <= 17.5, 0<=x<=3.5, y>=0 integer  ->  -23.5
{
  const m = new Model();
  m.add_var(0, 3.5);
  m.add_var(0, 1e30);
  m.change_col_cost(0, -1);
  m.change_col_cost(1, -10);
  m.change_col_integrality(1, 1);
  m.add_row(-1e30, 17.5, Int32Array.from([0, 1]), Float64Array.from([1, 7]));
  m.set_time_limit(5);
  m.run();
  assert.equal(m.get_model_status(), ModelStatus.Optimal);
  assert.equal(m.get_objective_value(), -23.5);
  const sol = m.get_solution_object();
  assert.ok(sol.colValue instanceof Float64Array);
  m.free();
  console.log('raw Model demo MIP: ok');
}

// --- bulk CSR load (add_model) + MPS export --------------------------------
{
  const m = new Model();
  // min x + y  s.t.  x + y >= 2, 0<=x<=10, 0<=y<=10 integer  ->  2
  m.add_model(
    2,
    Float64Array.from([1, 1]),
    Float64Array.from([0, 0]),
    Float64Array.from([10, 10]),
    Int32Array.from([0, 1]),
    Float64Array.from([2]),
    Float64Array.from([1e30]),
    Uint32Array.from([0, 2]), // one row, two entries
    Uint32Array.from([0, 1]),
    Float64Array.from([1, 1]),
  );
  assert.equal(m.num_col(), 2);
  assert.equal(m.num_row(), 1);
  m.run();
  assert.equal(m.get_model_status(), ModelStatus.Optimal);
  assert.ok(Math.abs(m.get_objective_value() - 2) < 1e-6, `objective ${m.get_objective_value()}`);
  const mps = m.export_model('mps');
  assert.ok(mps.includes("'MARKER'"), 'MPS export marks the integer column');
  assert.ok(mps.includes('RHS'));
  m.free();
  console.log('add_model bulk load + mps export: ok');
}

// --- high-level solver surface -------------------------------------------
{
  const solver = createSolver();
  const model = solver.createModel();
  model.options.set({ output_flag: false, time_limit_secs: 5 });
  const x = model.addVar(0, 3.5);
  const y = model.addVar(0, 1e30);
  model.changeColCost(x, -1);
  model.changeColCost(y, -10);
  model.changeColIntegrality(y, 1);
  model.addRow(-1e30, 17.5, { indices: [x, y], values: [1, 7] });
  model.passColName(x, 'x');
  model.passRowName(0, 'cap');
  model.run();
  assert.equal(model.getModelStatus(), ModelStatus.Optimal);
  assert.equal(model.getObjectiveValue(), -23.5);
  assert.equal(model.getSolution().colValue.length, 2);
  assert.equal(model.info.get('objective_function_value'), -23.5);
  assert.ok(model.exportModel('lp').includes('cap'));
  assert.equal(model.wasInterrupted(), false);
  model.dispose();
  console.log('solver surface: ok');

  // withModel disposes in finally.
  let observed;
  solver.withModel((m) => {
    observed = m.addVar(0, 1);
  });
  assert.equal(observed, 0);
  console.log('withModel scoping: ok');
}

// --- partial solutions: incumbents + progress callbacks --------------------
// Weighted binary selection; the LP bound is fractional, so B&B improves the
// incumbent in stages and both callbacks fire.
{
  const n = 12;
  const solver = createSolver();
  const model = solver.createModel();
  const weights = [7, 6, 5, 5, 4, 4, 3, 3, 2, 2, 1, 1];
  for (let i = 0; i < n; i++) model.addVar(0, 1), model.changeColIntegrality(i, 1);
  for (let i = 0; i < n; i++) model.changeColCost(i, -weights[i]);
  model.addRow(-1e30, 7.5, {
    indices: Array.from({ length: n }, (_, i) => i),
    values: weights,
  });
  const progress = [];
  const incumbents = [];
  model.run({
    callbacks: {
      onProgress: (p) => progress.push(p),
      onIncumbent: (inc) => {
        assert.equal(inc.colValue.length, n);
        incumbents.push(inc.objective);
      },
    },
  });
  assert.equal(model.getModelStatus(), ModelStatus.Optimal);
  const objective = model.getObjectiveValue();
  assert.ok(incumbents.length >= 1, 'expected at least one improving solution');
  assert.ok(progress.length >= 1, 'expected progress callbacks');
  assert.equal(incumbents.at(-1), objective, 'last incumbent must equal the final objective');
  for (let i = 1; i < incumbents.length; i++) {
    assert.ok(
      incumbents[i] <= incumbents[i - 1] + 1e-9,
      'incumbents must not get worse',
    );
  }
  model.dispose();
  console.log(`callbacks: ok (${incumbents.length} incumbents, ${progress.length} progress ticks)`);
}

// --- interrupt stops early but yields the partial solution --------------
{
  const n = 60;
  const solver = createSolver();
  const model = solver.createModel();
  for (let i = 0; i < n; i++) model.addVar(0, 1), model.changeColIntegrality(i, 1);
  for (let i = 0; i < n; i++) model.changeColCost(i, -(i + 1));
  model.addRow(-1e30, (n - 1) / 2, {
    indices: Array.from({ length: n }, (_, i) => i),
    values: Array.from({ length: n }, () => 1),
  });
  const incumbents = [];
  model.run({
    callbacks: {
      onIncumbent: (inc) => incumbents.push(inc.objective),
      interrupt: () => incumbents.length >= 1, // stop at the first incumbent
    },
  });
  assert.equal(model.getModelStatus(), ModelStatus.Interrupt, 'interrupt must surface as a status');
  assert.ok(model.wasInterrupted(), 'wasInterrupted must be set');
  assert.ok(incumbents.length >= 1, 'partial (interrupted) solve still reports incumbents');
  const sol = model.getSolution();
  assert.equal(sol.colValue.length, n, 'partial solution is a full column vector');
  const values = Array.from(sol.colValue).filter((v) => v > 0.5);
  assert.ok(
    values.every((v) => Math.abs(v - 1) < 1e-6),
    'reported partial solution must be integral',
  );
  assert.equal(incumbents.at(-1), model.getObjectiveValue());
  model.dispose();
  console.log('interrupt: ok (status Interrupt, partial solution retained)');
}

// --- infeasible model ------------------------------------------------------
{
  const solver = createSolver();
  const model = solver.createModel();
  const x = model.addVar(0, 1);
  model.addRow(2, 1e30, { indices: [x], values: [1] }); // x >= 2 with x <= 1
  model.run();
  assert.equal(model.getModelStatus(), ModelStatus.Infeasible);
  const iis = model.getIis();
  assert.ok(iis.colIndex instanceof Int32Array);
  model.dispose();
  console.log('infeasible + getIis: ok');
}

// --- statusName -------------------------------------------------------------
assert.equal(statusName(ModelStatus.Optimal), 'Optimal');
assert.equal(statusName(99), 'Unknown(99)');
console.log('statusName: ok');

// --- initSync (explicit bytes; works everywhere) ---------------------------
{
  const bytes = readFileSync(join(here, 'wasm', 'optio_wasm_bg.wasm'));
  const exports = initSync({ module: new Uint8Array(bytes) });
  assert.ok(exports.memory instanceof WebAssembly.Memory);
  // The same wasm is already loaded; re-init must be a no-op returning the
  // same exports, and version() must keep working.
  assert.equal(version(), pkg.version);
  console.log('initSync: ok');
}

console.log('\nall optio package tests passed');
