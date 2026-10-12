# Further scheduling performance improvements

Compared with the already working, proven-optimal solver delivered earlier on
2026-10-11, large WASM is **2.7× faster** and large native is **4.2× faster**.
Medium improves **1.3× in WASM** and **2.3× native**. Small changes are negligible.
All three fixtures still have checked optimal solutions and conservative lower
bounds, with zero branch-and-bound nodes.

## Repeated measurements

Apple M1 Pro. Medians of seven fresh-process solves per build/configuration,
alternating before/after order. Time covers solver execution; process startup,
module initialization and CSR ingestion are excluded. Each run requests zero
relative/absolute gap and a 5-second limit. All 168 runs finish, have witnesses
checked to 1e-9 feasibility tolerance, and close their conservative bounds to
within 1e-6. Native tests request one and eight threads; this proven scheduling
path does not need parallel search workers.

| Runtime | Case | Start / threads | Previous ms | Updated ms | Speedup |
|---|---|---|---:|---:|---:|
| WASM | Small | greedy | 3.33 | 3.26 | 1.02× |
| WASM | Small | none | 4.55 | 4.51 | 1.01× |
| WASM | Medium | greedy | 85.65 | 65.60 | 1.31× |
| WASM | Medium | none | 85.57 | 65.57 | 1.31× |
| WASM | Large | greedy | 148.46 | 55.25 | 2.69× |
| WASM | Large | none | 145.03 | 53.71 | 2.70× |
| Native | Small | 1 | 0.063 | 0.060 | 1.04× |
| Native | Small | 8 | 0.062 | 0.058 | 1.08× |
| Native | Medium | 1 | 28.55 | 12.60 | 2.26× |
| Native | Medium | 8 | 28.34 | 12.94 | 2.19× |
| Native | Large | 1 | 64.34 | 15.39 | 4.18× |
| Native | Large | 8 | 65.14 | 15.65 | 4.16× |

`repeated-results.json` retains every timing, phase counter, bound and check,
plus the first witness for each build/configuration. `repeated-summary.json`
retains all seven timing samples and phase medians. Every record includes
fixture and binary hashes. This comparison uses the previous successful
solver; it does not reuse the original failing app baseline.

## Changes in the shared Rust core

- Index integer-only constraint rows by component once, replacing a full-row
  scan and temporary row allocations for each scheduling block.
- Certificate detection builds only structural information. It no longer
  generates primal ring permutations or compiles auxiliary projection rules.
  It still validates all original round definitions and enumerates every
  allowed day alternative for its lower bound.
- On larger models, try one representative ring assignment per day/round
  choice before generating up to 512 samples. Balanced packing reconstructs
  the ring allocation. Accept this shortcut only if its fully checked point
  closes the row certificate; otherwise use the previous full sampled search.
- Index continuous projection rules by their target, and visit continuous
  columns only when resolving dependencies.
- Use single-entry updates and a cheaper numeric-profile hash in the bounded
  day-choice DP tables. Full key equality preserves distinct states even if
  hashes collide. Arbitrary model/template maps retain the standard hash.
- Check duplicate matrix entries with reusable column stamps rather than
  allocating a hash set for every original row.

The hash-only ablation is retained in `hash-ablation.json`: on top of the
indexing changes, medium improves from 74.8 to 65.8 ms in seeded WASM and from
21.9 to 12.6 ms in native. Large is unchanged by this additional change.

## Remaining costs and proofs

Seeded large planning drops from **119.85 to 26.62 ms**; presolve remains about
21.53 ms. Seeded medium planning drops from **48.96 to 39.11 ms**, and certificate
generation from **33.30 to 23.25 ms**. Medium's state expansion/scoring and
large's presolve are now the main remaining costs. Phase timings do not cover
every API/checking overhead.

| Case | Objective | Conservative lower bound |
|---|---:|---:|
| Small | 314.90 | 314.89999998612154 |
| Medium | 3096.80 | 3096.799999999998 |
| Large | 4028.40 | 4028.399999740922 |

The proof construction and existing MIP numeric optimality tolerance are
unchanged. The lower bound remains the computed certificate, rather than being
set equal to the incumbent. No model names or fixture optima enter solver code.
The original 120-second benchmark proof report and independently parsed LP
certificates remain in their earlier directories.

## Validation and packages

- 50 active Rust tests pass; two previously ignored tests remain ignored.
- 44/44 classic WASM regressions pass.
- 61/61 supported native fixtures pass the 3-second/eight-thread smoke sweep;
  one existing semi-integer fixture remains unsupported and is separately marked.
- Four browser ES-module checks pass: medium/large, seeded/unseeded. Seeded
  callbacks report the initial solution and then the optimum.
- Updated default Node and web packages contain identical WASM bytes.
- WASM is 267,631 bytes raw / 95,238 Brotli, up 2,279 / 705 bytes from the
  previous package (under 1%). Hashes and environment are in `metadata.json`.

Native witnesses and independent arithmetic checks are in
`native-regressions.json`; WASM checks in `wasm-regressions.json`; browser API
witnesses/checks in `web-smoke.json`. Raw process logs remain local and ignored.

## Reproduce in this workspace

The frozen previous and updated packages/binaries remain under `/tmp`; their
hashes are recorded. To rerun the alternating comparison:

```sh
python3 harness/tools/planning-speed-bench.py \
  --before /tmp/optio-app-speed-before \
  --after /tmp/optio-app-speed-after \
  --out /tmp/optio-speed-repeat --repeats 7
node harness/tools/planning-web-smoke.mjs pkg-web /tmp/optio-web-checks.json
```

HiGHS comparisons from the previous report were not rerun for this optimization
pass. This report measures the incremental change against Optio's previous build.
