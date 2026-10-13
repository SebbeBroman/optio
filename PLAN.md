# optio — a data-oriented LP/MIP engine

`optio` is a from-scratch optimization engine (LP now, MILP/cuts later; possibly
CP-style extensions) designed for **WebAssembly first** and **native second**,
with a **data-oriented** core and **lock-free-parallel** search. It is inspired
by HiGHS but is *not* a port and does **not** track upstream.

It exists to replace `highs-js` in a browser planning feature: find a
fully-placed, well-balanced plan fast, and deliver competitive proof performance.
Planning is the primary workload; the full fixture suite guards general solver
quality and performance.

## 1. Target problem shape

The target planning run is an event-scheduling / packing MILP:

- **Decision variables** (all integer/binary):
  - `y[unit]` — binary: which day the unit's finals land on.
  - `x[unit, timeslot, ring]` — integer matches of a group-unit in that slot/ring.
  - `xr[unit, roundType, timeslot, ring]` — integer, the round split of `x`.
  - `z[unit, timeslot, ring]` — binary "unit uses this ring in this slot".
- **Continuous auxiliaries** (linearized balance): `peak`, `min`, `range`, `over`,
  `deviation`, `pair_difference`, `total_load` per session.
- **Constraints**: exactly-one-finals-day; `x = Σ xr`; `Σ x = totalMatches` per
  unit; per-day round-series equality (rounds can't be unplaced); round-type ↔
  session eligibility; `x ≤ M·z`; one-ring-per-unit-per-session; hard/soft session
  caps; peak/min/range/deviation/pairwise definitions; ring-symmetry ordering.
- **Objective** (weighted): soft-cap overflow ≫ ring range > peak > mean deviation
  > pairwise difference > finals-day preference.
- **Scale**: ~109 group-units (more with preliminary splits), 3 days, ~5 rings,
  a few thousand to ~10⁴ integer vars. Small by MIPLIB standards.
- **Contract**: anytime incumbents (`onIncumbent`), warm start (`setSolution`),
  early stop on balance target (`mipInterrupt`), time limit, `getIis()` on
  infeasible, `exportModel('lp')`, HiGHS-numbered `ModelStatus`.

**Design consequence:** primal-first for early candidates, with proof performance
as a completion goal. Fast feasibility and local search must leave enough budget
for branch & bound to prove or refine the result.

## 2. Success metrics (the oracle)

> **A worse objective or slower solve compared to HiGHS is a bug in Optio.**

Treat reproducible losses in candidate quality, candidate latency, and time to
proof as bugs to investigate and fix. Finding the same objective without the
proof that HiGHS completes is also a loss. Track each loss in `LOSSES.md` with a
self-contained fixture, comparison settings, measured results, and a regression
benchmark; close it only after a matched rerun demonstrates the fix.

Comparisons use the same model, runtime, thread count, starts, time budget,
requested gaps, and timing scope. Judge objectives in the model's sense within
documented numerical tolerances. Confirm timing losses with repeated runs.
Record native-versus-WASM comparisons separately from comparisons within the
same runtime, and report deadline overruns. Use independently checked solutions
and validated HiGHS results as the reference; the current local `highs-js` WASM
build has contradicted MIP proof claims, so native HiGHS remains the correctness
reference until that build is validated.

| metric | target |
|---|---|
| differential parity (status, feasibility, `unplaced`) | exact |
| objective at matched time limit | no worse than HiGHS within numerical tolerance |
| time to **first fully-placed feasible** plan | ≤ HiGHS (primary goal) |
| time to the same objective quality | ≤ HiGHS |
| time to optimality / infeasibility / unboundedness proof | ≤ HiGHS |
| wasm size (brotli) | materially < HiGHS (~394 KB) |
| determinism | reproducible under a fixed seed |

Independent original-model feasibility checks and validated bounds are the
ground truth; the harness runs both solvers on generated + real planning
instances and compares. Solver status alone is insufficient.

### 2.1 Next proof-performance priorities

The 20-second suite comparison is the
baseline: both Optio builds prove 48 optima versus 50 for native HiGHS, and leave
eight supported fixtures unresolved. The
2122 parity batch closes that gap — both
Optio builds now prove **50** optima, matching native HiGHS. Priorities now:

1. Close `life_goe.mod` and `planning-faithful`, which HiGHS proves and Optio
   does not (a pure-feasibility engine and a stronger planning relaxation).
2. Diagnose LP warm-start fallbacks and improve dual-simplex repair, bound flips,
   and pivot stability to reduce repeated Phase I work.
3. Strengthen bounds with validated cover/clique/MIR cuts and generalized
   aggregate-load formulations for planning.
4. Propagate implied bounds and detect contradictions at every search node.
5. Enforce deadlines throughout the proof search and keep recovering numerically
   unresolved nodes (the 2122 batch scaled the box check and added a refactor +
   re-price retry; the remaining work is bound strengthening, not node throughput).

Measure proof time with known optimal starts as well as full cold solves, in
native and WASM. Track candidate latency, objective quality, dual-bound progress,
nodes, Phase I work, LP fallback reasons, and actual elapsed time. Improvements
must preserve independently checked witnesses and valid proof claims.

The first proof-engine batch adds
fallback diagnostics and endpoint repair, node propagation, reliability-based
strong branching with child-LP reuse, bounded objective-count reformulation,
integer-objective bound rounding, configurable
node limits, numerical retries, and deadline checks during factorization and crash.
Identical-column compression is experimental and opt-in because its detection
overhead regressed an already fast planning solve. WASM allocator choice is measured
on queued proof searches as well as small models; bundle size alone is insufficient.
General cover/clique/MIR separation, stronger planning relaxations, free-column
dual verification, and better infeasible-basis recovery remain open work.

## 3. Architecture

```
optio/
  crates/
    core/   optio-core   model IR, presolve, LP, MIP, heuristics
    wasm/   optio-wasm   wasm-bindgen bindings (HiGHS-inspired surface)
    bench/  optio-bench  native CLI + differential oracle runner
```

### 3.1 Data-oriented core
- No per-variable/row objects and no owning pointers: `VarId(u32)` / `RowId(u32)`
  indices into flat arrays. Struct-of-arrays for costs, bounds, integrality.
- Constraint matrix as CSR (rows) + lazily-built CSC (columns). Values `f64`,
  indices `u32`.
- Search state (nodes, cuts, fractional candidates, primal solutions) in per-solve
  **arenas/slabs**, reset between solves — no allocator churn in hot loops.
- `check_solution` is a first-class oracle, used by tests and the harness.

### 3.2 Solve pipeline
1. **Presolve** — bound tightening, empty/singleton rows and cols, coefficient
   tightening, duplicate detection, and domain-aware symmetry breaking (rings with
   identical availability + lock state are interchangeable).
2. **Primal heuristics (highest priority)** — seed from the caller's greedy plan /
   current plan; then feasibility pump + a structure-aware local search that
   minimizes ring-load range/peak by moving individual matches. Reports incumbents
   via callback as soon as they are feasible.
3. **LP relaxation** — a small, robust **dual simplex** with sparse LU
   (Markowitz pivot, Forrest–Tomlin-style updates), warm-startable across nodes.
   The balance objective makes the LP small and the interesting constraints simple.
4. **Branch & bound** — best-bound + diving, pseudo-cost branching, on the
   `x[unit, slot, ring]` assignment. Stops on gap, time, or balance target.
5. **Cuts (later)** — cover/knapsack for the cap constraints; ring symmetry.

### 3.3 Parallelism (native) & lock-freedom
- Node pool as a work-stealing deque (`crossbeam` native; `VecDeque` on wasm).
- One immutable `&Model` shared; all mutable state is per-worker (LP scratch,
  arenas). No locks on the hot path.
- Incumbent/dual-bound shared through atomics only (best objective as `AtomicU64`
  of `f64` bits + a sequence/version), read relaxed, updated rarely.
- Optional deterministic mode (fixed node order, bounded parallelism) for
  reproducible runs; default is throughput mode.
- `parallel` feature is compiled out on `wasm32` (which starts single-threaded;
  a shared-memory threaded build can come later behind COOP/COEP).

### 3.4 WebAssembly
- `wasm32-unknown-unknown`; `panic = "abort"`, `lto = "fat"`, `codegen-units = 1`,
  size profile `opt-level = "z"`.
- Time via an injected `Clock` (`performance.now()` on wasm; `Instant` native) —
  `std::time` is unusable on wasm.
- Zero-copy results: expose solution/bounds as `Float64Array` views over the wasm
  heap (the article's `WasmBuffer` idea) instead of copying vectors.
- Pre-size linear memory per solve to avoid `memory.grow` stalls.

### 3.5 API compatibility
The npm package implements a HiGHS-inspired surface:
`createModel` / `withModel`, `addVar`, `addRow({indices,values})`, `changeColCost`,
`changeColIntegrality`, `passColName`/`passRowName`, `setSolution`, `run({callbacks})`
(`onProgress`, `onIncumbent`, `interrupt`), `getModelStatus`,
`getObjectiveValue`, `getSolution().colValue`, `info.get(...)`, `getIis`,
`exportModel('lp')`, `options.set`, `dispose`, so solver call sites migrate
easily.

## 4. Phases & milestones

| phase | deliverable | acceptance |
|---|---|---|
| **P0 (done)** | workspace, model IR, oracle, wasm stub, native bench | `cargo test` green; oracle validates a model |
| **P1** | presolve-lite + dual-simplex LP relaxation | solves LP fixtures to optimality; matches HiGHS on random LPs |
| **P2** | B&B + primal heuristics + anytime/warm-start/early-stop | differential vs HiGHS on planning models: parity on feasibility/`unplaced`, no worse objective at matched limits; proof and timing losses tracked as bugs |
| **P3** | `optio-wasm` + npm package | drop-in for planning call sites behind a flag; in-browser run; brotli < HiGHS |
| **P4** | native rayon parallelism, memory/arena tuning, symmetry + first cuts | speedup scales with cores; no data races under a stress harness |
| **P5** | polish: IIS, `exportModel('lp')`, options/info, statuses, docs | app feature-complete vs `highs-js`; docs + examples |

## 5. Risks & mitigations

| risk | mitigation |
|---|---|
| simplex numerical robustness | start with a simple, heavily fuzzed dual simplex; random-LP differential tests against HiGHS; scale + tolerances explicit |
| AI-authored correctness | oracle-first, adversarial review, small runnable increments (Bun/pgrust recipe) |
| scope creep into full MIPLIB parity | hold the line at "LP + MILP for this workload class"; cuts deferred to P4 |
| wasm timing/memory pitfalls | injected clock; pre-sized memory; zero-copy views |
| divergence from app needs | drive every phase with real planning instances + the incumbent/interrupt callbacks |

## 6. Working method / budget

Adapt the Bun/pgrust playbook to a small budget:
- **Oracle before solver.** The differential harness is the spine; build it in P0–P1.
- One focused increment at a time, always runnable; commit per slice.
- Adversarial review on numerics (a second context checks LP/MIP results against
  HiGHS, not against `optio`'s own report).
- No large agent fan-out; this problem is ~10–30k lines, not 250k–1M.
