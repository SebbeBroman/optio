# Five Rust memory/work optimizations — 2026-10-11

All five changes are implemented. On the real app scheduling fixtures, medium and large native solves are about **30% and 33% faster**; fresh-process WASM solves are about **8–9% faster**. Checked objectives and conservative closing bounds are preserved. These fixtures use no simplex iterations or B&B nodes.

## Matched before/after

Medians of 11 alternating fresh-process runs per build/configuration on Apple M1 Pro, Node 24.21.0. Solver execution only; process startup, module initialization, parsing and model ingestion are excluded. Native requests one or eight threads; this planning path needs no parallel search. WASM has seeded and unseeded configurations. System load is uncontrolled.

| Runtime | Case | Configuration | Before ms | After ms | Time reduction |
|---|---|---|---:|---:|---:|
| wasm | small | greedy start | 3.333 | 3.382 | -1.5% |
| wasm | small | no start | 4.659 | 4.755 | -2.1% |
| wasm | medium | greedy start | 66.183 | 60.603 | 8.4% |
| wasm | medium | no start | 66.620 | 60.814 | 8.7% |
| wasm | large | greedy start | 56.003 | 51.440 | 8.1% |
| wasm | large | no start | 54.585 | 49.583 | 9.2% |
| native | small | 1 threads | 0.060 | 0.054 | 9.9% |
| native | small | 8 threads | 0.060 | 0.052 | 13.9% |
| native | medium | 1 threads | 13.389 | 9.390 | 29.9% |
| native | medium | 8 threads | 12.927 | 8.800 | 31.9% |
| native | large | 1 threads | 15.718 | 10.597 | 32.6% |
| native | large | 8 threads | 16.152 | 10.701 | 33.7% |

Small fresh-process WASM is 1–2% slower in this sample (about 0.05–0.10 ms); no universal speedup is claimed.

## Native incremental stages

A separate rotating comparison includes every frozen cumulative stage, 11 measured samples after two initial samples per fixture/build. All 234 runs pass an independent checker at 1e-9, retain the known optimum, and close the reported conservative bound within 1e-6. Differences in stages that do not exercise a code path are measurement noise.

| Cumulative build | Medium ms | Large ms |
|---|---:|---:|
| baseline | 13.120 | 15.919 |
| 1-scratch | 12.755 | 14.283 |
| 2-rows | 10.985 | 10.970 |
| 3-traces | 10.039 | 10.863 |
| 4-scoring | 9.248 | 10.989 |
| 5-certificates | 9.131 | 10.591 |

These are incremental contributions in this order, not isolated effects independent of other changes. Row temporaries produce the largest gain. Trace/scoring changes help medium; large uses direct packing and skips the DP. Certificate reuse adds a small native large improvement; medium does not perform the redundant large fast-path check.

## Changes

1. Reuse weighted-presolve `unique` and `activities` vectors across rows. Preserve the existing sort, duplicate rejection, rounding margins and update order.
2. Replace temporary CSR row collections with direct iteration in shape detection, reconstruction and both certificates. Reuse continuous-row scratch and the projection coefficient map. Owned feeder/demand vectors remain where the detected shape needs them.
3. Replace the active DP value `(cost, [u16; 40])` with `(cost, trace_id)`: 88 to 16 bytes on native. Store 8-byte parent/choice records in a contiguous arena and reconstruct histories only for candidate plans. Retained traces span earlier layers; this reduces active-table traffic, not necessarily every measure of total memory.
4. Partition the best 64 scores before sorting them, with an ordinal tie-breaker to preserve stable tie order. Memoize balanced session costs by integer count only when **all** session programs have local, topologically ordered inputs. Otherwise retain original evaluation so cached calls cannot omit writes needed by another session.
5. Lazily reuse the row certificate within a single planner invocation, bound to the same immutable model. The solver still independently constructs/checks its certificate against the original model, rather than reusing the presolved model's bound.

## Warm WASM

Node/V8, not an actual browser measurement. Before/after builds alternate within one process, with 15 initial solves discarded and medians over the next 15; module/model initialization is excluded. All 180 solves pass independent feasibility and bound checks.

| Case | Before ms | After ms | Time reduction |
|---|---:|---:|---:|
| small | 0.070 | 0.058 | 17.6% |
| medium | 43.565 | 37.375 | 14.2% |
| large | 24.369 | 22.423 | 8.0% |

## Validation and size

- 52 active Rust tests pass; two previously ignored tests remain ignored.
- 44/44 classic WASM status/objective/feasibility regressions pass.
- All 61 supported native fixtures pass the archived-reference, 3-second/eight-thread correctness smoke sweep. One existing unsupported semi-integer fixture is retained separately; it is excluded from semantic assertions. This sweep does not assert every open model was solved to optimum.
- Four browser ES-module checks pass: medium/large, with/without a supplied start. Node and web WASM bytes match.
- Every run in the 264-run matched planning comparison retains the known optimum, valid witness and conservative closing bound. Native stage runs additionally use the independent detached-model checker.
- WASM payload: 267,631 → 272,861 raw bytes; 95,238 → 96,858 Brotli bytes (+1,620, about 1.7%). Loader excluded. Build profile remains release-small plus wasm-opt -Oz.
- No hardware cache-miss counters or peak-memory measurements were collected. Memory-locality explanations are supported by code changes and timings, not a measured cache-miss rate.

## Reproduction

Frozen builds and stage executables remain under `/tmp`; hashes and base commit are in `metadata.json`, `native-stages.json` and the paired records.

```sh
cargo test -p optio-core --features parallel --release
cargo build --release -p optio-bench --features parallel
bash crates/wasm/build.sh
WASM_BINDGEN_TARGET=web PKG_DIR=pkg-web bash crates/wasm/build.sh

python3 harness/tools/planning-speed-bench.py \
  --before /tmp/optio-rust-opt-baseline --after /tmp/optio-rust-opt-final \
  --out /tmp/optio-rust-opt-repeat --repeats 11

node harness/tools/planning-rust-stages.mjs --out /tmp/optio-native-stages.json \
  --build baseline=/tmp/optio-rust-opt-baseline/native \
  --build 1-scratch=/tmp/optio-rust-opt-stages/1-scratch \
  --build 2-rows=/tmp/optio-rust-opt-stages/2-rows \
  --build 3-traces=/tmp/optio-rust-opt-stages/3-traces \
  --build 4-scoring=/tmp/optio-rust-opt-stages/4-scoring \
  --build 5-certificates=/tmp/optio-rust-opt-stages/5-certificates

node harness/tools/planning-warm-compare.mjs \
  /tmp/optio-rust-opt-baseline/pkg/optio_wasm.js \
  /tmp/optio-rust-opt-final/pkg/optio_wasm.js /tmp/optio-warm-wasm.json
```
