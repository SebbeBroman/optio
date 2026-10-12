# Proof engine foundation — 2026-10-11

This batch addresses repeated LP initialization, weak branching, missing node
propagation, and premature search termination. The benchmark compares the new
native and WASM builds with the archived 20-second suite and native HiGHS results.

## Results at 20 seconds

| Result (61 supported fixtures) | Previous native | New native | Previous WASM | New WASM | Native HiGHS |
|---|---:|---:|---:|---:|---:|
| Optimal | 48 | 49 | 48 | 49 | 50 |
| Completed correctly, including infeasible/unbounded | 53 | 54 | 53 | 54 | 55 |
| Checked feasible witnesses | 54 | 55 | 54 | 55 | 58 |
| Total solve time | 152.88 s | 141.31 s | 170.76 s | 143.13 s | 133.02 s |

Native total time falls **7.6%**, WASM **16.2%**. Every new returned witness passes
independent checking, and there are no contradicted new classifications, lost
proofs, lost incumbents, or worse objectives versus the previous Optio builds.
These are single-sample suite totals, including unsuccessful timed-out solves.

`exceeds_stack` is the new proof in both runtimes: **1354**, matching its dual bound,
**3.3 ms native / 23.7 ms WASM**, seven nodes. The previous native/WASM builds did
not prove it within the budget; native HiGHS proved it in 232 ms. An intermediate
Optio build with the other proof improvements but without count reformulation
needed 13.02 seconds and 148,957 nodes natively. `counts-ablation.json` contains
additional count-on/count-off measurements and stricter original checks.


The explicit ablation on the reviewed build has median count-on times
**4.33 ms native / 23.61 ms WASM**
over three fresh-process runs per runtime. With counts disabled, native needs
12.87 seconds / 148,957 nodes; WASM reaches 20 seconds / 102,022 nodes without
proof. Each returned original witness passes the stricter 1e-9 check.

Both runtimes now return an exactly integral checked candidate on `2122` with
objective **-204,822.79**; previous Optio runs returned none. This is a maximization
model, and native HiGHS's proved **-187,612.94** remains better. Optio has no finite
reported dual bound there. The new witness is a feasibility improvement, not an
optimality or objective-quality win over HiGHS.

On `planning-faithful`, native search processes **144,400 vs 13,235 nodes** at the
same 20-second limit. Phase I work drops **685,805 → 354,992 iterations**, about
**78% → 25%** of total LP iterations. Its final bound is slightly weaker
(167.68 vs 168.03), and the gap to objective 258.26 remains open. Faster node
processing alone does not close this proof. WASM processes 82,478 vs 10,129 nodes.

The severe WASM overruns disappear in this sweep: `planning-faithful` takes
**20.02 s** (previous 21.16 s), and `planning-s` **20.02 s** (previous 25.92 s).
Native `planning-s` still overruns by about 117 ms. Limits remain cooperative.

The seven remaining open fixtures in both runtimes are `2122`, `life_goe.mod`,
`planning-faithful`, `planning-l`, `planning-m`, `planning-s`, and `planning-tight`.
HiGHS proves the first three; Optio instead proves app-large and export where
HiGHS reaches its limit. WASM `planning-s` still has the worse objective 686.54
versus native HiGHS 675.91 (native Optio reaches 671.75). These losses remain bugs.

## Changes

- Dual-simplex repair moves nonbasic variables to finite reduced-cost endpoints
  while retaining the basis. Warm-start fallback counters identify invalid or
  singular bases, residual failures, dual failures, missing pivots, pivot failures,
  iteration limits, and invalid bounds.
- Reusable row/column adjacency propagates implied bounds at each node. Integer
  rounding, signed coefficients, infinities, duplicate entries, and interrupted
  propagation have explicit correctness coverage.
- Pseudocosts track reliability. Limited strong branching probes up to three
  candidates near the root and reuses selected child LP solutions without counting
  their work twice.
- Guarded objective-count reformulation adds integer category counts and equality
  links while retaining every original variable, row and domain. It moves equal
  objective costs onto counts so branching can resolve entire objective categories.
  Complete cover rows bound the counts; finite exact objective arithmetic and a
  small number of groups gate the transformation. Lifted points and callbacks are
  independently checked against the original model.
