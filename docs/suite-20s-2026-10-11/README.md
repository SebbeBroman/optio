# Full suite comparison, 20-second limit — 2026-10-11

Native Optio is faster on most commonly completed fixtures and finds better schedules on several open planning cases, while native HiGHS proves two more optima and finds four more incumbents. Both Optio builds return the same 48 validated optima. The local WASM HiGHS build makes ten claims contradicted by checked witnesses; its short total runtime cannot be treated as a reliable performance result.

## Coverage

All 62 LP fixtures were attempted. Aggregate results cover the **61 supported fixtures**; `semi-integer.lp` is reported in the raw data but excluded because Optio does not support its variable domain. There are five infeasible/unbounded fixtures. Counts below require independently checked witnesses and no contradiction against a better checked witness or archived reference proof.

| Result | Optio native | Optio WASM | HiGHS native | HiGHS WASM |
|---|---:|---:|---:|---:|
| Validated optimal | 48 | 48 | 50 | 46 |
| Correct infeasible/unbounded classifications | 5 | 5 | 5 | 5 |
| Total completed correctly | 53/61 | 53/61 | 55/61 | 51/61 |
| Checked feasible witnesses | 54 | 54 | 58 | 55 |
| Contradicted claims | 0 | 0 | 0 | 10 |
| Total solve time | 152.88 s | 170.76 s | 133.02 s | 7.81 s* |

The WASM HiGHS build reports 53 Optimal statuses; seven are contradicted by better checked schedules. Three additional Infeasible statuses are contradicted by feasible witnesses. Its 46 remaining optimal results agree with the checks and references; this does not establish reliability of the build on arbitrary inputs. The errors persist with native-parsed arrays, excluding LP parser differences as an explanation. See `summary.json` for all affected fixtures.

## Speed and solution quality

On the **51 cases completed correctly by both native Optio and native HiGHS**, Optio is faster on 48 and HiGHS on three. Median Optio/HiGHS solve-time ratio is **0.123**, about 8.1× faster. These results are dominated by small generated fixtures and are single samples, not a broad solver ranking.

Comparing WASM Optio with native HiGHS on those same cases, Optio is faster on six and slower on 45; the median time ratio is 9.90. This crosses runtimes and is provided as a baseline, not an equivalent-runtime comparison. On the 51 cases completed correctly by both WASM builds, WASM Optio is faster on 50, with median time ratio 0.306. That subset excludes the ten contradicted WASM HiGHS results.

Against native HiGHS, native Optio has six better final objectives, zero worse objectives, and 48 ties when both return checked witnesses. WASM Optio has five better objectives, one worse objective, and 48 ties. HiGHS alone finds a checked witness on four fixtures: `2122`, `life_goe.mod`, and the two unbounded fixtures. Both solvers have no witness on the three infeasible fixtures. A feasible point on an unbounded model is counted as a witness, not an optimum.

Representative planning results follow. Parentheses show solver status. Times include the actual measured overrun where applicable.

| Fixture | Optio native objective / time | Optio WASM objective / time | HiGHS native objective / time |
|---|---|---|---|
| app-large | 4,028.40 / 11 ms (Optimal) | 4,028.40 / 50 ms (Optimal) | 6,183.00 / 20.03 s (limit) |
| app-medium | 3,096.80 / 10 ms (Optimal) | 3,096.80 / 63 ms (Optimal) | 3,096.80 / 449 ms (Optimal) |
| export | 536,748.44 / 214 ms (Optimal) | 536,748.44 / 484 ms (Optimal) | 536,945.00 / 20.03 s (limit) |
| faithful | 258.26 / 20.02 s (limit) | 258.26 / 21.16 s (limit) | 258.26 / 9.30 s (Optimal) |
| large | 10,201.77 / 20.03 s (limit) | 10,210.02 / 20.06 s (limit) | 11,048.22 / 20.02 s (limit) |
| medium | 4,435.53 / 20.01 s (limit) | 4,435.53 / 20.02 s (limit) | 5,125.47 / 20.02 s (limit) |
| small | 671.75 / 20.19 s (limit) | 686.54 / 25.92 s (limit) | 675.91 / 20.00 s (limit) |
| tight | 5,044.72 / 20.03 s (limit) | 5,044.72 / 20.01 s (limit) | 5,461.89 / 20.01 s (limit) |

Optio's eight unresolved fixtures in both runtimes are `2122`, `exceeds_stack`, `life_goe.mod`, `planning-faithful`, `planning-l`, `planning-m`, `planning-s`, and `planning-tight`. Native HiGHS leaves six unresolved: app-large, export, large, medium, small and tight planning models. `exceeds_stack` reaches the same objective 1,354 with Optio but is proved only by HiGHS; native Optio returns early with an unresolved bound after 11.51 seconds.

## Method and limits

- Apple M1 Pro, arm64, Node 24.21.0; native HiGHS 1.15.1; local WASM HiGHS 1.15.1 (`04024d7`). Native Optio uses the current release build, WASM uses release-small / wasm-opt -Oz. Binary hashes are in `results.json`.
- One configured native thread; WASM builds are single-threaded. Nominal 20-second limit per solver/model, zero requested absolute/relative gaps, no shared incumbents or warm starts.
- Sequential solves, with one sample per fixture/runtime. WASM uses a fresh Node process per solve, including the first invocation's JIT work in solver timing. There is no warmup exclusion. These are Node measurements, not browser timings.
- Parsing, ingestion, process startup, module initialization and independent checking are excluded from recorded solve time. System load is uncontrolled; no competing solver benchmark was intentionally run during the final sweep.
- Native HiGHS parses the LP and exports CSR arrays for every solver. Original HiGHS integrality codes are preserved for WASM HiGHS, including the excluded semi-integer fixture. Every returned witness is independently checked at absolute tolerance 1e-6. Solver numerical tolerances still govern proof status; these are not rational certificates.
- The 20-second Optio limit is cooperative. The notable WASM overruns are `planning-faithful` at 21.16 seconds and `planning-s` at 25.92 seconds. The latter's final candidate was already emitted at 10.01 seconds; the overrun occurred during unsuccessful proof search. This comparison does not establish strict deadline compliance.
- HiGHS's MIP dual-bound field is inapplicable to nonoptimal continuous LP solves. Those bounds are recorded as null; the original unused zero value is retained separately where collected.

`results.json` contains raw checked witnesses and timings; `validated-results.json` adds validation judgments, `summary.json` contains aggregates, and `fixtures.csv` lists every solver/model result. Existing fixture files plus their hashes reproduce the input models; generated CSR copies are omitted to avoid duplicating the suite.

```sh
harness/tools/oracle-venv/bin/python harness/tools/suite-compare.py \
  --native /tmp/optio-aggregate-proof/native \
  --wasm /tmp/optio-aggregate-proof/pkg/optio_wasm.js \
  --time 20 --out /tmp/optio-suite-20s/results.json
python3 harness/tools/suite-summary.py /tmp/optio-suite-20s/results.json
```
