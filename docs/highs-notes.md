# Native HiGHS vs optio — measured gaps, root causes, and what is borrowable

Read-only analysis (no source edits). HiGHS reference: **1.15.1** via `highspy`
(`/tmp/optio-oracle-venv/bin/python`), the tree at
`/Users/seb/Developer/highs-js/HiGHS/highs`. optio: `target/release/optio-bench`
(rebuilt from current `crates/bench`). All timings are **medians of 5** runs on
one machine; another agent was compiling/testing during the session, so treat
±10 % as noise. optio numbers are single-thread (default). HiGHS numbers are
its default (dual simplex; MIP `Thread count 5 … 1 max workers`, i.e. parallel
search off for these tiny/hard cases).

Raw logs and helper scripts used for this note live in
`/private/var/folders/.../T/opencode/` (`highs_diag.py`, `highs_time.py`,
`sudoku_rules.py`, `*.log`).

---

## 1. Measured gaps

### 1.1 End-to-end (MIP)

| fixture | HiGHS status | HiGHS ms | HiGHS iters | optio status | optio ms | optio iters |
|---|---|---:|---:|---|---:|---:|
| sudoku-easy | Optimal | **1.24** | 0 | Optimal | **101** | 1049 |
| sudoku-hard | Optimal | **1.82** | 0 | Optimal | **159** | 1656 |
| tsp-12      | Optimal | **131**  | 78 | Optimal | **39** | 122 |

### 1.2 LP relaxation (planning models relaxed to continuous)

| fixture | cols×rows | HiGHS ms | HiGHS iters | optio solve ms | optio build ms | optio iters | µs/iter HiGHS | µs/iter optio |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| planning-m       | 3051×2073  | 33.3 | 1342 | 40.2 | 0.2 | 758  | **24.8** | **53.1** |
| planning-l       | 10458×6764 | 423  | 4900 | 221  | 0.6 | 1696 | **86**   | **130**  |
| planning-faithful| 321×341    | 1.65 | 92   | 2.4  | 0.0 | 204  | 17.9     | 11.8     |

optio does **fewer** iterations on the two large models yet is slower per
iteration. This is the "per-iteration cost" gap the handover already flags:
**≈2.1× (planning-m)** and **≈1.5× (planning-l)** per pivot. On `planning-l`
optio still wins overall because it takes 2.9× fewer pivots; on `planning-m` the
1.8×-fewer-pivots advantage does not compensate for the 2.1× per-iteration cost.
Note the raw HiGHS times include its presolve + postsolve; for these LPs HiGHS
presolve is ~0 (below), so the comparison is close to like-for-like.

### 1.3 Where optio's per-pivot time goes (feature `profiling`, `OPTIO_LPTIME=1`)

Instrumented (Instant-per-iteration, so these overstate absolute cost; ratios
are the signal):

| fixture | iters | btran | price | ftran | ratio+update | refactor |
|---|---:|---:|---:|---:|---:|---:|
| sudoku-easy | 1049 | **19 ms** | 8 ms | 14 ms | 7 ms | 2 ms |
| planning-m  | 758  | 11 ms | **17 ms** | 6 ms | 8 ms | 1 ms |
| planning-l  | 1696 | 59 ms | **77 ms** | 32 ms | 50 ms | 5 ms |

- **Sudoku is btran/ftran-bound** (73 %): the basis LU is dense-ish
  (`factor_nnz≈10836` for `m=306`, ~35 nnz/col) and optio's `ftran`/`btran`
  allocate a full `Vec` and stream all of L/U **every** pivot.
- **Planning is price-bound**: `Simplex::price` (`lp.rs:1519`) computes reduced
  costs by gathering `self.lpr.col(j)` (CSC) for a window of columns and dotting
  against the dense `y`; on a degenerate LP the window scan falls through to a
  full `O(nnz)` pass each pivot.

### 1.4 optio solves the root LP twice

