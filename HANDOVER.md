# optio — handover

Two related artifacts exist:

- **`/Users/seb/Developer/highs-minimal`** — the *investigation*: how small/fast can a
  stripped HiGHS wasm build get, and what are the levers. Answer is largely complete.
- **`/Users/seb/Developer/optio`** — the *product*: a new Rust, wasm-first,
  data-oriented LP/MIP engine meant to replace `highs-js` in a planning feature.
  This is early: model IR, test oracle, fixtures, and a first feasibility
  heuristic exist; the actual solver (LP + branch & bound) does not yet.

A full "data-driven, lock-free rewrite" is **not** done. This document is the
starting point for whoever continues it.

---

## 1. `highs-minimal` — what we learned about fitting HiGHS in wasm

Toolchain used (native, **not** docker): Emscripten 6.0.3 at `~/emsdk`, CMake,
Binaryen. Scripts live in `highs-minimal/minimal/` (see its `RESULTS.md` and
`README.md`).

**Size progression (wasm, brotli):**

| build | brotli |
|---|---:|
| `-O3` stripped reference | 640 KB |
| `-Oz` | 481 KB |
| `-Oz` + real LTO | 462 KB |
| + `wasm-opt -Oz` | 444 KB |
| + iostream/file-I/O removal | 412 KB |
| **drop LTO** (best single-file) | **394 KB** |

**Levers, in order of impact:**
1. `-Oz` (the dominant one; `wasm-opt -Oz` alone cannot recover this because it
   runs after `-O3` already inlined).
2. `wasm-opt -Oz --converge` post-link.
3. Removing iostream/stringstream + file I/O (`minimal/apply-streams.mjs` + shim):
   `std::stringstream` drags in ~44 KB of stream/locale machinery on its own.
4. Brotli serving (394 KB brotli vs ~532 KB gzip).
5. **Do not re-add LTO** once streams are removed — it made the final size bigger
   (410 vs 394 KB) because LLVM duplication left less for `wasm-opt`.

**Attribution:** ~60% of the binary is the C++/C runtime (≈42% is `std::vector`/
`string`/`map`/`algorithm`, ~7% iostream/locale), ~40% is HiGHS. Flags are
exhausted; going materially smaller means cutting features or a rewrite.

**Toolchain blockers found (Emscripten 6.0.3):**
- `-msimd128` segfaults clang on `mip/HighsSearch.cpp` and `wasm-ld` on the large
  LTO+SIMD link. Try a newer emsdk.
- PGO (`-fprofile-generate`) never writes usable `.profraw` (0 bytes). Try newer.
- `-fno-exceptions` is not drop-in (`presolve/HPresolve.cpp` uses `try/catch`).

**Practical takeaway:** a single-file HiGHS wasm for this feature set bottoms out
around ~0.4 MB brotli; the remaining wins are architectural (split/lazy-load,
fewer features, or a different solver).

---

## 2. `optio` — goal and design

Rust, **wasm-first and native**, data-oriented, lock/contention-free. Not a HiGHS
port and does not track upstream. Scope: LP now → MIP; possibly CP-style later.

**API:** mirrors the `highs-js` extended surface used by the app's
`planning/mip-plan.ts` (`createModel`, `addVar`, `addRow`,
`changeColCost`, `changeColIntegrality`, `passColName/RowName`, `setSolution`,
`run({callbacks})`, `getModelStatus`, `getObjectiveValue`, `getSolution().colValue`,
`info.get`, `getIis`, `exportModel('lp')`, `dispose`). Goal: swap behind a flag so
the app works unchanged and the differential oracle can run both.

**Design rules (see `PLAN.md`):**
- Flat arrays + index newtypes (`VarId`/`RowId`); no per-variable objects, no owning
  pointers. CSR now, CSC built by the solver.
- The `Model` is immutable and shared. All mutable state is per-solve/per-worker
  (LP scratch, arenas) → no hot-path locks.
- Incumbent / dual bound shared only through atomics (f64 bits + version); native
  parallelism via a work-stealing node pool (`rayon` feature, compiled out on
  wasm32).
- Injected `Clock` (`Instant` native / `performance.now()` wasm — `std::time` is
  unusable on wasm). `check_solution` is the oracle; never trust the solver's own
  status. Primal-first: feasible incumbents early, optimality optional.
