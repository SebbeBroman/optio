# Shared native/WASM performance improvements — 2026-10-11

Phase timing identified repeated child-LP feasibility work as a major avoidable cost. The solver now reuses parent bases with bounded dual simplex, strengthens weighted bounds and integer auxiliaries, adds a small root cut pool, learns branching pseudocosts, and reuses sparse-factorization scratch space. Node bound arrays and sibling bases are shared until a branch changes them.

Adaptive local-search stopping was tested and left disabled by default because it worsened planning solution quality. Feasibility-pump time is capped to leave proof-search time. This is a small root cut pool, not full GMI separation; row/column elimination and strong branching remain future work.

## Matched ten-second measurements

Alternating paired solves, no compilation during measurements. Reference proof jobs finished before this run; no other benchmark or compilation runs concurrently. Absolute timing and multithread search are load-sensitive. LP seconds sum across workers.

One alternating before/after pair per fixture and runtime, zero requested MIP gap. Parsing, process startup and checking are excluded. Every returned incumbent is checked independently. These are node-throughput ratios on different search trees, not speedups in time to prove optimality. The export baseline scarcely enters search during ten seconds, so its ratio is not comparable.

| Problem | Runtime | Nodes before → after | B&B work rate | Objective before → after | Final bound |
|---|---|---:|---:|---:|---:|
| planning-s | native-1 | 2,621 → 20,413 | 7.77× | 692.79 → 692.79 | 660.067 |
| planning-s | native-8 | 20,069 → 48,985 | 2.44× | 672.79 → 678.13 | 660.067 |
| planning-s | wasm | 1,528 → 7,903 | 4.42× | 692.79 → 692.79 | 660.067 |
| planning-faithful | native-1 | 1,655 → 8,196 | 4.95× | 258.26 → 258.26 | 167.434 |
| planning-faithful | native-8 | 14,122 → 31,035 | 2.20× | 258.26 → 258.26 | 167.433 |
| planning-faithful | wasm | 1,041 → 4,942 | 4.53× | 258.26 → 258.26 | 167.434 |
| planning-m | native-1 | 105 → 209 | 1.99× | 4,435.53 → 4,435.53 | 4,294.63 |
| planning-m | native-8 | 723 → 1,261 | 1.75× | 4,435.53 → 4,435.53 | 4,294.63 |
| planning-m | wasm | 51 → 96 | 1.90× | 4,435.53 → 4,435.53 | 4,294.63 |
| planning-tight | native-1 | 91 → 265 | 2.92× | 5,044.72 → 5,044.72 | 4,686.87 |
| planning-tight | native-8 | 630 → 1,333 | 2.12× | 5,039.22 → 5,039.22 | 4,686.87 |
| planning-tight | wasm | 46 → 135 | 2.93× | 5,044.72 → 5,044.72 | 4,686.87 |
| planning-l | native-1 | 21 → 38 | 1.81× | 10,210.02 → 10,210.02 | 9,064.74 |
| planning-l | native-8 | 130 → 404 | 3.11× | 9,992.22 → 9,992.22 | 9,064.74 |
| planning-l | wasm | 10 → 24 | 2.41× | 10,210.02 → 10,210.02 | 9,064.74 |
| planning-export | native-1 | 1 → 25 | — | — → — | 536,124.198 |
| planning-export | native-8 | 1 → 129 | — | — → — | 536,124.198 |
| planning-export | wasm | 1 → 13 | — | — → — | 536,124.198 |
| planning-infeasible | native-1 | 1 → 0 | — | — → — | — |
| planning-infeasible | native-8 | 1 → 0 | — | — → — | — |
| planning-infeasible | wasm | 1 → 0 | — | — → — | — |

One eight-thread small-planning incumbent regressed from 672.79 to 678.13 (0.79%) in the matched run; all other feasible paired objectives matched. Parallel deadline-sensitive local search varies with scheduling. Single pairs do not establish a stable quality regression or improvement.

## Two-minute final suite

All 59 fixtures attempted with eight native workers and a 120-second limit. 58 use supported variable types; 1 semi-domain fixture is excluded from solver-certification counts. All supported incumbents and status/bound consistency checks pass. Final Optio statuses: 45 optimal, 3 infeasible, 2 unbounded, 8 unresolved. Status 7 is a numerical solver certificate, not an exact rational proof.