`--lp` solve time vs full MIP time:

| fixture | optio `--lp` ms (1× root LP) | optio MIP ms | ratio |
|---|---:|---:|---:|
| sudoku-easy | 50.5 | 101 | 2.0× |
| tsp-12      | 19.2 | 39  | 2.0× |

So for these two the MIP cost is **two identical LP solves** — one inside
`find_feasible::lp_round_feasible` (`heuristic.rs:19`, reached via
`solve_with_start`, `solve.rs:204`) and one at the B&B root
(`mip.rs::branch_and_bound`, `mip.rs:650`).

### 1.5 Presolve reductions (HiGHS)

HiGHS prints "Presolve reductions: rows …(-r); columns …(-c); nonzeros …(-n)".

| fixture | HiGHS presolve removed (rows / cols / nnz) | what fired |
|---|---|---|
| sudoku-easy | **324 / 729 / 2916 → empty** ("Reduced to empty") | core bound propagation, fixed cols, singleton rows |
| sudoku-hard | **324 / 729 / 2916 → empty** | same |
| planning-m LP | 0 / 0 / 0 ("Not reduced") | doubleton/dependent found nothing |
| planning-m MIP | 3 / 3 / 6 | probing (56 evaluations, 0 deletions) |
| planning-l LP | 0 / 0 / 0 | — |
| planning-l MIP | 12 / 12 / 24 | — |
| planning-faithful LP | 160 / 120 / 450 | **doubleton eq 90/90, aggregator 50/30, parallel rows 20/0** |
| planning-faithful MIP | 280 / 210 / 720 | same + probing |
| tsp-12 MIP | 76 / 0 / 1421 | parallel/redundant rows |

optio's only structural reduction is `lp::redundant_equalities`
(`lp.rs:116`), which drops linearly-dependent **equality** rows: 18 of 324 on
Sudoku, 40 of 341 on planning-faithful, 0 on the planning-m/l/tsp models.

---

## 2. Root cause per gap

### 2.1 Sudoku — HiGHS solves it in presolve, with **zero** simplex pivots

Trace (default HiGHS config), from the captured logs:

1. `HighsMipSolver` → `HPresolve::run` (`presolve/HPresolve.cpp:6330`) →
   `HPresolve::presolve` (`:5780`).
2. `initialRowAndColPresolve` (`:5734`) calls `rowPresolve` / `colPresolve`
   once over the model → Sudoku-easy drops to 172 rows × 122 cols.
3. The main loop calls `fastPresolveLoop` (`:5760`):
   `removeRowSingletons` → `presolveChangedRows` → `removeDoubletonEquations` →
   `presolveColSingletons` → `presolveChangedCols`, repeated until
   `problemSizeReduction() < 1 %`. For Sudoku this reaches **0 rows / 0 cols**.
4. `HPresolve::run` returns `kOptimal` → log "**Presolve: Optimal**".

The log confirms: `Nodes 0`, `LP iterations 0`, `Simplex iterations 0`. **No
simplex, no B&B, no primal heuristic.**

The actual mechanism is **bound propagation on the equality rows**, not exotic
presolve:

- Rows are `Σ v = 1` over binaries. `rowPresolve` (`:3440`) computes row-activity
  bounds and tightens variable bounds; the `impliedRowBounds` machinery +
  `computeColBounds` (`:6171`) / `updateColImpliedBounds` (`:669`) turn a fixed
  variable into implied bounds for the others: from `Σv = 1` and `v_c = 1`,
  every other `v_d` gets upper bound `(1 − 1)/1 = 0` → fixed to 0, which then
  re-fires the rows that contain each `v_d`. `colPresolve` (`:4409`) +
  `removeFixedCol` (`:6908`) remove newly fixed columns, `presolveChangedRows/Cols`
  requeue affected rows/cols. That fixed point solves the puzzle.
