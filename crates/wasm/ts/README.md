# `highs-shim.ts` — optio behind the `highs` import

A thin, typed adapter that lets the planning app switch between `highs-js` and
`optio-wasm` behind a flag. It maps the `highs` extended surface used by
`mip-plan.ts` onto the `snake_case` builder exposed by `crates/wasm`.

## Usage

```ts
import optioModule from '../pkg/optio_wasm.js';   // adjust to your loader
import highsLoader from 'highs';                   // the real implementation
import { createOptioHighs, selectHighs } from './highs-shim';

const real = await highsLoader();
const optio = createOptioHighs(await optioModule());
const highs = selectHighs(real, optio);            // OPTIO_WASM=1 / __USE_OPTIO__

// From here the app code is unchanged:
const model = highs.createModel();
const x = model.addVar(0, 1);
model.changeColIntegrality(x, 1);
model.changeColCost(x, -1);
model.addRow(0, 1, { indices: Int32Array.of(x), values: Float64Array.of(1) });
model.options.set({ output_flag: false, time_limit: 10, mip_rel_gap: 1e-4 });
model.run({ callbacks: { mipImprovingSolution: (inc) => console.log(inc.objective) } });
model.getModelStatus();
model.getObjectiveValue();
model.getSolution().colValue;
model.dispose();
```

Feature flag: `optioEnabled()` is `true` when `OPTIO_WASM=1`/`true` (Node) or
`globalThis.__USE_OPTIO__ === true` (browser). Pass an explicit boolean to
`selectHighs(real, optio, true)` to force a side (used by the differential
oracle).

## Method map

| `highs` surface | `optio-wasm` method |
|---|---|
| `addVar(lower, upper)` | `add_var` |
| `addRow(lower, upper, {indices, values})` | `add_row` |
| `changeColCost` | `change_col_cost` |
| `changeColIntegrality` | `change_col_integrality` |
| `changeColLower` / `changeColUpper` | `change_col_lower` / `change_col_upper` |
| `changeColBounds` | `change_col_bounds` |
| `changeRowBounds` | `change_row_bounds` |
| `passColName` / `passRowName` | `pass_col_name` / `pass_row_name` |
| `setSolution({colValue})` | `set_solution` |
| `options.set(name, value)` / `options.set({...})` | `set_option` (per key) |
| `run({callbacks})` | `run_with_callbacks` (else `run`) |
| `getModelStatus` | `get_model_status` |
| `getObjectiveValue` | `get_objective_value` |
| `getSolution()` → `{colValue}` | `get_solution_object` |
| `info.get(name)` | `get_info` |
| `getIis()` | `get_iis` |
| `exportModel('lp' \| 'mps')` | `export_model` |
| `dispose` | `free` |

## Callback payloads

The wasm side calls each JS function with one flat object:

- `mipLogging(progress)` / `mipInterrupt(progress)`:
  `{ runningTime, nodes, lpIterations, gap, primalBound, dualBound, hasIncumbent }`.
  Return a truthy value from `mipInterrupt` to request an interrupt.
- `mipImprovingSolution(incumbent)`: `{ objective, colValue: Float64Array }`.

## Known gaps (see the Rust doc comments for detail)

- **No LP/MPS reader.** `createModel()` builds an empty model; the
  `withModel(source, op)` overloads throw. Only the write direction
  (`exportModel`) is implemented.
- **Warm start is stored, not forwarded.** `optio_core::solve_with` has no
  warm-start parameter yet, so `setSolution` retains the vector without using
  it.
- **Callbacks are not fired by the current core stub** (`mip::branch_and_bound`
  does not yet invoke `on_progress`/`on_incumbent`), and an interrupt request is
  only recorded, not honored. The mechanism is exposed so it works once the MIP
  workstream wires it.
- **`getIis()` returns empty arrays** (non-throwing) until the core computes an
  IIS.
- **`options.set` only knows the core's option subset** (see
  `crates/wasm/src/lib.rs::set_option`); unknown names are ignored rather than
  throwing (unlike highs-js).
- **Minimization is assumed** (no objective-sense API in the core); `getSolution`
  exposes only `colValue` (no `rowValue`/duals).
- **Column ordering** may differ after exporting/importing LP because readers
  order columns by first appearance.
