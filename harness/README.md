# optio harness

The differential oracle. It runs a model through **highs-js** (baseline) and,
optionally, **optio**, then verifies the returned solution **arithmetically**
with `checkModel` — never trusting a solver's own status.

## Run

```sh
node src/oracle.mjs           # highs-js baseline + independent check
node src/oracle.mjs --optio   # also run optio (build the wasm pkg first)
```

Environment:

| var | effect |
|---|---|
| `HIGHS_JS` | absolute path to the highs-js loader (default `../../highs-js/build/highs.mjs`) |
| `OPTIO_WASM` | explicit path to an optio wasm entry; default tries the npm package (`npm/index.cjs`) then `pkg/optio_wasm.js` |
| `TIME_LIMIT` | override the per-fixture time limit (seconds) |

Build the optio package first (npm layout, preferred):

```sh
node ../npm/build.mjs
```

or the raw nodejs-target build the harness falls back to:

```sh
../crates/wasm/build.sh
```

Outputs `oracle-report.json` (one record per fixture, both solvers).

## What it checks

- `detached = model.getModel('csr')` — the exact model (costs, bounds,
  integrality, CSR matrix), so the checker is independent of the LP parser.
- `checkModel(detached, colValue)` recomputes row residual `Ax`, bound and
  integrality violations, and the objective.

## Fixtures

`fixtures/*.lp` are linear LP/MIP only (no QP):

- small HiGHS regression instances (`1451`, `fixed-binary`, `semi-integer`,
  `issue-2585`, `issue-2388`, `2122`, `life_goe.mod`, `exceeds_stack`);
- `planning-s/m/l/tight` — synthetically generated planning-shaped MILPs
  (units × sessions × rings, caps, min/max/deviation balancing, one-ring-per-unit,
  ring restrictions). Guaranteed feasible by construction;
- `planning-export` — a **real app export, anonymized** (all group/ring/
  timeslot IDs replaced with `c#`/`r#`). Although it came from the app's
  infeasible/IIS branch, native HiGHS found an independently checked feasible
  solution in the 120-second certification run. It is retained as `planning-export`; see
  `fixtures/certified-bounds.json`.

- `planning-infeasible` — a capacity-shortage regression derived from `planning-s`,
  with each of its six ring capacities set to one. Unit 0 alone needs
  three assignments of load 654 (1,962 total), exceeding combined capacity six.
  Thus even the continuous relaxation is infeasible.

## Tools

| tool | purpose |
|---|---|
| `tools/build-fixtures.mjs <src.lp>` | anonymize a real export (rename all vars/rows) → `fixtures/planning-export.lp` |
| `tools/gen-planning.mjs` | (re)generate the feasible `planning-*` variants |

`gen-planning.mjs` builds a random witness assignment first and sets each
session/ring cap from its max load, so every generated instance is feasible.

## Differential LP/MIP oracle (`gen-lp` + `native`)

The `oracle` above compares against the **wasm** optio package. `native` is the
sibling that compares against the **native CLI** (`cargo run -p optio-bench`)
over a seeded random suite, which is the project's verification spine.

```sh
npm run gen-lp    # (re)generate fixtures/lp/*.lp + manifest.json
npm run native    # solve every instance with HiGHS + the native CLI, compare
# self-contained (regenerates first):
node tools/gen-lp.mjs && node src/native.mjs
```

Environment:

| var | effect |
|---|---|
| `ONLY` | `lp` or `mip` — restrict to one instance kind |
| `TIME_LIMIT` | override every instance's per-instance time limit (seconds) |
| `SKIP_GEN` | `1` — do not regenerate the suite (requires `manifest.json`) |

`tools/gen-lp.mjs` emits ~24 continuous LPs (`lp-*.lp`, incl. 2 deliberately
infeasible and 2 unbounded) and ~15 mixed-integer LPs (`mip-*.mip.lp`), all
derived from explicit seeds (re-running is byte-identical). Every "feasible"
instance is built around a random witness assignment, so feasibility holds by
construction; continuous instances with infinite bounds give those columns zero
cost, which keeps the objective bounded. For each file the manifest records
`{ name, file, kind, seconds, expect }`, where `expect` is **derived by running
HiGHS**, never by the generator's intent.

`src/native.mjs` runs each instance through both solvers:

1. HiGHS (highs-js) gives the reference status/objective and the `detached`
   model (`getModel('csr')`);
2. the detached model is serialised to a temp JSON file (±`Infinity` mapped to
   the CLI's `1e30` `INF`) and solved by
   `cargo run -q -p optio-bench -- --model-json <file> [--time] [--gap] [--lp]`
   (`--lp` for continuous instances, default MIP mode otherwise);
3. optio's returned `colValue` is **independently re-checked** with
   `checkModel` — optio's own status is never trusted;
4. statuses are compared with documented acceptable mappings
   (`UnboundedOrInfeasible` may match `Infeasible` or `Unbounded`), the objective
   relative delta is reported (tolerance `1e-4`; MIPs run at the same time limit
   as HiGHS), and any solution the checker rejects is flagged as a bug.

Outputs `native-report.json` (per-instance records + a summary) and a readable
table.


## Adding the planning generator

The next step is to make the generated instances mirror the app's exact model
(round series, finals days, `y`/`x`/`xr`/`z` families) rather than the reduced
flavour above, and to emit the same `detached`-style model JSON. See `PLAN.md` §2.

## Native-HiGHS oracle (independent of `highs-js`)

The `highs-js` wasm build used by `src/oracle.mjs` **mis-solves MIPs**: it
returns `Optimal` for feasible-but-suboptimal points (e.g. `planning-s` reports
1918 while a feasible 680 exists, and native HiGHS reaches 680). Do not trust
its MIP status/objective as ground truth.

`tools/native-oracle.py` drives **native HiGHS** (`highspy`) instead, and builds
the model JSON that `optio-bench` consumes from native HiGHS too, so the wasm
build is not in the loop for either side. For each `.lp` it prints the reference
status/objective/MIP-gap/dual bound, optio's status/objective, the relative gap,
and an independent python feasibility check of optio's solution.

```sh
bash tools/setup-oracle.sh                 # venv + highspy (native HiGHS)
tools/oracle-venv/bin/python tools/native-oracle.py harness/fixtures/planning-s.lp --time 30
```

For small instances the ground truth can also be obtained exactly
(brute force / Held-Karp for TSP), and any candidate solution's feasibility is
checked arithmetically by `src/checker.mjs` / the python checker in the tool.


Planning timing and shared native/WASM improvements are measured with the
internal benchmark tooling.
`planning-timing.mjs` emits phase times and node-LP counters;
`planning-improvement-bench.py` alternates frozen before/after builds;
`native-certification-smoke.py` rechecks every fixture at 120 seconds/eight workers.

The real app fixtures and identical-group scheduling path are measured with the
internal benchmark tooling.
`planning-app-bench.py` compares identical CSR models and starts against native
HiGHS with zero requested gaps. `planning-timing.mjs` accepts `--start <json>`
and reports `time_bound_certificate_secs` alongside LP/search counters.