- A bounded integer-objective lattice rounds certified lower bounds to attainable
  objective values. Its group-count domains form a superset of feasible objective
  values. It rejects fractional costs, continuous objective variables, excessive
  ranges, and large terms whose floating sums may lose integer precision.
- Experimental, opt-in identical-column compression runs after presolve. Original-model checks guard
  solution lifting and proof closure; unsuccessful lifts fall back to the original
  model. This compression does not apply to `exceeds_stack`. It is disabled by default:
  scanning the large planning model slowed its WASM solve without reducing it.
- The fixed 100,000-node cutoff is replaced by configurable `mip_max_nodes`,
  unlimited by default. Explicit node exhaustion reports `SolutionLimit`.
  Numerical/iteration failures get one cold retry. Valid frontier bounds remain
  available for proof closure. LP loops check deadlines every iteration; factorization, crash and initialization
  check during expensive work; warm setup time is charged to the LP budget; and
  parallel search polls interruption through its coordinator.

The enabled changes can be ablated with `--no-node-presolve`, `--no-strong-branch`,
`--no-objective-lattice`, and `--no-objective-counts` in the native benchmark and
WASM timing harness.
`--column-compression` enables the experimental compression. Corresponding Rust options and WASM `set_option` entries are
available. LP and MIP diagnostics are exposed by both benchmark interfaces.

The standard WASM allocator is now the default. The small free-list allocator
remains selectable with `OPTIO_SMALL_ALLOC=1 bash crates/wasm/build.sh` (or Cargo
`--no-default-features`). A controlled allocator comparison on `planning-faithful`
reduced elapsed time from 29.54 to 20.02 seconds and increased processed nodes from
30,082 to 82,440. Both runs used the same proof engine and 20-second budget. This
supports allocator fragmentation as a major cost on queued searches; it does not
isolate every allocator operation or establish a strict deadline guarantee.

## Validation and measurement

`cargo test --offline -p optio-core --features parallel`: **98 tests pass**, two
pre-existing tests ignored. Coverage includes exhaustive feasible witnesses,
warm/cold LP agreement, interrupted propagation and LP solves, parallel search,
cache work accounting, and original planning fixtures.

`suite.json` records sequential cold solves of all 61 supported fixtures, one
thread, zero requested gaps, 20 seconds per solve, independently checked original
model witnesses, and binary/input hashes. New proof claims are checked against
archived witnesses and reference classifications. MIP Optimal claims additionally
require a closed finite dual bound. Pure LP numeric Optimal claims are validated
against the reference objective; the existing conservative certificate can be
unavailable for free columns, even at a numerical simplex optimum. Native HiGHS and previous Optio
results are archived samples rather than reruns in the same sweep. WASM runs use
fresh Node processes and include first-solve JIT work in solve timing; these are
not browser wall-clock measurements.

The reviewed default WASM is **366,771 bytes raw /
128,279 bytes Brotli-11**, versus
323,871 / 112,159 previously.
The proof improvements increase bundle size; `builds.json` records exact hashes.
The default `pkg/` was rebuilt, and six ES-module API checks pass for seeded and
unseeded medium/large planning models and count-variable callback dimensions
(`web-smoke.json`, executed in Node).

The full suite uses the frozen build hashes in `suite.json`. Subsequent review
added guards validating caller warm starts before they can replace a checked
lifted incumbent, preserving feasible starts at numerical row edges, and loosening
count caps outward at those edges. The cold sweep has no caller starts. The 98 tests were rerun,
and a follow-up one-second sweep of all 61 supported fixtures returns no invalid
witnesses or contradicted claims (`reviewed-smoke.json`). The web checks use this
final reviewed build; ablation hashes identify the earlier review snapshot.

Deadlines remain cooperative: an individual factorization or initialization can
overrun, and more frequent loop checks do not establish a hard real-time limit.
Numerical optimality statuses use solver tolerances, not rational certificates.

```sh
harness/tools/oracle-venv/bin/python harness/tools/proof-foundation-bench.py \
  --baseline docs/suite-20s-2026-10-11/results.json \
  --native /tmp/optio-proof-foundation/counts-native \
  --wasm /tmp/optio-proof-foundation/counts-pkg/optio_wasm.js \
  --time 20 --out docs/proof-foundation-2026-10-11/suite.json
```