- wasm profile: `panic = "abort"`, `lto = "fat"`, `codegen-units = 1`,
  `opt-level = "z"` (`release-small`). Zero-copy results via `Float64Array` views
  (not yet done — currently copies).

---

## 3. Current state of `optio`

Crates (workspace):
- `crates/core` — `Model` (flat SoA + CSR), `check_solution` oracle, `Options`,
  `Clock`, `Callbacks`; `heuristic.rs` (generic min-conflicts),
  `local_search.rs` (**objective local search** for planning + constructive
  planner + transfer repair), `lp.rs`
  (**revised simplex**), `mip.rs` (**feasibility pump + diving + best-bound B&B**).
- `crates/wasm` — full `highs-js`-compatible surface + callbacks + TS shim.
- `crates/bench` — detached-model JSON CLI.
- `harness/` — differential oracles + generators (LP/MIP + Sudoku/TSP).

**What works (differentially verified):**
- `cargo test`: **19 pass**, 2 ignored.
- **LP**: **24/24** seeded random LPs match HiGHS; planning-s/m/l relaxations
  match to ~1e-8 (planning-m: 4294.63 exact).
- **MIP**: **15/15** seeded random MIPs optimal; **Sudoku easy+hard optimal**;
  **TSP-8/10/12 optimal** (DFJ). Full 44-instance suite: **LP 24/24, MIP 20/20**.
- **Planning**: a new objective local search (`local_search::improve_planning`)
  detects the planning shape generically (units / sessions / `(session,ring)`
  groups / fixed-charge links / auxiliaries — no names) and minimises the true
  objective with incremental transfer moves. It is seeded from the feasible
  incumbent and replaces the old `construct_with` balance sweep. Measured
  against the **native-HiGHS oracle** (`harness/tools/native-oracle.py`,
  highspy 1.15.1, default multi-thread), 20 s time limit:

  | fixture | optio @1s | optio @20s | native HiGHS @20s | rel (optio vs HiGHS) |
  |---|---|---|---|---|
  | planning-s | 692.79 | 671.75 | 680.14 | **−1.2% (better)** |
  | planning-tight | 5278.00 | 5044.72 | 5461.89 | **−7.6% (better)** |
  | planning-m | 4435.53 | 4435.53 | 5125.47 | **−13.5% (better)** |
  | planning-l | 10210.02 | 10201.77 | 11048.22 | **−7.7% (better)** |
  | planning-faithful | 258.26 | 258.26 | 258.26 | **0.0% (matches optimum)** |

  In other words optio now matches or beats HiGHS's 20 s objective within ~1 s
  on 4/5 planning fixtures (and is within 2% on `planning-s`). All plans are
  verified feasible by the independent checker.

- **Performance** (data-oriented pass): the simplex stores everything in flat,
  contiguous arrays to cut cache misses on the hot path. `SparseLu` keeps L/U as
  compressed (`start`/`idx`/`val`) columns and the eta file is flattened into
  parallel arrays (no `Vec<Vec<..>>` pointer chasing in `ftran`/`btran`/
  `apply_etas`); factorization is output-sensitive (spike over pivot rows via a
  heap for sparse bases, a plain pivot scan for dense ones, chosen from the
  previous fill) with row-index tie-breaks; partial pricing on large LPs. LP
  relaxation time vs native HiGHS (single-thread simplex; medians):

  | fixture | cols×rows | HiGHS | optio | ratio |
  |---|---:|---:|---:|---:|
  | planning-s | 302×250 | 1.7 ms | 0.8 ms | **0.51×** |
  | planning-tight | 3015×2055 | 50 ms | 40 ms | **0.80×** |
  | planning-l | 10458×6764 | 396 ms | 209 ms | **0.53×** |
  | planning-faithful | 321×341 | 1.3 ms | 1.5 ms | 1.08× |
  | planning-m | 3051×2073 | 31 ms | 36 ms | 1.18× |
  | seeded LPs (10–52 cols) | — | — | — | 0.2–0.5× |

  `optio-bench` also reports its own solve time (`ms` / `buildMs`), so the
  harness can compare solver time without `cargo`/process startup.
- **Shared root LP**: `find_feasible`'s LP-rounding fast path hands its solved
  root relaxation (`Feasible::root_lp`) to `branch_and_bound`, whose root node
  reuses it instead of re-solving — so Sudoku/TSP no longer pay for the root LP
  twice. Measured **~2×**: native sudoku-easy 101.7 → 52.3 ms, sudoku-hard
  162.4 → 82.9 ms, tsp-12 40.4 → 20.2 ms; wasm sudoku-easy 155.6 → 91.9 ms,
  tsp-12 291 → 200.9 ms. Suite stays LP 24/24, MIP 20/20.
