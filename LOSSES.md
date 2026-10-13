# optio — known losses (vs HiGHS)

A running list of where optio is still **worse than HiGHS**, each with a
self-contained repro that runs **inside this repo** (no external checkout, no
fresh installs beyond the repo's own harness).

Correctness/feasibility is assumed unless a row says otherwise. Keep the
differential oracle (`harness/`) as the spine.

## Update — 2026-10-12

The 2122 parity batch closes the last
proof-count gap: **both Optio native and WASM now prove 50 optima at 20 seconds,
matching native HiGHS (50)**. The new proof is `2122`, optimal
−187 612.944194 with dual bound −187 612.944176 (native 12.1 s, WASM 18.9 s,
single thread, zero requested gap). Four generic fixes were needed — a finite
Lagrangian dual bound on columns with one-sided infinite bounds, a Bland leaving
rule that terminates a degenerate LP, a larger strong-branching budget, and a
bound-magnitude-scaled box check. No previous proofs or witnesses were lost
(checked witnesses stay 55; total native time falls to 133.6 s versus HiGHS's
133.0 s). Optio and HiGHS prove the same number but different sets: Optio proves
`planning-app-large`/`planning-export`, HiGHS proves `life_goe.mod`/
`planning-faithful`.

## Update — 2026-10-11

The proof-engine batch resolves the
`exceeds_stack` proof loss: both runtimes prove 1354 in seven nodes (3.3 ms native,
23.7 ms WASM in the cold suite). It uses a general bounded objective-count
reformulation, preserving every original constraint. Both runtimes now complete
54/61 supported fixtures correctly and prove 49 optima; native HiGHS completes
55/61 and proves 50. No previous Optio proofs or witnesses were lost.

Remaining priority bugs at a matched 20-second budget:

| Bug | Evidence | Next direction |
|---|---|---|
| Generic proof gap on `planning-faithful` | Objective 258.26; native bound 167.68 after 144,400 nodes. HiGHS proves 258.26 in 9.30 s. | Stronger validated cuts and planning relaxations; faster nodes alone do not close the gap. |
| Weak objective and unavailable bound on `2122` | New candidate -204,822.79, no finite dual bound. This maximization model has HiGHS optimum -187,612.94. | Better primal search and verified dual bounds with free columns / safe elimination. |
| Missing feasible point on `life_goe.mod` | Native explores 121,532 nodes without a witness; HiGHS proves objective 0 in 2.14 s. | Feasibility heuristics and conflict learning rather than more objective polishing. |
| WASM candidate quality on `planning-s` | WASM 686.54 versus native HiGHS 675.91 and native Optio 671.75. | Improve candidate search under the browser/runtime budget; preserve incumbent quality when allocating proof time. |

`planning-l`, `planning-m`, and `planning-tight` also remain unproved by both
Optio and native HiGHS at this budget. WASM deadline regressions were investigated
separately: the standard allocator replaces the small linked free-list allocator
by default, removing multi-second cleanup overruns in the measured suite. Limits
remain cooperative, including a roughly 117 ms native overrun on `planning-s`.

L1 and L2 are resolved for these three fixtures. Optio now proves the optima
314.90, **3096.80**, and **4028.40**, both with and without the greedy start.
The historical 3096.90 HiGHS result below used its default gap tolerance;
a zero-gap run proves 3096.80.

| scenario | seeded Optio WASM | seeded native HiGHS | Optio objective | status |
|---|---:|---:|---:|---|
| small | 3.6 ms | 3.7 ms | 314.90 | Optimal |
| medium | 88.1 ms | 565.2 ms | 3096.80 | Optimal |
| large | 149.7 ms | 17,243.5 ms | 4028.40 | Optimal |

These are matched CSR models, column order, starts, time limits and zero requested
gaps. Native HiGHS uses one thread; Optio WASM uses one thread. Without a seed,
Optio proves large in 143.9 ms; HiGHS reaches its 20-second limit at 7443.00.
Small without a seed remains faster in native HiGHS (0.2 ms versus 4.8 ms).

The scheduling path shares identical templates, merges equal aggregate day-load
states, and packs round demands into balanced sessions. Two checked lower-bound
certificates avoid branching. This does not add general symmetry detection to
branch-and-bound. Full results, bounds, phase timings, hashes, and reproduction
live in the internal experiment records.

## Repro (self-contained)

The reference is **native HiGHS** (`highspy`), not the in-repo `highs-js` wasm —
that wasm build mis-solves MIPs (it returns "Optimal" for feasible-but-suboptimal
points; see `harness/tools/native-oracle.py` for why).

```sh
cd /Users/seb/Developer/optio

bash harness/tools/setup-oracle.sh      # once: creates harness/tools/oracle-venv (highspy)
crates/wasm/build.sh                    # (re)build pkg/ for the current solver

node harness/tools/planning-app-loss.mjs            # optio seeded, native HiGHS reference
NO_SEED=1 node harness/tools/planning-app-loss.mjs  # drop the seed
TIME_LIMIT=30 node harness/tools/planning-app-loss.mjs
```

### Fixtures

Real planning MILPs, exported once and committed here:

| file | what |
|---|---|
| `harness/fixtures/planning-app-<s>.lp` | the model (for the native-HiGHS reference) |
| `harness/fixtures/planning-app-<s>.model.json` | the same model as detached CSR (for optio) |
| `harness/fixtures/planning-app-<s>.start.json` | the sparse greedy MIP start the app hands over |

`<s>` = `small` (1 group), `medium` (28 groups), `large` (72 groups). They were
exported once from the planning app and are committed so the repro does not
need the app's repository.

---

## L1 — Resolved: post-incumbent improvement stalls on the planning class

**What:** when the caller hands optio a warm start, optio reports that incumbent
and never improves it; native HiGHS keeps improving and wins by a wide margin.
First-incumbent time is essentially the seed — the search adds nothing after it.

**Historical evidence** (seeded, `node harness/tools/planning-app-loss.mjs`):

| scenario | native HiGHS | optio | optio status | gap | optio time |
|---|---:|---:|---|---:|---:|
| small | 314.90 | 314.90 | Optimal | 0.0% | 7 ms |
| medium | 3096.90 | **4979.40** | TimeLimit | **+60.8%** | 10.0 s |
| large | 4028.40 | **18379.80** | TimeLimit | **+356%** | 20.1 s |

- Both final optio values equal the seed objective → branch & bound / local search
  add nothing after the incumbent on these models (2 incumbents, then flat).
- Small is a control: optio proves the optimum and matches HiGHS.

**Why it matters:** the app cares about ring-load balance, i.e. objective quality
*after* feasibility. Feasible-first is not enough.

**Levers to try:** warm-started node LPs (dual simplex) + pseudo-cost branching;
cuts to strengthen the LP bound (cover/knapsack for session caps, ring symmetry);
check B&B isn't pruning immediately because the seed's bound `>=` incumbent; check
`local_search::improve_planning` actually runs once an incumbent exists.

---

## L2 — Resolved: feasible-first depends on the caller's warm start

**What:** without a warm start, optio does not even reach a feasible incumbent on
the larger app models; native HiGHS solves them easily.

**Historical evidence** (`NO_SEED=1 node harness/tools/planning-app-loss.mjs`):

| scenario | native HiGHS | optio (no seed) |
|---|---:|---|
| small | 314.90 | 314.90 Optimal |
| medium | 3096.90 | **Unknown — no incumbent** |
| large | 4028.40 | **Unknown — no incumbent** |

The app therefore **keeps** its greedy seed (`mip-plan.ts`), but optio should not
depend on it.

**Levers:** a stronger stand-alone primal heuristic (feasibility pump / diving)
so the caller's seed is optional.

---

## L3 — No proof of optimality / infeasibility on the app's models

**What:** the planning models come back `TimeLimit` (objective from the heuristic)
rather than proved optimal; infeasible models do not exhaust and `getIis` is a stub
(returns empty), so the app falls back to a generic message.

**Status:** expected given L1; tracked so it is not forgotten. Repro: the
infeasibility fixtures in `harness/fixtures` (e.g. `planning-infeasible.lp`) via
`native-oracle.py`.

---

## Historical / fixed

- **2026-10-10 — `NoSolution` on most app planning models.** Earlier optio builds
  returned no feasible incumbent (`primal_solution_status != 2`) on the
  planning MILPs; 16/32 of the app's planning tests failed. Fixed by the
  solver-side changes on 2026-10-11; the app suite is now 32/32.

### Further speed measurements

The incremental follow-up batch
uses seven alternating runs against the already corrected solver: seeded medium
WASM 85.65→65.60 ms, large 148.46→55.25 ms; native medium 28.55→12.60 ms,
large 64.34→15.39 ms. All optima and conservative bounds remain checked.
