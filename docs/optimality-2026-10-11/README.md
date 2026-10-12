# Sudoku performance and full-suite optimality — 2026-10-11

All 59 fixtures were attempted. Results: 49 optimal, 3 infeasible, 2 unbounded, 5 open, 0 bound conflicts.

## Method

Release Optio with the `parallel` feature and eight workers; native HiGHS 1.15.1 with eight configured threads. Each solver gets 120 seconds per fixture and zero requested absolute/relative MIP gap. HiGHS receives an independently checked Optio incumbent when available. Remaining cases receive a SCIP 10 concurrent portfolio with eight workers and a 120-second budget (including its one-second Optio seed). Solver benchmark runs are sequential; brief compilation and validation overlapped some runs. Final Sudoku medians were measured with other solver work stopped. Each two-minute problem/solver measurement is a single run. The first eight fixtures preceded the row-multiplier projection refinement; their valid bounds and reference proofs are retained, with the original native build hash in `build.json`. Subsequent records carry their native build hash. Parsing, process startup, and independent checking are excluded from solver times. Deadlines are checked cooperatively and can overshoot slightly. These are numerical solver proofs, not exact rational proof certificates. Bounds use the objective in the original model sense; maximization has an upper bound.

Optio now propagates unit-coefficient binary cardinality rows, returning fully fixed models without simplex. It computes LP Lagrangian bounds from unperturbed costs and bounds rather than using the primal LP objective; branch pruning and the reported global bound use these values. Open or numerically unresolved nodes cannot certify optimality. An exhausted search with a material remaining dual gap returns a feasible-solution status.

## Sudoku A/B

Medians of nine measured solves after two warmups, same new build with presolve disabled/enabled. Every solution is independently checked. The final timing pass ran after the full solver suite finished, without competing solver work. HiGHS was measured in the same runtimes, also with nine samples after two warmups. Native HiGHS used eight configured threads; WASM is single-threaded. The WASM reference comparison is restricted to Sudoku, whose zero-cost feasible solutions are trivially optimal.

| Fixture | Runtime | Presolve off (ms) | Presolve on (ms) | Speedup | HiGHS (ms) |
|---|---|---:|---:|---:|---:|
| sudoku-easy | native-1 | 49.657 | 0.076 | 657× | 1.081 |
| sudoku-easy | native-8 | 50.644 | 0.076 | 666× | 1.081 |
| sudoku-easy | wasm | 66.052 | 0.156 | 423× | 1.587 |
| sudoku-hard | native-1 | 78.571 | 0.173 | 455× | 1.650 |
| sudoku-hard | native-8 | 79.635 | 0.171 | 467× | 1.650 |
| sudoku-hard | wasm | 105.602 | 0.197 | 537× | 1.962 |

Both Sudoku fixtures are fixed completely, have exact objective/bound zero, and use zero LP iterations and zero search nodes.

## Certified objectives and bounds

