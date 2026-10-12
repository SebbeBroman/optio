# Planning candidate, quality and proof challenge — 2026-10-11

Update: the later [integer-load proof implementation](../planning-proof-2026-10-11/README.md) closes this model in approximately 213 ms native / 479 ms WASM with a five-second budget. The measurements below preserve the earlier heuristic-only stage.

On the downloaded `planning-model (2).lp`, Optio now produces checked schedules quickly. The first two latency targets are met in the measured native and WASM runs. The 20-second optimality target remains open.

The downloaded LP parses to the same model arrays as `harness/fixtures/planning-export.lp`: 5,602 columns, 5,085 rows, 41,799 nonzeros, 128 independent scheduling blocks, three sessions and 15 ring/session load groups. The two LP text hashes differ; see `metadata.json`.

## Measurements

Three sequential fresh-process runs per runtime and short budget, with zero requested relative gap. Native uses one thread; WASM uses Node/V8. These are solver times, excluding parsing, process startup, module initialization and model ingestion. WASM measurements are not an actual browser benchmark. No initial samples are excluded and system load is uncontrolled.

For this benchmark, “good” means an objective at most 0.5% above the archived best checked objective of 536,748.44, i.e. at most 539,432.1822. This reference threshold is used only by the benchmark, never by the search implementation. It is not an optimality certificate.

With a five-second solve budget:

- Native median first candidate: **37.72 ms**. Median first good candidate: **500.46 ms**. Final objectives: 537,328.40, 537,328.40 and 537,519.92. Median final objective is 0.108% above the archived best; its reported dual gap is 0.224%.
- WASM median first candidate: **92.47 ms**. Median first good candidate: **881.40 ms**. Final objective in all three runs: **538,304.36**, 0.290% above the archived best, with reported dual gap 0.405%.

With a two-second budget, median first candidate/good times were 38.12/501.41 ms native and 92.18/868.68 ms WASM. Final objectives were 538,692.08 native and 539,277.80 WASM. The LP search had not recovered its stronger bound within these budgets; the reported bound was only 380,604.20. A close objective relative to the archived witness must not be confused with a closed solver proof gap.

One separate 20-second run per runtime:

- Native: first candidate **37.30 ms**, first good candidate **499.50 ms**.
- WASM: first candidate **92.43 ms**, first good candidate **872.98 ms**.
- Both returned checked objective **537,136.52**, bound **536,124.20**, absolute gap **1,012.32**, relative gap **0.1885%**, and time-limit status. The objective is 0.0723% above the archived best. **Neither proved optimality.**

The timing targets refer to incumbent callbacks. Full solves continue attempting proof until their requested budget expires. Deadlines are cooperative: two-second runs finished in approximately 2.03–2.06 seconds and five-second runs in 5.02–5.06 seconds. All 14 final witnesses were independently checked against the original detached model at tolerance 1e-9; all row, bound and integrality violations were zero. Improvement callbacks from the new exchange search are also checked against the original model before emission.

`short-runs.json` and `proof-runs.json` contain complete witnesses, incumbent timing histories, checks, build hashes, model hash and phase timings. `diagnostic-before.json` retains the earlier five-second diagnosis: the previous Optio path found no incumbent. Those baseline diagnostic jobs partly overlapped, so they are not a controlled timing comparison.

## Implementation

- Keep one-hot scheduling choices out of the round-to-load map. A single eligible ring can otherwise make `x - y = 0` look like a round definition and cause the planner to reject the choice block.
- Include feeder duration in the shared pattern cache key. Structurally identical blocks with different durations must not reuse weighted load profiles.
- Add coordinate descent over feasible local patterns using actual weighted ring loads, followed by bounded exchanges between pairs of blocks. No ring-label symmetry or uniform-duration assumption is used by this search. Global load rows and auxiliary bounds guide construction with penalties; original row checks remain authoritative for accepting schedules.
- Retain useful representative candidates even when they cannot close a certificate, and preserve better supplied incumbents. Keep the previous direct packing/certificate path for the uniform planning fixtures; use weighted search when the small DP or direct packing does not handle the model.
- Stream strictly improving, checked incumbents during exchange search instead of waiting for the entire polishing budget to finish.

The existing general branch-and-bound and original-row bound verification remain responsible for proof. Sampled patterns and pair-move shortlists are heuristic restrictions and **are never used to certify optimality**. Unsupported planning structures fall back to the general solver. These measurements establish performance on this model, not a universal latency guarantee for arbitrary mixed planning models.

## Validation

New generated models cover four durations (420, 720, 900 and 1,110 seconds), multiple overlapping ring eligibility sets including singleton sets, precedence, one-ring-per-session constraints, hard capacities and soft capacity penalties. Tests compare every cached pattern load with the actual original-column load, verify a 48-block candidate and incumbent preservation, reject candidates under impossible hard capacities, and check that streamed improvements are feasible and strictly improving.

- 56 active Rust tests pass; two existing ignored tests remain ignored. This includes the generated mixed-model and streaming tests.
- 44/44 classic WASM regressions pass.
- All 61 supported native fixtures pass the three-second, one-thread archived-reference correctness sweep. A separate unsupported semi-integer record is retained but excluded from semantic assertions.
- Six independently checked native/WASM small, medium and large planning solves retain their known optima and closing bounds.
- Formatting and diff checks pass.

The validation logs and native/WASM regression records are stored alongside this report. Regression timings were collected with validation jobs running concurrently and are not used as performance comparisons.

WASM payload size versus the previous five-optimization build: 272,861 → 286,171 raw bytes and 96,858 → 100,639 Brotli bytes (+3,781 bytes, about 3.9%). Loader excluded. The build remains the size-oriented release-small profile with wasm-opt -Oz.

## Reproduction

Build and run against a detached CSR model parsed from the LP:

```sh
cargo build --release -p optio-bench
PKG_DIR=/tmp/optio-planning-challenge/pkg bash crates/wasm/build.sh
node harness/tools/planning-challenge.mjs \
  --native target/release/optio-bench \
  --wasm /tmp/optio-planning-challenge/pkg/optio_wasm.js \
  --model /tmp/optio-download-planning-2.json \
  --limits 2,5 --repeats 3 --out /tmp/planning-short.json
node harness/tools/planning-challenge.mjs \
  --native target/release/optio-bench \
  --wasm /tmp/optio-planning-challenge/pkg/optio_wasm.js \
  --model /tmp/optio-download-planning-2.json \
  --limits 20 --repeats 1 --out /tmp/planning-proof.json
cargo test --release -p optio-core --features parallel
```

`harness/tools/native-oracle.py` exposes `model_json(highs.getLp())` for regenerating detached models with highspy. The frozen measured native and WASM builds remain in `/tmp/optio-planning-challenge`.
