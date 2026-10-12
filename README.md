# optio

A from-scratch, **data-oriented LP/MIP engine**, built **wasm-first** and
**native-second**. Inspired by HiGHS; not a port, and does not track upstream.

It targets a real planning MILP first (event scheduling + ring-load
balancing), where the requirement is *a good, fully-placed plan fast* — optimality
is rarely required. See [`PLAN.md`](./PLAN.md) for the full design and roadmap.

## Status

The core implements LP simplex, presolve, branch-and-bound, primal planning
heuristics, and native parallel search. The real app scheduling fixtures now
reach checked optima in native and WASM, with verified lower bounds and shared
identical-group templates. See the [measured app report](docs/planning-app-2026-10-11/README.md).

Known losses vs HiGHS, each with a self-contained repro, live in
[`LOSSES.md`](./LOSSES.md).

## Layout

| path | crate | purpose |
|---|---|---|
| `crates/core` | `optio-core` | model IR, oracle, solve pipeline (presolve/LP/MIP) |
| `crates/bench` | `optio-bench` | native CLI + differential oracle runner |
| `crates/wasm` | `optio-wasm` | WebAssembly bindings, `highs-js`-compatible surface |
| `npm/` | — | publishable `optio` npm package: universal wasm loader + highs-js-compatible adapter |

## Build & test

```sh
cargo test                 # core unit tests + oracle
cargo run -p optio-bench   # tiny demo model through the oracle
```

## WebAssembly

```sh
rustup target add wasm32-unknown-unknown
cargo build -p optio-wasm --target wasm32-unknown-unknown --profile release-small
# then run wasm-bindgen (wasm-pack not installed here):
wasm-bindgen --target web --out-dir pkg target/wasm32-unknown-unknown/release-small/optio_wasm.wasm
```

## npm package

`npm/` builds the publishable, single-package wasm artifact (browser-first;
Node and Bun work too — no native/napi build yet):

```sh
cd npm
node build.mjs        # two wasm-bindgen targets → wasm/ + version sync with the workspace
node test.mjs         # Node/Bun smoke suite (also: bun test.mjs)
node test-http.mjs    # browser loading path over http (fetch + instantiateStreaming)
node serve.mjs        # open http://localhost:8123/smoke.html for the browser check
npm pack --dry-run
```

The public surface is a solver API inspired by HiGHS (`createSolver()`:
`createModel`, `addVar`, `addRow`, `run({callbacks})`, `getSolution`,
`info.get`, …) over the raw wasm-bindgen `Model`, with `onIncumbent` /
`interrupt` callbacks exposing partial solutions. See `npm/README.md`.

## Principles

1. Data-oriented: flat arrays + indices, no per-variable objects, no owning pointers.
2. The model is immutable and shared; mutable state is per-solve/per-worker.
3. `check_solution` is the oracle — the solver's own status is never trusted.
4. Anytime, primal-first: feasible incumbents early, optimality optionally.
5. Lock-free hot path; parallelism via work-stealing + atomics (native only).

## License

MIT.