| Fixture | Sense | Best feasible objective | Best bound | Gap | Result | Bound source |
|---|---|---:|---:|---:|---|---|
| 1451.lp | min | 2 | 2 | 0 | Optimal | HiGHS |
| 2122.lp | max | -187,612.944194 | -187,612.944194 | 0 | Optimal | HiGHS |
| exceeds_stack.lp | max | 1,354 | 1,354 | 0 | Optimal | HiGHS |
| fixed-binary.lp | min | 0 | 0 | 0 | Optimal | Optio |
| issue-2388.lp | min | 0 | 0 | 0 | Optimal | HiGHS |
| issue-2585.lp | min | -175.91 | -175.91 | 0 | Optimal | HiGHS |
| life_goe.mod.lp | min | 0 | 0 | 0 | Optimal | HiGHS |
| planning-faithful.lp | min | 258.26 | 258.26 | 0 | Optimal | HiGHS |
| planning-export.lp | min | 536,748.44 | 536,341.46 | 406.98 | Open | HiGHS |
| planning-l.lp | min | 9,945.03 | 9,064.740002 | 880.289998 | Open | HiGHS |
| planning-m.lp | min | 4,377.47 | 4,294.63 | 82.84 | Open | HiGHS |
| planning-s.lp | min | 661.14 | 660.921 | 0.219 | Open | SCIP |
| planning-tight.lp | min | 4,802.04 | 4,686.87 | 115.17 | Open | HiGHS |
| semi-integer.lp | max | 8.133333 | 8.133333 | 0 | Optimal | HiGHS |
| lp-01.lp | min | -51.25 | -51.25 | 0 | Optimal | HiGHS |
| lp-02.lp | min | -65.666668 | -65.666667 | 0 | Optimal | HiGHS |
| lp-03.lp | min | -124 | -124 | 0 | Optimal | HiGHS |
| lp-04.lp | min | -334.500001 | -334.5 | 0 | Optimal | HiGHS |
| lp-05.lp | min | -67.992425 | -67.992424 | 0 | Optimal | HiGHS |
| lp-06.lp | min | -34 | -34 | 0 | Optimal | HiGHS |
| lp-07.lp | min | -158.25 | -158.25 | 0 | Optimal | HiGHS |
| lp-08.lp | min | -45.000001 | -45 | 0 | Optimal | HiGHS |
| lp-09.lp | min | -112.500001 | -112.5 | 0 | Optimal | HiGHS |
| lp-10.lp | min | -274.750001 | -274.75 | 0 | Optimal | HiGHS |
| lp-11.lp | min | -165.958334 | -165.958333 | 0 | Optimal | HiGHS |
| lp-12.lp | min | -49.5625 | -49.5625 | 0 | Optimal | HiGHS |
| lp-13.lp | min | -134 | -134 | 0 | Optimal | HiGHS |
| lp-14.lp | min | -157.2125 | -157.2125 | 0 | Optimal | HiGHS |
| lp-15.lp | min | -194.975001 | -194.975 | 0 | Optimal | HiGHS |
| lp-16.lp | min | -87.833334 | -87.833333 | 0 | Optimal | HiGHS |
| lp-17.lp | min | -176.777778 | -176.777778 | 0 | Optimal | HiGHS |
| lp-18.lp | min | -33 | -33 | 0 | Optimal | HiGHS |
| lp-19.lp | min | -75.500001 | -75.5 | 0 | Optimal | HiGHS |
| lp-20.lp | min | -218.75 | -218.75 | 0 | Optimal | HiGHS |
| lp-21-infeasible.lp | min | — | — | — | Infeasible | — |
| lp-22-infeasible.lp | min | — | — | — | Infeasible | — |
| lp-23-unbounded.lp | min | -1 | — | — | Unbounded | — |
| lp-24-unbounded.lp | min | 0 | — | — | Unbounded | — |
| mip-01.mip.lp | min | 4.5 | 4.5 | 0 | Optimal | HiGHS |
| mip-02.mip.lp | min | -14.5 | -14.5 | 0 | Optimal | Optio |
| mip-03.mip.lp | min | -138 | -138 | 0 | Optimal | HiGHS |
| mip-04.mip.lp | min | -55.5 | -55.5 | 0 | Optimal | Optio |
| mip-05.mip.lp | min | -17 | -17 | 0 | Optimal | HiGHS |
| mip-06.mip.lp | min | 10 | 10 | 0 | Optimal | HiGHS |
| mip-07.mip.lp | min | -54 | -54 | 0 | Optimal | HiGHS |
| mip-08.mip.lp | min | -38.5 | -38.5 | 0 | Optimal | HiGHS |
| mip-09.mip.lp | min | -83 | -83 | 0 | Optimal | HiGHS |
| mip-10.mip.lp | min | -23.666667 | -23.666667 | 0 | Optimal | Optio |
| mip-11.mip.lp | min | -20 | -20 | 0 | Optimal | HiGHS |
| mip-12.mip.lp | min | -24.333333 | -24.333333 | 0 | Optimal | HiGHS |
| mip-13.mip.lp | min | -27 | -27 | 0 | Optimal | Optio |
| mip-14.mip.lp | min | -24.000001 | -24.000001 | 0 | Optimal | Optio |
| mip-15.mip.lp | min | -27 | -27 | 0 | Optimal | HiGHS |
| sudoku-easy.lp | min | 0 | 0 | 0 | Optimal | Optio |
| sudoku-hard.lp | min | 0 | 0 | 0 | Optimal | Optio |
| tsp-10.mip.lp | min | 320 | 320 | 0 | Optimal | HiGHS |
| tsp-12.mip.lp | min | 279 | 279 | 0 | Optimal | HiGHS |
| tsp-8.mip.lp | min | 282 | 282 | 0 | Optimal | HiGHS |
| planning-infeasible.lp | min | — | — | — | Infeasible | — |

## Limits and corrections

Optio still lacks full MIP presolve, cuts, and warm-started node LPs. Stronger reference proofs do not imply that Optio itself proved those cases. Semi-continuous and semi-integer domains remain unsupported by Optio; their transformed Optio results are excluded from proof and bound aggregation. Native HiGHS solves their original domains. `2122` and `life_goe` are feasible and have native optimality proofs; earlier notes calling them infeasible were incorrect. The real export is now named `planning-export` because it has an independently checked native HiGHS incumbent. The replacement `planning-infeasible` is derived from planning-s with six capacities of one: mandatory unit-0 load 1,962 exceeds combined capacity six, so its LP relaxation is infeasible.

The rebuilt WASM binary is 150,941 raw bytes / 58,514 Brotli quality-11 bytes (node and web artifacts match). The working tree contained substantial solver changes before this task. Build hashes are in `build.json`; complete per-solver results and independent residual checks are in `results.json` and `scip.json`. `bounds.json` and `harness/fixtures/certified-bounds.json` provide the corrected baselines tied to SHA-256 hashes of the exact LP files.

Optio itself reports 43 optimal fixtures and the five infeasible/unbounded outcomes; the 49-optimum count aggregates native solver proofs. Validation: 34 active Rust tests passed, two pre-existing tests ignored. Native and WASM Sudoku A/B checks pass; the rebuilt WASM also passes all 44 seeded/classic regressions.

Reproduce:

```sh
cargo build --release -p optio-bench --features parallel
/tmp/optio-oracle-venv/bin/python harness/tools/optimality-bench.py --time 120 --threads 8 --models-out /tmp/optio-certification-models
/tmp/optio-oracle-venv/bin/python harness/tools/certify-remaining.py --time 120 --threads 8
./crates/wasm/build.sh
node harness/tools/sudoku-presolve-bench.mjs
/tmp/optio-oracle-venv/bin/python harness/tools/sudoku-highs-bench.py
node harness/tools/sudoku-highs-bench.mjs
/tmp/optio-oracle-venv/bin/python harness/tools/optimality-report.py
```
