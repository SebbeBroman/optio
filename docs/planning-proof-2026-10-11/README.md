# Planning proof challenge — 2026-10-11

Optio now proves the downloaded `planning-model (2).lp` optimal at objective **536,748.44** using a structural integer-load relaxation and a checked reconstruction of the original schedule. With a five-second budget, median full solve times are **212.68 ms native** and **478.52 ms WASM**. All five repetitions in each runtime returned Optimal. This meets the requested candidate under two seconds, good candidate under five seconds, and optimality under twenty seconds on this model.

## Measurements

Five sequential fresh-process runs per runtime and budget, one thread, zero requested gaps. Timing excludes LP parsing, process startup, module initialization and model ingestion. WASM runs use Node/V8, not a browser. No samples are excluded. The final matched runs started after regression jobs completed; system load is otherwise uncontrolled. Full witnesses, checks, callbacks and build/model hashes are in `proof-runs.json`.

| Runtime | Budget | Median first candidate | Median first good/optimal witness | Median full solve | Optimal runs |
|---|---:|---:|---:|---:|---:|
| Native | 2 s | 35.70 ms | 213.40 ms | 213.46 ms | 5/5 |
| Native | 5 s | 35.80 ms | 212.62 ms | 212.68 ms | 5/5 |
| Native | 20 s | 35.66 ms | 211.47 ms | 211.52 ms | 5/5 |
| WASM | 2 s | 92.24 ms | Not reached | 2,051.14 ms | 0/5 |
| WASM | 5 s | 92.35 ms | 478.25 ms | 478.52 ms | 5/5 |
| WASM | 20 s | 91.98 ms | 477.44 ms | 477.70 ms | 5/5 |

“Good” is the existing benchmark threshold of 0.5% above 536,748.44. In every successful proof run, the first good candidate is also the optimal witness. The five-second baseline previously took median 500.46 ms native / 881.40 ms WASM to reach this threshold, and neither runtime proved optimality in twenty seconds.

The two-second WASM deadline leaves too little time in the nested assignment heuristic to reconstruct the optimal loads. Those runs return a checked candidate of 540,054.68 and the same optimal lower bound; they do not claim Optimal. Deadlines remain cooperative, with approximately 51 ms overrun. This is a measured short-budget limitation, despite the longer-budget proof finishing in under half a second.

All 30 final witnesses passed the independent JavaScript checker at tolerance 1e-9. Successful proofs have objective 536,748.4400000005, conservative lower bound 536,748.4387727398, absolute numerical gap 0.00122726, and relative gap 2.2865e-9. Maximum original row violation is 4.6566e-10; bound and integrality violations are zero. Optimal status uses the solver's existing numerical closing tolerance, 1e-7 × (1 + |objective|), even with zero requested gaps. This is a floating-point MIP certificate, not an exact rational certificate.

## Why the proof works

The continuous load relaxation stops at 536,124.20 because it can split total time into unrealizable fractional loads. The actual durations here are 720 and 900 seconds, so every ring load is an integer multiple of 180 seconds. Preserving that integrality raises the aggregate optimum to 536,748.44.

The implemented relaxation retains original one-hot choices and all continuous auxiliaries, replaces complete weighted feeder expressions with integer group loads scaled by their duration GCD, and derives session totals from weighted sums of original demand equalities. Original placement restrictions may be dropped, so its dual bound remains a lower bound on the original minimization problem. No sampled patterns or heuristic shortlists enter this proof. The downloaded model reduces from 5,602 columns / 5,085 rows to 270 columns / 281 rows.

An optimal relaxation alone is insufficient. The second stage batches demands by their complete eligible-ring and duration vectors, solves integer assignment counts, disaggregates those counts, and reconstructs round and occupancy columns. It accepts the result only after checking every original row, column bound and integrality condition at 1e-9. An unliftable relaxation can contribute a bound but cannot create an original witness or prove original infeasibility. Successful runs use 55 search nodes and 641 LP iterations across both stages.

The construction reads coefficients and structure, without fixture names, ring symmetry, or a uniform-duration assumption. Mixed durations, overlapping ring eligibility, hard limits and soft penalties remain represented through retained rows and the final original-model check. Unsupported coefficient structures fall back to the existing solver; limits include positive integer durations and at most 128 load groups. A stronger aggregate bound need not be attainable under all original placement restrictions, so these timings are not a guarantee for arbitrary planning instances.

The existing exact scheduling certificate and its local polish take precedence when available. An initial integration violated the seeded medium fixture's zero-node requirement by using 361 search nodes. The ordering fix restores its original two callbacks, zero nodes and optimal result; the assertion remains unchanged.

## Validation and payload

- 59 active Rust tests pass; two existing tests remain ignored. New tests exhaustively project small mixed plans into the relaxation, compare its bound with an enumerated optimum, reject unsafe duplicate/coefficient/cost structures, and prevent unliftable loads from creating a witness.
- All 61 supported native fixtures pass archived-reference checks with a three-second budget. The unsupported semi-integer record is retained separately in `native-regressions.json`.
- All 44 classic WASM regressions pass.
- Four browser ES-module API checks pass for seeded/unseeded medium and large planning fixtures, including exact callback counts. These exercise the web module in Node, not browser timing.
- Formatting and diff checks pass.

The Node WASM payload grows from 286,171 to 323,871 raw bytes and from 100,639 to 112,159 Brotli bytes (quality 11), an increase of **11,520 compressed bytes / 11.4%**. Loaders are excluded. Both builds use the size-oriented release-small profile and wasm-opt -Oz.

`experiments/` preserves the continuous/integer/subset relaxation comparisons, derivations, assignment experiments and original lifted witness. Native HiGHS independently finds the same integer relaxation optimum and successfully lifts it. Additional eligibility-subset cuts did not improve the bound here and are omitted from the Rust implementation.

## Reproduction

```sh
cargo test --release --offline -p optio-core
cargo build --release --offline -p optio-bench
PKG_DIR=/tmp/optio-aggregate-proof/pkg bash crates/wasm/build.sh
node harness/tools/planning-challenge.mjs \
  --native target/release/optio-bench \
  --wasm /tmp/optio-aggregate-proof/pkg/optio_wasm.js \
  --model /tmp/optio-download-planning-2.json \
  --limits 2,5,20 --repeats 5 --out /tmp/planning-proof-runs.json
```

The detached CSR JSON was produced from the downloaded LP with `native-oracle.py`'s `model_json`. Its model arrays match `harness/fixtures/planning-export.lp`; source hashes are retained in the earlier challenge report. Experimental scripts are `harness/tools/planning-proof-experiment.py` and `harness/tools/planning-proof-lift.py`. They require the existing oracle virtual environment with highspy.
