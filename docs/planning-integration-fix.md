# Planning app integration: verified fixes

Status: the planning MILP suite passes **32/32** with the rebuilt
web-target Optio WASM. The LP-import parser adds 3 passing regression tests.
Bound-propagation presolve remains a future optimization; it was not needed to
resolve these failures.

## Failure and repair

The simplex uses scaled tolerances. Planning load rows (`720*x - min >= 0`)
left about 1e-4 error in original units, so the 1e-6 feasibility checker rejected
otherwise valid rounded placements. `heuristic::polish` projects continuous
auxiliaries and validates the candidate without changing integer assignments.
Both heuristic LP rounding and B&B candidate rounding use it.

The subsequent integration review found two additional causes:

- The app supplied placement-only warm starts with all load auxiliaries zero.
  Recursive projection did not reliably complete the larger coupled models.
  The planner now fills peak/min/range/total/deviation/pairwise-difference and
  soft-cap overflow values directly from the chosen assignments and frozen load,
  after symmetry relabelling. These are still checked against the full model.
- Optio attempted to construct a new incumbent before validating the supplied
  start, and stopping callbacks were consulted too late. The solver now validates
  the supplied incumbent first, keeps a heuristic incumbent if the supplied one
  is invalid, and emits incumbent/progress callbacks before local search or LP
  preprocessing. A caller-accepted plan returns `Interrupt` with its valid point;
  the app maps its balance-target stop to `ObjectiveTarget`.
- The adapter ignored `createModel({format:'lp', data})`. It now parses the linear
  minimization LP subset used by the exporter, including multiline expressions,
  bounds, general integers and binaries. Unsupported input (including
  maximization and nonlinear expressions) throws instead of silently changing
  the model. `withModel(source, operation)` also uses the parser and disposes the
  model on errors.

The solution checker now explicitly rejects NaN/infinity in candidate vectors:
`f64::max` can otherwise hide NaN constraint violations.

## Regression coverage

`crates/core/tests/planning_app_model.rs` preserves the exact 12-column,
15-row app-shaped reproducer. Seven tests cover heuristic rounding, B&B rounding,
objective/feasibility, genuine infeasibility, placement-only warm-start acceptance,
early stopping before search, invalid-start fallback, and non-finite candidates.
The negative test now requires **no reported incumbent**, rather than accepting
an invalid returned point.

The original three rounding regressions were reported verified by OpenCode
against a disabled projection loop (3 failures); subsequent independent tests
verified the final paths and stricter infeasibility contract.

The app's LP-parser tests cover solving/reloading linear integer LPs,
binary bounds and rejection of malformed/unsupported input. The existing
planning assertions were preserved.

## Verification (2026-10-11)

- `cargo test`: 28 passed, 2 ignored, 0 failures (21 unit + 7 integration).
- Integration regressions also pass compiled with the `parallel` feature.
- Native HiGHS differential suite: LP 24/24 and MIP 20/20, no discrepancies.
- Rebuilt web WASM: the app's `mip-plan.test.ts` 32/32; LP parser 3/3.
  Full planning directory + parser: 56 tests passed across four files.
- App type/lint checks: zero errors, zero warnings.
  Only generated `src/lib/optio/pkg/` is excluded; handwritten code is checked.

The 720-bout case now passes all capacity, completeness and no-ring-splitting
assertions in about 2.2-2.5 seconds on this machine; the previous review measured
22 seconds and `NoSolution`. The 28-group case now finds a complete plan and can
stop at the balance target. These are fixture results, not timing guarantees for
arbitrary planning instances or a proof of optimality.

## Size

Rust core: approximately 7,000 lines including comments/unit tests; WASM bindings:
877 lines; benchmark CLI: 255 lines; integration regressions: 240 lines.

Measured final WASM: 146,861 bytes raw, 65,727 gzip, 57,061 Brotli (quality 11).
Generated JS loader: 23,271 raw / 4,856 Brotli. Minified adapter + LP parser:
16,506 raw / 5,421 Brotli (excluding the loader/WASM). Bundler choices may change
JS payload size.

## Commands

```sh
cargo test
cargo test -p optio-core --features parallel --test planning_app_model
cargo build --release -p optio-bench
SKIP_GEN=1 node harness/src/native.mjs
WASM_BINDGEN_TARGET=web PKG_DIR=<app pkg dir> ./crates/wasm/build.sh
# then run the app's own planning/parser test suite from the app repository
```
