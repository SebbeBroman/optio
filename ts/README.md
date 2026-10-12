# optio-ts

A faithful TypeScript port of the Rust [`optio`](../README.md) LP/MIP engine,
tuned for **single-threaded V8 / browser** performance. It mirrors
`crates/core/src/*.rs` module for module (same algorithms, tolerances and branch
structure); `checkSolution` remains the oracle, never the solver's own status.

> **scriptc was dropped.** An earlier revision also compiled with
> [scriptc](https://scriptc.dev/docs/limitations) to a native executable and a
> `wasm32-wasi` module. That path was removed: the scriptc wasm was larger
> (~219 KB brotli vs ~28 KB brotli for the minified JS) *and* ~4–8× slower than
> the same code under V8, so for a browser target the transpiled JS wins.
> Dropping scriptc also lifted its generic-array-write restriction, letting
> `VecMap` use a plain array. The Rust [`optio-wasm`](../crates/wasm) remains the
> fastest, smallest *compiled* artifact; this port's primary value is the
> independent oracle / differential harness (see [Tests](#tests)).

## Layout

| TS | Rust |
|---|---|
| `src/model.ts` | `crates/core/src/model.rs` |
| `src/status.ts` | `crates/core/src/status.rs` |
| `src/options.ts` | `crates/core/src/options.rs` |
| `src/clock.ts` / `src/types.ts` | type half of `crates/core/src/solve.rs` |
| `src/lp.ts` | `crates/core/src/lp.rs` |
| `src/presolve.ts` | `crates/core/src/presolve.rs` |
| `src/heuristic.ts` | `crates/core/src/heuristic.rs` |
| `src/mip.ts` | `crates/core/src/mip.rs` |
| `src/local_search.ts` | `crates/core/src/local_search.rs` |
| `src/block_planner.ts` | `crates/core/src/block_planner.rs` |
| `src/solve.ts` | `crates/core/src/solve.rs` |
| `src/bench.ts` / `src/cli.ts` | `crates/bench/src/main.rs` (JSON contract) |

The native parallel branch & bound (`branch_and_bound_mt`, rayon +
crossbeam-deque) is intentionally **not** ported: this is a single-threaded port,
matching the wasm build's code path. `options.threads` is accepted and ignored.

## Performance posture

- Hot fixed-size numeric / index state uses `Float64Array` / `Int32Array`
  (the simplex `x`/`lo`/`hi`/`cost`, the basis factors' `work`, the CSC/CSR
  matrix, and the f64 bit helpers). Growing collections stay plain `number[]`
  (packed doubles).
- No per-iteration allocations in the simplex hot loop; eta files and basis
  columns are reused in place.
- `block_planner`'s row scans read `aStart`/`aIndex`/`aValue` directly instead
  of materialising `(col, val)` tuples, and its scheduling/day DPs use an exact
  numeric-vector `VecMap` rather than string keys.
- u64 arithmetic that Rust relies on (xorshift RNGs, LCGs) uses `BigInt` masked
  to 64 bits for exactness; the f64 bit operations in `f64bits.ts` are done with
  `Uint32Array` words (no per-call BigInt allocation).

## Run

```sh
cd ts
npm install

npm test              # node --test over test/*.test.ts (type stripping)
npm run typecheck     # tsc --noEmit

# Differential check against the Rust oracle.
cargo build -p optio-bench --release            # from the repo root
node tools/differential.mjs --mode both --count 40
```

`tools/differential.mjs` generates random LP/MILP models, solves each with the
Rust `optio-bench` binary and with `src/cli.ts`, and compares status, objective,
dual bound and feasibility.

## Comparison

`tools/compare.mjs` (`npm run compare`) solves the same detached model JSON with
three engines and reports each engine's internally-measured solver time plus an
independent feasibility check:

- **rust-native** — `target/release/optio-bench`
- **rust-wasm** — `pkg/optio_wasm.js` (wasm-bindgen, `--target nodejs`)
- **ts-node** — `src/cli.ts` under Node (V8)

`harness/fixtures/planning-app-{small,medium,large}.model.json`, 10 s limit,
`threads=1` (rust wasm is single-threaded). `solver_ms` is the in-process solve
time (excludes process startup and model construction):

| model | engine | objective | solver_ms | wall_ms | feasible |
|---|---|---|---|---|---|
| small (12 cols) | rust-native | 314.9 | 0.3 | 6.2 | yes |
| | rust-wasm | 314.9 | 4.6 | 4.9 | yes |
| | ts-node | 314.9 | 4.0 | 99.3 | yes |
| medium (848 cols) | rust-native | 3096.8 | 13.3 | 17.1 | yes |
| | rust-wasm | 3096.8 | 63.9 | 64.0 | yes |
| | ts-node | 3096.8 | 141.0 | 237.8 | yes |
| large (15.4k cols) | rust-native | 4028.4 | 15.2 | 31.0 | yes |
| | rust-wasm | 4028.4 | 28.5 | 29.7 | yes |
| | ts-node | 4028.4 | 121.0 | 233.2 | yes |

**Solutions match exactly** — identical status and objective on every instance,
and every point passes the shared `checkModel` oracle. Speed ordering is
`rust-native` ≪ `rust-wasm` < `ts-node`.

### Browser (V8) warm comparison

The table above is one **cold** solve per fresh process. V8 optimizes the hot
loops during a solve, while the wasm builds are AOT, so the fairer "best case"
for a browser (and what an app session reaches after the first solve) is the
minimum over repeated solves (`tools/timing.ts`, `tools/warm.mjs`):

| instance | rust-native | rust-wasm | ts-node (before) | ts-node (after) |
|---|---|---|---|---|
| medium (848 cols) | 12.9 ms | 43.0 ms | 136 ms | **58 ms** |
| large (15.4k cols) | 15.0 ms | 24.6 ms | 59 ms | **35 ms** |

Warm ts-node is **~1.35–1.4×** rust-wasm on both instances (2.4–3.2× before the
data-layout pass). Rust native stays ~2.5–5× ahead (multi-core AOT).

The pass changed **no algorithm** — only the data layout of the hot phases
(`heuristic`/`boundCert` for medium, `presolve`/`heuristic` for large):


- `f64bits.ts`: `nextUp`/`nextDown`/`totalCmp` via `Uint32Array` word ops instead
  of `DataView.getBigUint64` (which allocated a BigInt per call; `rowBound` calls
  them per nonzero).
- `vecmap.ts`: an exact numeric-vector hash map replaces the `Map<string>` +
  `join(",")` keys in `dayDp` and `schedulingBound` (no string allocation).
- `presolve.propagateWeighted`: stamp-based duplicate detection and in-place
  scratch — no per-row allocation, no sort.
- `detect`/`rowBound`: the `row()` tuple allocations and `.filter/.every/.some`
  closures are inlined as direct `aStart`/`aIndex`/`aValue` index loops.



## Tests

`test/` ports the Rust tests that assert numeric outcomes (`lp.rs`,
`presolve.rs`, `planning_optimization.rs`, `planning_app_model.rs`, the
`heuristic.rs` fixtures, the presolve contradiction cases) plus smoke tests for
the two structure-aware planning modules.

## License

MIT, matching the Rust project.
