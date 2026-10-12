# Optio performance — 2026-10-11

Current working tree (base commit `820ad59`, with existing uncommitted solver changes), rebuilt release native and nightly release-small WASM. Apple M1 Pro / arm64, Node 24.21.0. Native: HiGHS 1.15.1, GLPK 5.0 (swiglpk 5.0.13), SCIP 10.0.2 / SoPlex (PySCIPOpt 6.2.1). WASM: local highs-js HiGHS 1.15.1 (`04024d7`), glpk.js 5.0.0 / GLPK 5.0, Optio rebuilt into `/tmp/optio-perf-wasm`.

Optio is strong at finding planning incumbents and competitive on these LPs. It is not generally faster than mature solvers: lack of presolve makes Sudoku much slower, and it does not certify the planning incumbents within this budget.

## Method

- Native Optio uses the release binary, default single thread; HiGHS is explicitly one thread; SCIP maxnthreads=1; GLPK single thread. No parallel Optio portfolio was measured.
- Five planning LP relaxations and eight MIPs, all existing project fixtures. LP and Sudoku/TSP timings are medians of three solves; planning MIP quality is one run per solver with a nominal five-second limit. These small samples are directional, not a broad solver ranking. Time limits can overshoot (native GLPK large: 6.14s; WASM GLPK large: 5.61s).
- LP parsing, module initialization, process startup and checking are excluded. Native timings cover optimization; Optio LP timing excludes LP construction. WASM HiGHS/Optio time `run()` only; GLPK.js times its synchronous `solve()` call, which also includes model construction and result extraction. Therefore GLPK.js timing has a slightly wider scope. Optio bulk WASM loading is separately recorded in raw results (under 1ms on this suite).
- Native HiGHS parses the model for Optio and the independent checker. GLPK/SCIP parse the same LP files; variable names map their solutions to the shared model. WASM Optio/GLPK load the native-parsed arrays; WASM HiGHS parses the same LP. LP integrality is relaxed for all solvers.
- Solutions are checked arithmetically at absolute tolerance 1e-6. LP objectives match across solvers, but Optio LP row residuals on faithful (1.78e-6) and large (1.32e-6) fail that strict check, in both runtimes. Asterisks mark those results. All returned Optio planning MIP incumbents pass.
- System load is uncontrolled; a short size-compression calculation ran during part of the native planning measurements. No browser startup, peak memory, commercial solver, CBC, CP-SAT, or SCIP-WASM benchmark was performed.

## Native LP relaxation (ms)

| Fixture | Optio | HiGHS | GLPK | SCIP |
| --- | ---: | ---: | ---: | ---: |
| s | 1.0 | 2.0 | 5.8 | 6.0 |
| faithful | 2.2* | 1.4 | 4.7 | 2.7 |
| tight | 38.7 | 47.4 | 195.2 | 35.4 |
| m | 37.5 | 30.5 | 225.9 | 35.8 |
| l | 209.5* | 394.8 | 954.9 | 190.6 |

Optio is 1.96x / 1.23x / 1.88x faster than HiGHS on small / tight / large; 1.60x / 1.23x slower on faithful / medium. Large is fastest with SCIP (190.6ms), with Optio at 209.5ms and HiGHS at 394.8ms. Large Optio carries the residual caveat above.

## WASM LP relaxation (ms, Node)

| Fixture | Optio | HiGHS | GLPK |
| --- | ---: | ---: | ---: |
| s | 1.4 | 3.4 | 4.5 |
| faithful | 3.9* | 2.7 | 3.9 |
| tight | 80.5 | 63.2 | 97.3 |
| m | 79.5 | 43.3 | 100.4 |
| l | 426.1* | 529.8 | 571.1 |

Optio beats HiGHS on small and large, trails on faithful, tight and medium. Optio WASM/native ratio is roughly 1.4–2.1x on these LPs; runtime and build flags differ, so this is not an inherent WASM penalty.

