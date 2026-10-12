# optio

A data-oriented **LP/MIP solver compiled to WebAssembly**, browser-first with
first-class Node and Bun support. The high-level API is inspired by the HiGHS
JS API, so migrating an existing integration is mostly renaming.

```sh
npm install optio
```

## Quick start

```js
import { init, createSolver, ModelStatus, statusName } from 'optio';

await init(); // loads the bundled .wasm (see "Loading" below)

const solver = createSolver();
const model = solver.createModel();

const x = model.addVar(0, 3.5);
const y = model.addVar(0, Infinity);
model.changeColCost(x, -1);
model.changeColIntegrality(y, 1); // 0 continuous, nonzero integer
model.addRow(-Infinity, 17.5, { indices: [x, y], values: [1, 7] });

model.run();
model.getModelStatus();          // 7 === ModelStatus.Optimal
model.getObjectiveValue();       // -23.5
model.getSolution().colValue;    // Float64Array [3.5, 2]
model.exportModel('lp');         // CPLEX LP text ('mps' also supported)
model.dispose();                 // or solver.withModel(...) to scope it
```

`options.set` / `info.get` accept string or record forms:

```js
model.options.set({ output_flag: false, time_limit_secs: 5, mip_rel_gap: 0.01 });
model.info.get('objective_function_value');
model.info.get('mip_dual_bound'); // certified lower bound (minimisation)
```

## Partial solutions and callbacks

`run` accepts callbacks; you get every improving incumbent as it is found, so
you always hold a usable partial plan:

```js
model.run({
  callbacks: {
    // progress snapshot: { runningTime, nodes, lpIterations, gap,
    //                      primalBound, dualBound, hasIncumbent }
    onProgress: (p) => console.log(p),

    // new incumbent { objective, colValue: Float64Array }
    onIncumbent: (inc) => renderPlan(inc.colValue),

    // return a truthy value to request an early stop
    interrupt: (p) => deadlineReached() || p.nodes > 10_000,
  },
});

model.wasInterrupted(); // true if `interrupt` fired
model.getModelStatus(); // ModelStatus.Interrupt when stopped early
```

An interrupted solve keeps the best solution found so far —
`getSolution()` returns a full, feasible column vector.

## Loading

`init()` is idempotent and auto-detects the environment:

| environment          | how the `.wasm` is loaded                                        |
| -------------------- | ---------------------------------------------------------------- |
| bundler (Vite, webpack 5, esbuild, …) | `new URL(..., import.meta.url)` asset reference |
| browser (no bundler) | `fetch` of the file next to the ES module                        |
| Node / Bun (ESM)     | read from disk with `node:fs`, no fetch involved                  |
| Node / Bun (`require`)| synchronous read + instantiate at require time                   |

Pass an explicit source to override the default:

```js
await init(fetchedBytes);                    // Uint8Array/ArrayBuffer
await init(myWebAssemblyModule);             // precompiled module
await init(new URL('...', import.meta.url)); // custom location
await init(new Response(stream));            // Response (streamed)
```

`initSync(bytesOrModule)` is available when you must construct models
synchronously; without arguments it reads from disk in Node/Bun and throws a
helpful error in the browser.

The CommonJS entry (`require('optio')`) exposes the raw surface (`Model`,
`version`) and initialises synchronously from disk; use the ESM entry for the
full API.

## Raw wasm surface

The high-level API is sugar over the wasm-bindgen exports; the snake_case
surface is exported directly (`Model`, `version`):

```js
import { Model } from 'optio';
await init();
const m = new Model();
m.add_var(0, 1);
m.run();
m.get_model_status(); // 7 === ModelStatus.Optimal
```

### Options (`set_option` / `options.set`)

`output_flag`, `time_limit_secs`, `mip_rel_gap`, `mip_abs_gap`, `threads`,
`random_seed`, `presolve`, `lp_warm_start`, `mip_cuts`, `mip_pseudocost`,
`mip_node_presolve`, `mip_column_compression`, `mip_objective_counts`,
`mip_objective_lattice`, `mip_strong_branch`, `mip_max_nodes`,
`adaptive_search`, `balance_target_ratio`. Unknown names are ignored.

### Info keys (`get_info` / `info.get`)

`objective_function_value`, `mip_gap`, `mip_dual_bound`, `mip_node_count`,
`simplex_iteration_count`, `primal_solution_status`, `max_integrality_violation`,
timing keys (`time_heuristic_secs`, `time_branch_and_bound_secs`, …) and
solver statistics (`node_lp_solves`, `mip_root_cuts`, …). Unknown names
return `0`.

### Status codes

`ModelStatus` maps names to the stable numeric codes the solver reports
(`Optimal` = 7, `Infeasible` = 8, `TimeLimit` = 13, `Interrupt` = 17, …);
`statusName(code)` returns the name.

## Development

```sh
cd npm
node build.mjs      # cargo + wasm-bindgen + wasm-opt → wasm/, version sync
node test.mjs       # Node/Bun smoke suite
node test-http.mjs  # browser loading path over http (fetch + streaming)
node serve.mjs      # then open http://localhost:8123/smoke.html
bun test.mjs        # same suite under Bun
```

`typecheck.ts` (not published) guards the public `.d.ts`; run it with
`tsc --noEmit --strict --target es2022 --module esnext --moduleResolution bundler typecheck.ts`.

Build requirements: Rust (nightly with `rust-src` unlocks the smallest
`panic=immediate-abort` build; `OPTIO_STABLE=1` forces the stable path) with
the `wasm32-unknown-unknown` target, plus `wasm-bindgen` and `wasm-opt`.

## Status

Early (0.0.x): the API is settling and parity with HiGHS is tracked in the
repository's `LOSSES.md`. Native (napi) builds are planned but not part of
this package yet.

## License

MIT — see [LICENSE](./LICENSE).