- The givens are encoded as bound fixings (`gen-classics.mjs:88-90` emits
  `v = 1`), so propagation has a seed.

Two independent confirmations that this is propagation, not search:

- Disabling rules (`--logrules` + `presolve_rule_off`): masking **probing
  (32768)**, **initial sweep (524288)**, **aggregator (4096)**, **sparsify
  (16384)** and **forcing row (64)** individually still leaves sudoku-hard
  "Reduced to empty". Setting the mask to `(1<<20)-(1<<6)` — i.e. **all**
  switchable rules (bits 6–19) off — *also* still reduces both puzzles to empty
  with 0 nodes / 0 LP iterations. So the work is done by the un-switchable core
  (`HConst.h:260-288`: empty row, singleton row, redundant row, empty col, fixed
  col, dominated col) plus the implied-bound machinery those core rules call,
  not by probing/aggregator/doubleton/sweep.
- With **presolve off**, HiGHS still solves sudoku-hard at the **root node with
  0 LP iterations**: `HighsMipSolverData::runSetup`
  (`mip/HighsMipSolverData.cpp:831`) calls `getDomain().propagate()` at `:982`,
  before the "Solving MIP model with … **729 domain fixed**" print at `:1085`.
  The engine is `HighsDomain::propagate` (`mip/HighsDomain.cpp:2366`) →
  `propagateRowUpper`/`propagateRowLower` (`:1413`/`:1457`), whose formula is the
  classic `x_i ≤ (U − minActivity + a_i·lo_i)/a_i` (and the `≥` mirror), driven
  by a worklist of changed rows and impacted columns.

**optio counterpart:** there is no presolve. `LpRelaxation::build` (`lp.rs:208`)
drops 18 dependent equality rows, then `Simplex::run` (`lp.rs:1131`) starts a
two-phase **primal** revised simplex with a crash basis (`crash`, `lp.rs:1013`).
The objective is all-zero, so the whole cost is a **degenerate Phase 1**:
`--lp` reports Phase 1 at 1049 pivots (easy) / 1656 (hard) with `t=0.0000` on
almost every pivot. Then `solve_with_start` re-solves the same root LP in B&B.
Hence 101 / 159 ms where HiGHS spends 1–2 ms fixing variables.

### 2.2 Large LP (planning-m / planning-l) — dual simplex + hyper-sparsity, not presolve

For these two, HiGHS presolve removes **nothing** (planning-m "Not reduced";
planning-l 0/0/0). The entire gap is the simplex engine. HiGHS runs the **dual
simplex**:

- Log: `WARNING …` then `Using dual simplex solver` and iterations labelled
  `DuPh2` (dual phase 2). Selection: `simplex_strategy` defaults to
  `kSimplexStrategyChoose` (`HighsOptions.h:899`, `SimplexConst.h:19-24`) →
  `HEkk::solvePhaseI/II` pick dual for LPs; the string comes from
  `simplex/HEkk.cpp:2310`.
- Leaving row: `HEkkDualRHS::chooseNormal` picks the max `infeasibility /
  edge_weight` over the infeasibility worklist (`HEkkDualRHS.cpp:37-118`) — i.e.
  **dual steepest edge / Devex** (`dual_edge_weight_`,
  `HEkkDual::interpretDualEdgeWeightStrategy`, `HEkkDual.cpp:2300`; DSE computed
  from a logical basis, Devex otherwise, `HEkkDual.cpp:148-176`, `switchToDevex`).
- Entering variable: within the chosen row via `HEkkDualRow::choosePivot`
  (`HEkkDualRow.cpp`).