- **Crash basis + LP-rounding fast path** (algorithmic): `Simplex::crash` seeds a
  feasible nonbasic point so most rows are already satisfied (Phase 1 nearly
  vanishes — tsp-12: ~6100 → ~110 Phase-1 pivots); `find_feasible` tries a
  bounded root-LP round before min-conflicts. Result: **tsp-12 4273 → 39 ms**
  (now *faster* than HiGHS 128 ms), sudoku-hard 2199 → 160 ms.
- **Native parallelism** (`--features parallel`; rayon + crossbeam-deque):
  work-stealing B&B (lock-free hot path — per-worker deques + atomic incumbent;
  no lock per node) plus a multi-start portfolio for the planning local search.
  **~5–6.5× node throughput on 8 threads** (planning-s 5.5k → 32k nodes/solve).
- **Wasm** (`crates/wasm/build.sh`): shipped package is **146 KB raw / 57 KB
  brotli** (+ ~4 KB JS brotli) — built with `+simd128`, `wasm-opt -Oz`, and
  nightly `-Z build-std=std,panic_abort` + `panic=immediate-abort` (opt out with
  `OPTIO_STABLE=1`). `harness/tools/wasm-size.sh` reports the size matrix;
  `harness/tools/wasm-bench.mjs` times optio-wasm vs highs-js with the
  independent checker. New bindings: **bulk load** (`Model::add_csr` /
  `add_model`, one call per model), **zero-copy results** (`get_solution_view`),
  and **phase profiling** (`get_info("time_heuristic_secs" | ...)`).