## Native planning objective at five seconds (lower is better)

| Fixture | Optio | HiGHS | GLPK | SCIP |
| --- | ---: | ---: | ---: | ---: |
| s | 692.79 | 711.44 | 728.32 | 722.21 |
| faithful | 258.26 | 258.26 | 258.26 | 258.26 |
| tight | 5,044.72 | 7,468.65 | No plan | 5,791.64 |
| m | 4,435.53 | 5,526.20 | No plan | 6,367.31 |
| l | 10,210.02 | 11,048.22 | No plan | 11,178.07 |

Optio beats HiGHS by 2.6%, 32.5%, 19.7%, 7.6% on small/tight/medium/large and ties faithful. It also beats SCIP incumbents on those four cases. SCIP proves faithful optimal in 4.87s; Optio returns TimeLimit on every planning fixture. Native GLPK finds no feasible incumbent on tight/medium/large in this budget. At one second Optio objectives are small 692.79, faithful 259.26, tight 5278.00, medium 4435.53, large 10210.02 — all independently feasible.

## WASM planning objective (five-second cap)

| Fixture | Optio | HiGHS | GLPK |
| --- | ---: | ---: | ---: |
| s | 692.79 | 1,918.25 | 740.41 |
| faithful | 258.26 | 580.35 | 258.26 |
| tight | 5,044.72 | 18,601.96 | 9,060.42 |
| m | 4,435.53 | 8,764.67 | 10,060.71 |
| l | 10,210.02 | 21,469.22 | No plan |

The local HiGHS WASM artifact stops after 72–1469ms and reports Optimal for all five planning models despite objectively better independently feasible solutions from Optio (and GLPK on four). This is a correctness problem in this particular local baseline, not evidence that HiGHS generally mis-solves MIPs, and its fast returns must not be interpreted as equivalent-quality speed wins. Optio returns TimeLimit on all five. GLPK returns feasible incumbents on four, none on large. Optio WASM and native reach the same five-second objectives here.

## Other MIPs (ms, median of three)

Native:

| Fixture | Optio | HiGHS | GLPK | SCIP |
| --- | ---: | ---: | ---: | ---: |
| sudoku-easy | 51.0 | 1.1 | 0.5 | 1.7 |
| sudoku-hard | 80.6 | 1.7 | 0.6 | 2.0 |
| tsp-12 | 19.4 | 128.4 | 28.0 | 85.9 |

WASM:

| Fixture | Optio | HiGHS | GLPK |
| --- | ---: | ---: | ---: |
| sudoku-easy | 66.7 | 1.3 | 1.1 |
| sudoku-hard | 105.8 | 1.9 | 1.2 |
| tsp-12 | 35.1 | 145.1 | 18.0 |

All these results pass feasibility checks and return optimal statuses (Sudoku objective 0; TSP-12 objective 279). Optio beats native HiGHS 6.6x on this TSP instance, but trails HiGHS approximately 47x on both native Sudoku fixtures. In WASM, GLPK is fastest on TSP; Optio still beats HiGHS 4.1x. Optio trails WASM HiGHS approximately 51–55x on Sudoku. These are individual fixtures, not general TSP/Sudoku rankings.

## WASM binary payload

| Build | Raw bytes | Brotli quality 11 bytes |
| --- | ---: | ---: |
| Optio | 146,861 | 57,061 |
| HiGHS | 2,926,666 | 708,768 |
| GLPK | 294,072 | 107,111 |
| HiGHS_minimal | 1,379,690 | 403,186 |

Binary-only, excludes JS loaders/adapters. Optio is 12.4x smaller than the full local HiGHS WASM and 1.88x smaller than GLPK WASM after Brotli. The stripped size-focused HiGHS artifact is 7.07x larger than Optio, with a narrower feature surface; it was sized, not speed-benchmarked in this run.

Raw results and machine-specific runner snapshots accompany this file. The runners retain absolute paths and temporary dependency locations used during this session.