- Per-iteration cost is dominated by **hyper-sparse** linear algebra:
  `HEkk::btran` of a **unit** vector `e_p` (the leaving row) produces `row_ep`,
  then the tableau row is priced by `HighsSparseMatrix::priceByRowWithSwitch`
  (`util/HighsSparseMatrix.cpp:1457`) / `priceByRow` (`:1439`), which loops only
  over the **nonzeros of `row_ep`** and accumulates `multiplier · A_row` into a
  sparse result, switching to dense only when fill-in exceeds
  `kHyperPriceDensity`. The vector type `HVectorBase` carries `count` and a
  packed `index` alongside the dense `array` (`util/HVectorBase.h:40-43`), so
  every FTRAN/BTRAN/PRICE knows its own density.
- Updates: `HEkk::updateFactor` (`HEkk.cpp:3075`) → `HSimplexNla` product-form /
  Forrest–Tomlin-style updates (`HSimplexNla.h`, `HEkk::updateFtranBFRT`),
  refactorised on an update limit / synthetic clock (`HEkk.cpp:3081-3090`).

**optio counterparts and why each pivot is dearer:**

| HiGHS | optio | note |
|---|---|---|
| dual simplex (`HEkkDual`) | primal two-phase (`Simplex::run`, `lp.rs:1131`) | Phase 1 adds artificials; needs the crash to be short |
| hyper-sparse `ftran`/`btran` (HVector count/index) | `SparseLu::ftran/btran` (`lp.rs:616/645`): allocate `vec![0;m]`, copy, scan **all** L/U columns; `apply_etas_*` walks the whole eta file | no density awareness |
| tableau-row pricing `priceByRow` | `Simplex::price` (`lp.rs:1519`): per-candidate `self.lpr.col(j)` gather + dense-`y` dot | full `O(nnz)` pass on degenerate LPs |
| Devex / dual steepest edge | Dantzig (largest reduced cost) + Bland after 100 degenerate pivots; partial-pricing window | Dantzig is the simplest, most iteration-hungry rule |
| BFRT/FT product-form updates | eta file + refactor every 128 etas (`refactorize`, `lp.rs:902`) | ok, but etas are applied densely |

Also worth noting: `ftran`, `btran`, `dense_column` (`lp.rs:800`), and
`recompute_basic` (`lp.rs:933`) each allocate a fresh `Vec` on the hot path, so
small models pay allocator overhead on top of the arithmetic.

### 2.3 TSP — why optio is now faster

HiGHS (131 ms): presolve removes 76 redundant rows / 1421 nnz (its
parallel-row / dependent-row detection sees that many of the enumerated
`sec_S ≥ 2` cuts are duplicates or implied by the degree rows — it does **not**
"know" they are subtour cuts, it just detects parallel rows,
`HPresolve::detectParallelRowsAndCols`, `:7267`), then `Presolve 0.04 s` +
`Solve 0.10 s` with `Nodes 1`, `78` LP iterations. The 0.10 s is dominated by
HiGHS's fixed MIP setup (feasibility jump/root LP machinery), not the 78 pivots
(the LP is 66 cols × 2058 rows).

optio (39 ms): the crash basis (`lp.rs:1013`) plus LP rounding make Phase 1
negligible (`phase1 iters=52`, `m=2058, n=66`), and the tall/narrow LP is cheap
per pivot. So optio wins by **avoiding HiGHS's MIP setup overhead** and by
starting Phase 1 near-feasible — not by any subtour insight. The remaining ~2×
on this instance is the duplicated root LP (1.4).

---

## 3. Ranked, in-scope recommendations (impact × effort)

Scope guard (PLAN.md §5): stay at "LP + MILP for this app's class". Explicitly
**out of scope** and not recommended here: general cutting planes / clique
tables / conflict pools, full MIPLIB parity, tableaus/separation, IIS machinery.
Nothing below adds a dependency; all are data-oriented and wasm-safe.