- **Callbacks** wired (incumbent/progress/interrupt; wasm `runWithCallbacks`).

  Measured non-wins (recorded so they aren't retried): SIMD auto-vectorization is
  only ~1–2% (the simplex is gather-bound and wasm `f64` is 2 lanes), and explicit
  `f64x2` kernels are not worth it; `f32`/`f16` are unusable on the critical path
  (f32 ε ≈ 1.2e-7 ≈ our tolerance). The free-list allocator (`lol_alloc`) shaves
  ~5 KB but is perf-neutral.

**Findings on the baseline**: the HiGHS build used by the harness returns
*suboptimal* MIP answers labelled `Optimal` (tsp-8 brute-force true 282 vs 364;
and planning-faithful / all planning instances where optio's verified-feasible
plans beat it). Its LP results match optio exactly, so its **MIP path is
unreliable**. Use brute force / Held-Karp for small TSP; compare planning to the
independent checker, not HiGHS.

**What still does NOT work:**
- Planning objectives now match/beat HiGHS, but plans are **not proven optimal**
  (status `TimeLimit`): the LP relaxation is weak (planning-faithful: LP bound
  167 vs integer optimum 258), so B&B cannot close it; the incumbent comes from
  the objective local search. `planning-faithful` reaches the *known optimum*
  (258.26) but is not certified.
- The old infeasibility assumptions were incorrect: native HiGHS proves
  `2122` and `life_goe` optimal, and finds an independently checked feasible
  incumbent for `planning-export`. The latter still has a proof gap at
  120 seconds. `getIis` remains a stub; see the corrected full-suite bounds in
  `docs/optimality-2026-10-11/`.
- `exceeds_stack` (a maximisation fixture) returns a loose incumbent
  (`TimeLimit`); maximisation is not the app's use-case.
- LP per-iteration cost is ~1.2× HiGHS on `planning-m` (full O(nnz) pricing dot
  products); HiGHS uses a hyper-sparse dual simplex + presolve.
- Sudoku now solves entirely by exact binary-cardinality propagation, with
  all 729 columns fixed, no LP iterations/nodes, and objective/lower bound 0.
  Native solve times are 0.076/0.171 ms and WASM 0.156/0.197 ms; the presolve gap
  is closed. General weighted implied-bound presolve remains future work.
- LP primal objectives can still carry rounding/perturbation error. B&B now
  prunes on Lagrangian dual bounds using true costs, projected row multipliers,
  and reduced-cost roundoff margins, rather than optimistic primal LP values.
  Unresolved LP nodes or a material remaining dual gap cannot certify optimality.
- Semi-continuous/semi-integer approximated as integer; `getIis` is a stub.
- Wasm LP is ~5× slower than native (inherent single-thread wasm penalty); this
  is not SIMD-addressable.

## 3.5 Planning integration (see `docs/planning-integration-fix.md`)

The planning MILP suite now passes **32/32** with rebuilt web WASM; 3 LP-import
regressions also pass. Fixes: polish scaled-LP rounding residuals, fill all warm
start load auxiliaries in the planner, validate the caller's incumbent before
search, emit progress/stopping callbacks before expensive preprocessing, and
parse exported minimization LPs in the adapter. NaN/infinite candidate values are
explicitly rejected. Seven Rust integration regressions cover the app model and
warm-start/feasibility contracts. Rust: **28 pass / 2 ignored**; native oracle:
LP 24/24, MIP 20/20; app type check and lint pass (3 unrelated lint warnings).
The 720-bout case is now ~2.2-2.5 s with complete placement within caps.
Bounds-propagation presolve remains planned. WASM: 146,861 raw / 57,061 Brotli.

## 4. Immediate next steps (priority order)

1. **Prove planning optimality**: cuts (cover/knapsack for caps, ring symmetry)
   to strengthen the weak LP bound, then warm-started node LPs (dual simplex)
   and pseudo-cost branching. Objective quality is already at/above HiGHS.
2. **Close the remaining planning proof gaps** using the corrected native
   certificates; `planning-export`/`2122`/`life_goe` are feasible. Implement IIS
   for genuine infeasibility cases.
3. **Extend presolve beyond binary-cardinality propagation** (now implemented
   and sufficient to solve both Sudoku fixtures). The worklist and general
   weighted implied-bound reductions from `docs/highs-notes.md` remain useful.
   A fixed-point worklist (implied-bound tightening on equalities → fix columns →
   tighten/remove rows; empty-row/singleton cleanup) would solve Sudoku in
   presolve (→ sub-ms, like HiGHS), shrink planning relaxations, and help the MIP
   generally. Data-oriented, wasm-safe, no deps, ~300–500 LOC; no postsolve of
   solutions needed for bounds/fixings.
4. **LP per-iteration cost**: first reuse scratch buffers to drop the per-pivot
   `Vec` allocations in `ftran`/`btran`/`dense_column`/`recompute_basic` (small,
   mechanical); then hyper-sparse ftran/btran + tableau-row pricing and a dual
   simplex (closes the measured 1.5–2.1× per-pivot gap on large LPs).
5. Node-LP robustness; semi-continuous; `getIis`; wasm PGO for the last few %.

(Done since the previous handover: native work-stealing B&B + multi-start
portfolio; wasm bulk-load / zero-copy / phase profiling; crash basis; LP-rounding
fast path; **shared root LP (~2× on Sudoku/TSP)**; wasm size 146 KB raw / 57 KB
brotli. `docs/highs-notes.md` records why HiGHS is faster.)

## 5. Risks / hard problems

- **Numerical robustness** of the simplex (pivoting, tolerances, LU updates) is the
  real 20% of the work. Build it small and fuzz it against the LP oracle.
- **Coupled SOS1 / fixed-charge** structure in the planning model — the reason the
  heuristic stalls.
- Getting from feasible to a **good balance objective** needs LP/duality plus local
  search; feasibility alone is not enough for the app.
- **Correctness stays differential** (solve a benchmark set, compare
  status/objective/gap/feasibility against HiGHS), not unit-test-only.
- Scope creep toward general MIPLIB parity. Hold the line at the app's problem
  class; HiGHS remains the fallback for hard instances.

---

## 6. Environment gotchas

- Native Emscripten at `~/emsdk` — `source ~/emsdk/emsdk_env.sh`. Docker path is
  configured in `highs-js` but the docker build failed here; the native sdk works.
- `/Users/seb/Developer/package.json` is `"type": "module"`, so wasm-bindgen's
  CommonJS output is misparsed. `optio/crates/wasm/build.sh` writes
  `pkg/package.json` (`{"type":"commonjs"}`); the harness loads it with
  `createRequire`.
- Oracle uses `/Users/seb/Developer/highs-js/build/highs.mjs` (override with
  `HIGHS_JS`). `wasm-pack` is not installed; use the `wasm-bindgen` CLI.
- The real app export came from the app's infeasible/IIS branch but is
  mathematically feasible. It is now `planning-export.lp`; the replacement
  `planning-infeasible.lp` has six capacities of one and mandatory demand
  exceeding their sum. Native HiGHS and Optio confirm infeasibility.

## 7. Key commands

```sh
# highs-minimal (size experiments)
cd ~/Developer/highs-minimal
./minimal/run-matrix.sh oz-streams          # build + size
node minimal/smoke.mjs minimal/out/oz-streams/highs.mjs

# optio
cd ~/Developer/optio
cargo test
./crates/wasm/build.sh                       # -> pkg/ (wasm-bindgen nodejs)
node harness/src/oracle.mjs --optio          # differential vs highs-js
node harness/tools/gen-planning.mjs          # regenerate planning fixtures
```

## 8. Recommendation

Don't try to out-HiGHS HiGHS on general MIPs with a rewrite — the algorithm quality
is the moat. The value in `optio` is a **wasm-first, small, anytime** solver tuned
to the planning problem class, with HiGHS as the fallback for hard cases. Keep the
differential oracle as the spine of the project, and make the **LP relaxation** the
next deliverable — it unblocks feasibility quality, branch & bound, and objective
optimization all at once.

## 9. Full 120-second certification run (2026-10-11)

See `docs/optimality-2026-10-11/README.md` and the SHA-256-bound baselines in
`harness/fixtures/certified-bounds.json`. All 59 fixtures were attempted with
eight native threads, zero requested MIP gap, and a 120-second limit per solver.
Native HiGHS plus an eight-worker SCIP concurrent pass certify 49 optimal,
3 infeasible, 2 unbounded; 5 planning cases remain open. Optio itself reports
43 optimal fixtures, plus the 5 infeasible/unbounded outcomes. Every returned Optio
incumbent passes the independent checker.

Best planning incumbents: small 661.14, faithful 258.26 (proved), tight 4802.04,
medium 4377.47, large 9945.03. The renamed real fixture `planning-export` is
feasible at 536748.44 with lower bound 536341.46. The report contains all real
proof bounds and remaining gaps; none of these values is hardcoded in the solver.

The core now returns `Solution::dual_bound`, native JSON exposes `dualBound`/`gap`,
and WASM exposes `get_info("mip_dual_bound")`. Parallel unresolved LP nodes cannot
certify optimality; a material remaining numerical dual gap returns SolutionLimit.
Both node/WASM and web packages were rebuilt. 34 active Rust tests and all 44
WASM seeded/classic regressions pass; both web Sudoku smoke checks pass.

Current WASM binary: 150,941 raw bytes / 58,514 Brotli quality-11 bytes.


## Shared core timing and performance pass — 2026-10-11

See `docs/planning-improvements-2026-10-11/README.md` and `comparison.html`.
Phase timings and node-LP work counters are exposed in native JSON and WASM info.
Parent bases feed bounded dual simplex with verified infeasibility certificates
and cold fallback. Weighted propagation, integer auxiliary inference, objective
bounds from strictly checked witnesses, a small root cut pool, learned pseudocosts,
shared node bounds and reused sparse-factorization scratch reduce repeated work.
Adaptive early local-search stopping remains opt-in because it worsened quality.

Final uncontended 10-second pairs across five planning cases measured B&B node
throughput gains of 1.8–7.8× native single-thread, 1.7–3.1× native eight-thread,
and 1.9–4.5× WASM. These are work rates on different trees, not proof-time speedups.
One native-eight-thread small incumbent was 0.79% worse; other feasible pairs matched.

All 59 fixtures were rerun with 120 seconds/eight native workers. All supported
status/bound/incumbent checks pass: 45 optimal, three infeasible, two unbounded,
eight unresolved. One unsupported semi-domain fixture is excluded. New Optio
proofs: exceeds_stack (1354 in 117.569s) and issue-2585 (-175.91 in 0.367ms).
No new planning optimality proof. The separate 600-second SCIP searches improved
tight to 4750.39, lower bound 4686.870000034753; exact-hash canonical baselines retain it.

42 active Rust tests pass both default and parallel builds; two prior tests ignored.
All 44 final native and WASM seeded/classic regressions pass. Both web Sudoku
smokes pass. Sudoku medians: easy/hard native-eight-thread 0.079/0.177ms,
WASM 0.107/0.216ms. Node/web packages rebuilt and their WASM bytes match.
Current WASM binary: 175965 raw bytes / 67222 Brotli quality-11 bytes.

Next shared-core targets: reduce warm LP cold fallbacks/reuse numeric factors,
strengthen relaxations with useful cuts and broader presolve, then reliability
branching and heuristic budget tuning with repeated quality measurements.

### Real app scheduling and identical groups (2026-10-11)

- New `crates/core/src/block_planner.rs` detects scheduling structure from CSR,
  shares normalized pattern templates (medium 28 blocks/6 classes; large
  72/3), merges aggregate day-load DP states, and packs balanced round demands
  with at most one ring per session. All proposals pass the full checker and
  cannot replace a better supplied start. Generic B&B symmetry is unchanged.
- Native/WASM both find and prove small 314.90, medium **3096.80**, large 4028.40,
  seeded or unseeded. The earlier 3096.90 reference was HiGHS' default gap.
- Original-row dual combination certificate uses outward-rounded intervals;
  separate integer peak/day-count certificate validates complete equalities
  and enumerates all eligible day options, never a heuristic pattern subset.
  Reported bounds are 314.89999998612154, 3096.799999999998, 4028.399999740922.
- Final seeded WASM medium 88.1 ms vs matched native HiGHS 565.2 ms; large
  149.7 ms vs 17,243.5 ms. Unseeded WASM large 143.9 ms proves while HiGHS
  reaches 20 s at 7443.00. Small unseeded remains about 5 ms slower than HiGHS.
- Proof checked both before/after local search; new `time_bound_certificate_secs`
  info/native timing field. Native planning fast path runs once before any
  parallel portfolio. Native 1/8-thread requests prove all three with no nodes.
- 50 Rust tests pass (+2 existing ignored); 44 WASM classic regressions; all
  61 supported native fixtures pass a 3 s/8-thread smoke. The remaining
  semi-integer model is still unsupported. Prior 120 s report is preserved.
- Default `pkg` and `pkg-web` rebuilt/matching and web-module large smoke passes.
  WASM 265352 raw / 94533 Brotli bytes. Hashes and complete witnesses/timings:
  `docs/planning-app-2026-10-11/`. Frozen before remains `/tmp/optio-app-before`.
- Updated LOSSES.md with resolved L1/L2 and historical baseline. Repro uses
  per-case limits and `--ref-gap 0`; native oracle now emits null for absent/
  nonfinite reference objectives instead of invalid JSON Infinity.
- No commits. Existing dirty work was preserved.

- Original LP exports also independently proved; actual conservative bounds added
  to canonical `harness/fixtures/certified-bounds.json` and archived bounds.
  `app-lp-proofs.json` retains full parsed models and checked original vectors.
- Final local-search improvements are emitted through incumbent callbacks before
  the early optimal return; seeded medium/large now emit seed then optimum.

## Further scheduling speed pass (2026-10-11)

- Shared `block_planner.rs`: index component rows once; certificate-only detection
  skips primal patterns/projection; index projection rules by target; larger
  models try one ring representative and accept only a checked row-certified
  optimum, otherwise retain 512-sample fallback. DP uses entry updates and a
  numeric-profile hasher; proof duplicate-entry checks use reusable stamps.
- Seven alternating fresh-process runs/config: seeded WASM medium 85.65→65.60 ms,
  large 148.46→55.25 ms; native one-thread medium 28.55→12.60 ms, large
  64.34→15.39 ms. Unseeded and eight-thread requests also checked. Small unchanged.
- All 168 measured runs prove the same three optima and preserve real conservative
  bounds. 50 Rust tests + 44 classic WASM checks + 61 supported native fixtures
  pass; one semi-integer fixture remains unsupported. Four browser API/callback
  checks pass. Node/web default packages updated with identical WASM bytes.
- New tools: `planning-speed-bench.py` alternating repeated comparisons and
  `planning-web-smoke.mjs`. Full evidence/report in `docs/planning-speed-2026-10-11/`.
- Frozen previous successful builds `/tmp/optio-app-speed-before`, updated
  `/tmp/optio-app-speed-after`, indexing-only ablation `/tmp/optio-app-speed-mid`.
  This baseline is distinct from the older failing `/tmp/optio-app-before`.
- WASM 267631 raw / 95238 Brotli, SHA256
  `4dc78da29ce71c174e71ef1770c906d63f21fb9650446c86ec72b52c7abd5132`.
- Next costs: medium DP state expansion/scoring/certification, large presolve.
  No commits; prior dirty work and 120-second proof reports preserved.