| Planning problem | Optio objective | Optio bound | Old nodes → final nodes | Reference result |
|---|---:|---:|---:|---|
| planning-export | — | 536,124.198 | 638 → 1,378 | Open: 536,748.44; bound 536,341.46 |
| planning-faithful | 258.26 | 167.433 | 383,593 → 405,588 | Optimal: 258.26; bound 258.26 |
| planning-infeasible | — | — | 1 → 0 | Infeasible: —; bound — |
| planning-l | 9,945.03 | 9,064.74 | 2,398 → 4,649 | Open: 9,945.03; bound 9,064.74 |
| planning-m | 4,435.53 | 4,294.63 | 8,822 → 10,422 | Open: 4,377.47; bound 4,294.63 |
| planning-s | 670.69 | 660.067 | 408,211 → 684,152 | Open: 661.14; bound 660.921 |
| planning-tight | 5,020.42 | 4,686.87 | 8,005 → 15,439 | Open: 4,750.39; bound 4,686.87 |

Optio added two numerical optimum certificates relative to its archived run: exceeds_stack at 1,354 in 117.569 seconds (native HiGHS: 0.199 seconds), and issue-2585 at −175.91 in 0.367 milliseconds. No new planning optimum was proved.

The separate five 600-second, eight-thread SCIP searches improved planning-tight to **4,750.39**, retaining the independently checked lower bound **4,686.870000034753**. All five remain open. See [long-search report](../optimality-2026-10-11/long-search/README.md). Archived stronger reference bounds remain canonical; weak current Optio bounds never replace them.

## Validation and payload

42 active Rust tests pass; two pre-existing heuristic tests remain ignored. Final WASM passes all 44 seeded/classic fixtures. Both web Sudoku smoke tests pass. New tests check warm/cold LP agreement including infeasible boxes, exhaustive binary MIP optima, weighted propagation, cut validity, zero-length bound flips, rounded redundant equalities and tolerance-feasible objective cutoffs.

| Sudoku | Runtime | Median ms |
|---|---|---:|
| sudoku-easy | native-1 | 0.0800 |
| sudoku-easy | native-8 | 0.0794 |
| sudoku-easy | wasm | 0.1075 |
| sudoku-hard | native-1 | 0.1730 |
| sudoku-hard | native-8 | 0.1770 |
| sudoku-hard | wasm | 0.2163 |

Final WASM: 175,965 raw bytes / 67,222 Brotli quality-11 bytes, versus 150,941 / 58,514 at the Sudoku-only stage. Node and web bytes match.

## Remaining priorities

1. Reduce warm-start cold fallbacks on medium/large models, and reuse numeric factorizations where safe. Many remaining pivots are still feasibility work.
2. Strengthen root and node relaxations with useful separation and broader presolve. Faster nodes have barely moved the planning proof bounds; full GMI separation and row/column elimination were not implemented here.
3. Improve branching with reliability/strong-branching information, then retune heuristic budgets against repeated quality measurements. Adaptive early stopping remains off.

## Timing interpretation and reproduction

`timing` reports presolve, heuristic, local search, root strengthening and branch-and-bound elapsed seconds. `nodeLpWorkerSeconds` sums elapsed LP time across workers, includes scheduling/preemption, and can exceed wall time. LP counters distinguish phase 1, phase 2, warm solves and cold fallbacks. Incumbent callback timestamps indicate reporting time; they are not exact internal time-to-first-find measurements.

Each paired record binds the executable/WASM and LP file by SHA-256. The checkout already had substantial uncommitted work; no clean historical source snapshot or commit is claimed. Frozen baseline binaries are local `/tmp/optio-planning-before` artifacts. Current paired binaries are `/tmp/optio-planning-after`. The JSON artifacts preserve the measured hashes, timings, residual checks and final full-suite primal vectors.

```sh
cargo test -p optio-core --features parallel
cargo build --release -p optio-bench --features parallel
bash crates/wasm/build.sh
WASM_BINDGEN_TARGET=web PKG_DIR="$PWD/pkg-web" bash crates/wasm/build.sh
/tmp/optio-oracle-venv/bin/python harness/tools/planning-improvement-bench.py --time 10
/tmp/optio-oracle-venv/bin/python harness/tools/native-certification-smoke.py --time 120 --threads 8
python3 harness/tools/planning-improvement-report.py
```