| # | change | impact | effort | in scope |
|---|---|---|---|---|
| 1 | **Bound-propagation presolve pass** (implied-bound tightening + fixed-col / empty-row / singleton cleanup, row+col worklist, iterate to fixed point) | Sudoku 100→<1 ms and 159→<1 ms (matches HiGHS); shrinks planning-faithful LP (optio 301 rows vs HiGHS 181); no effect on planning-m/l | medium (~300–500 LOC; pure bound tightening needs no real postsolve — fixed vars are restored from their bound) | yes — PLAN.md §3.2 "presolve — bound tightening, empty/singleton rows and cols" |
| 2 | **Share the root LP** between `find_feasible::lp_round_feasible` and B&B's root node (plumb the `LpSolution` + crash basis through `solve.rs`) | ~2× on TSP (39→~21 ms) and any MIP whose root LP rounds (Sudoku, but moot after #1) | small (one function + a struct field) | yes — HANDOVER §4 step 4 |
| 3 | **Kill per-pivot allocations** in `SparseLu::ftran/btran` and `Simplex` (`dense_column`, `btran` `to_vec`, `recompute_basic`): reuse per-solver scratch buffers | part of optio's per-pivot gap, especially Sudoku (btran-bound); likely 10–30 % on small/medium LPs | small (~50–100 LOC, purely mechanical) | yes |
| 4 | **Hyper-sparse ftran/btran/price** (add a count+packed-index "HVector"-style wrapper; skip-zero in LU/eta traversals; price the tableau row from `row_ep`) | closes most of the 1.5–2.1× per-pivot gap on planning-m/l | large (~400–800 LOC, touches the numerics; must re-run the LP differential oracle) | yes — HANDOVER §4 step 3 |
| 5 | **Devex / steepest-edge (or dual) pricing** to cut iteration count | medium on planning; helps re-optimisation later | medium | yes (dual simplex is PLAN.md §3.2 phase-3 target) |
| 6 | **Doubleton-equation substitution + aggregator** (with a postsolve stack) | large only on planning-faithful-like models; needs a postsolve stack optio lacks | medium–large | yes but lower priority |

The **single presolve technique with the biggest win is bound propagation**
(row #1). It is also the cheapest of the presolve rules (no matrix edits, no
postsolve beyond "fixed column = its bound"), data-oriented (CSR + CSC + a
worklist of changed rows/cols), and dependency-free — the exact profile of a
wasm-first pass. Empirically it is what cracks Sudoku both ways in HiGHS
(§2.1), and `doubleton equation` — the other candidate — cannot fire on Sudoku
at all.

---

## 4. Things I could NOT fully explain

- **Why HiGHS presolve removes nothing on planning-m/planning-l** despite the
  obvious structured rows (`x = Σ xr`, `load = Σ dur·x`, `total = Σ load`,
  `peak ≥ load`). `totaldef_*` has hundreds of nonzeros but `aggregator` and
  `doubleton` still find no safe substitution. I did not trace the per-row
  decisions (the rule log is LP-only, `HPresolveAnalysis.cpp:67`, and these are
  MIPs). It may be the huge `1e9` upper bounds / the `±INF`-like row ranges
  blocking implied-bound tightness. This is the one place the "count the
  reductions" question has no faceted answer beyond "0".
- **First-pass split for sudoku-hard**: initial sweep leaves 248×295 (vs
  172×122 easy), then it collapses to empty. I could not pin the exact sequence
  of rules from the logs because the MIP path does not emit the per-rule counter
  table; the masking experiment only proves the core (non-switchable) rules do
  it.
- **Exact size of the allocation win** (row #3): the `profiling` feature adds an
  `Instant::now()` pair per pivot, which inflates everything; I did not build a
  clean allocator-instrumented run, so the split between "buffer churn" and
  "genuine dense L/U scan" in Sudoku's btran cost is estimated, not measured.
- **optio's TSP objective 279 vs the LP bound**: optio reports `Nodes 1` and the
  rounded root LP is accepted; I did not verify whether the LP bound is exactly
  279 or slightly optimistic (the handover notes a ~1e-4 objective excess on
  integral TSP LPs), so "optio is 3× faster" should be read together with that
  caveat.
