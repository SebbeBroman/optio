# Real application scheduling: shared native/WASM improvements

Further speed improvements and repeated measurements are documented in
[the follow-up report](../planning-speed-2026-10-11/README.md). The results below
record the first successful app-model implementation.

All three exported app fixtures now reach a checked optimum and a closing lower
bound, with and without the greedy start. The medium optimum is **3096.80**;
3096.90 was the earlier HiGHS result at its default relative gap.

## Matched results

Native HiGHS 1.15.1 and Optio WASM receive the identical detached CSR, column
order, and (for seeded runs) sparse start. Both request zero relative/absolute
gaps. Both use one thread. Limits are small 5 s, medium 10 s, large 20 s.
Times cover solver execution, excluding process startup/model loading. These
are individual paired runs, not statistical latency distributions.

| Case | Start | Before WASM objective | Current WASM objective | Current status | WASM wall | HiGHS wall | HiGHS result |
|---|---|---:|---:|---|---:|---:|---|
| Small | greedy | 314.90 | 314.90 | Optimal | 3.6 ms | 3.7 ms | 314.90 Optimal |
| Small | none | 314.90 | 314.90 | Optimal | 4.8 ms | 0.2 ms | 314.90 Optimal |
| Medium | greedy | 4979.40 | 3096.80 | Optimal | 88.1 ms | 565.2 ms | 3096.80 Optimal |
| Medium | none | no incumbent | 3096.80 | Optimal | 87.6 ms | 422.9 ms | 3096.80 Optimal |
| Large | greedy | 18379.80 | 4028.40 | Optimal | 149.7 ms | 17,243.5 ms | 4028.40 Optimal |
| Large | none | no incumbent | 4028.40 | Optimal | 143.9 ms | 20,022.6 ms | 7443.00 TimeLimit |

Seeded medium is **6.4× faster** than native HiGHS, and seeded large **115×**.
The small unseeded case still favors native HiGHS by about 5 ms. The old medium
and large seeded runs consumed their full limits without improving the start.

The original LP files were independently parsed and also proved, with complete
witnesses in `app-lp-proofs.json`. Their exact-hash conservative bounds are now
registered in `harness/fixtures/certified-bounds.json`.

Native Optio also proves all three at requested thread counts **1 and 8**.
Medium takes 40.1/29.6 ms; large takes 62.2/64.6 ms. These paths need zero B&B
nodes, so creating multiple search workers would add overhead.

## What changed

The old aggregate-transfer heuristic could not reconstruct the app's day
choices, multiple rounds, AM/PM splits, and one-ring-per-session constraints.
The new CSR-based detector shares identical normalized templates: **28 groups
become 6 template classes**, and **72 groups become 3**. It does not inspect
variable names. Day-choice dynamic programming merges equivalent aggregate
session-load states and retains the cheapest representative, avoiding searches
through permutations of identical groups. Fixed arrays avoid allocations in
the DP state loop in WASM.

Balanced packing splits round demands between sessions while using at most one
ring per group in each session. It fills ring-capacity residues before placing
whole round chunks. Every returned point is checked against every model row,
box bound and integrality requirement. Supplied starts are reported first and
are retained if the proposed plan is worse or invalid.

General B&B still has no automatic symmetry detection. The original app model
already supplies `ring_order_*` inequalities; the new path respects those
through balanced packing. Exact search remains available for other models.

## The bounds are actual certificates

| Case | Primal objective | Reported conservative lower bound |
|---|---:|---:|
| Small | 314.90 | 314.89999998612154 |
| Medium | 3096.80 | 3096.799999999998 |
| Large | 4028.40 | 4028.399999740922 |

The first certificate combines original row inequalities/equalities, then
minimizes every residual objective coefficient over its original column box.
Outward-rounded interval arithmetic keeps the result conservative.

For medium, an integer peak certificate closes the remaining continuous-bound
gap. It validates complete round-to-count equalities and one-hot day choices
from the original rows, enumerates all aggregate day counts (relaxing other
constraints), and uses `peak >= duration * ceil(total_matches / ring_count)`.
All other objective terms receive conservative box bounds. Incomplete round
definitions, duplicate row entries, mixed-session round demands, differing
match durations, and excessive DP state counts cause this certificate to
return no bound. It never derives a bound from a heuristic pattern subset.

Optimal status uses the existing MIP floating-point tolerance. The conservative
bound is preserved in `mip_dual_bound`; it is not replaced with the incumbent.
The largest certificate/primal difference here is 2.6e-7.

## Where time goes

Phase counters include the new `time_bound_certificate_secs` / native
`boundCertificate` timing. In the seeded runs, medium spends roughly 51 ms in
planning and 34 ms in certification; large spends 121 ms in planning, 22 ms in
presolve and 3 ms in certification. Both avoid all node LP work and root cuts.
Full incumbent timestamps and worker counters are retained in the JSON.

## Validation and artifacts

- 50 active Rust tests pass; the two existing ignored tests remain ignored.
- 44/44 classic WASM objective/status/feasibility regressions pass.
- All 61 supported native fixtures pass at 3 s / 8 requested threads
  (58 archived fixtures plus the three new original-LP app proofs). The existing semi-integer fixture remains unsupported; its record is
  retained but excluded from semantic assertions. This smoke sweep does not
  replace the earlier 120-second certification report.
- All six app seed configurations prove optimal; native requests of 1 and 8
  threads do too. HiGHS witnesses also pass an independent arithmetic check.
- Node and web packages were rebuilt, their WASM bytes match, and the web ES
  module independently proves the large fixture.

`results.json` contains the full before/after/HiGHS witnesses, checks, bounds,
incumbents and phase timings. `native.json` contains the six native runs;
`metadata.json` records fixture/build hashes and reference version. Regression
records are in `wasm-regressions.json` and `native-regressions.json`.

The shared scheduling and certificate code increases WASM from 175,965 to
265,352 bytes, and Brotli from 67,222 to 94,533 bytes. See `build.json`.

## Reproduction

```sh
cargo build --release -p optio-bench --features parallel
cargo test -p optio-core --features parallel --release
bash crates/wasm/build.sh
WASM_BINDGEN_TARGET=web PKG_DIR=pkg-web bash crates/wasm/build.sh

# Quick app repro, with matched per-case reference limits and zero HiGHS gap:
JSON_OUT=/tmp/app-seeded.json node harness/tools/planning-app-loss.mjs
NO_SEED=1 JSON_OUT=/tmp/app-unseeded.json node harness/tools/planning-app-loss.mjs

# Full matched CSR comparison. Requires the frozen before package:
harness/tools/oracle-venv/bin/python harness/tools/planning-app-bench.py \
  --before /tmp/optio-app-before/pkg/optio_wasm.js \
  --after pkg/optio_wasm.js --out /tmp/app-comparison

OPTIO_WASM="$PWD/pkg/optio_wasm.js" OPTIO_REGRESSION_OUT=/tmp/app-wasm-regressions.json \
  node harness/tools/wasm-certification-smoke.mjs
harness/tools/oracle-venv/bin/python harness/tools/native-certification-smoke.py \
  --archived-only --time 3 --threads 8 --out /tmp/app-native-regressions.json
```

The recorded final comparison reused unchanged frozen before records, checked
against fixture/seed hashes, and reran every current solve. The unchanged HiGHS measurements were retained
after the final callback-only fix; all their witnesses were rechecked.
For a fresh comparison omit `--reuse-before` (the default).
