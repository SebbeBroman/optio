# Planning long searches — 2026-10-11

Five sequential SCIP 10.0.2 concurrent searches, eight threads, 600 seconds per original model. Separate HiGHS 1.15.1 original-objective-cutoff feasibility recovery was capped at 45 seconds per model. Existing native/oracle reports and the certification JSON artifacts did not retain usable solution vectors. No best vector was claimed as a seed without reconstruction and an independent check.

Total elapsed: 3214.375 seconds. No timeout is interpreted as infeasible. Native solver numerical certificates are not formal mathematical proofs.

## Results

- **planning-export**: timelimit; checked objective 536939.96; final API dual bound 536180.36; actual search 600.691s; recovery 45.038s; seed objective None, accepted False; proof status open.
- **planning-s**: timelimit; checked objective 661.14; final API dual bound 660.709075763; actual search 600.175s; recovery 32.410s; seed objective 661.1400009997709, accepted True; proof status open.
- **planning-m**: timelimit; checked objective 4399.9; final API dual bound 4294.63; actual search 600.188s; recovery 45.010s; seed objective None, accepted False; proof status open.
- **planning-l**: timelimit; checked objective 10387.95; final API dual bound 9064.74; actual search 600.808s; recovery 45.178s; seed objective None, accepted False; proof status open.
- **planning-tight**: timelimit; checked objective 4750.39; final API dual bound 4686.87; actual search 600.221s; recovery 26.314s; seed objective 4800.620000000001, accepted True; proof status open.

The concurrent live progress bound can be stronger than SCIP’s final returned solver bound (observed especially for planning-s). The persisted `dualBound` is `getDualbound()` after completion and matches the final solver summary. Rounded live display values are preserved in logs, and were not promoted into canonical baselines.

Every final witness and saved seed was independently checked again against the current original LP, including exact hash, variable names, row feasibility, bounds, integrality, and objective. Complete vectors are in per-instance JSON and `results.json`; `verification.json` records the second checks. LP snapshots, SCIP logs, and recovery seed vectors are retained. Atomic checkpoints were written before recovery, before the main search, and after each completed search.

## Baseline changes

- harness/fixtures/planning-tight.lp: objective 4802.040000000002 → 4750.39; bound 4686.870000034753 → 4686.870000034753; status Open.

Other historical incumbents and stronger historical dual bounds were preserved. `prior-*.json` and `baseline-updates.json` retain the pre-update baselines and exact changes. Solver code and fixture names were not changed. No commit was created.

Reproduce searches:

```sh
/tmp/optio-oracle-venv/bin/python harness/tools/planning-long-search.py
/tmp/optio-oracle-venv/bin/python harness/tools/planning-long-search-report.py
```
