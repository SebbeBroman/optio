#!/usr/bin/env python3
"""Render the measured planning comparison; retain proof and work-rate distinctions."""
import collections, hashlib, html, json, pathlib
ROOT = pathlib.Path(__file__).resolve().parents[2]
OUT = ROOT / 'docs/planning-improvements-2026-10-11'
def read(name):
    return json.loads((OUT / name).read_text())
def fmt(x):
    return '—' if x is None else f'{x:,.3f}'.rstrip('0').rstrip('.')
def main():
    paired = read('paired.json')
    groups = collections.defaultdict(dict)
    for r in paired['records']:
        groups[(r['name'], r['runtime'])][r['variant']] = r
    measured = []
    for (name, runtime), p in groups.items():
        b, a = p['before'], p['after']
        bt, at = b['timing']['branchAndBound'], a['timing']['branchAndBound']
        rate = (a['nodes'] / at) / (b['nodes'] / bt) if min(bt, at, b['nodes']) > 0 else None
        measured.append(dict(name=name, runtime=runtime, rate=rate, before=b, after=a))
    native = read('final-native.json')
    records = native['records']
    assert len(records) == 59 and all(r['pass'] for r in records)
    supported = [r for r in records if r['optioSupportsVariableTypes']]
    counts = collections.Counter(r['statusCode'] for r in supported)
    size = read('wasm-size.json')
    sudoku = [r for r in read('sudoku.json') if r['presolve']]
    old = {r['file']:r for r in json.loads((ROOT/'docs/optimality-2026-10-11/results.json').read_text())['records']}
    lines = ['# Shared native/WASM performance improvements — 2026-10-11', '',
      'Phase timing identified repeated child-LP feasibility work as a major avoidable cost. The solver now reuses parent bases with bounded dual simplex, strengthens weighted bounds and integer auxiliaries, adds a small root cut pool, learns branching pseudocosts, and reuses sparse-factorization scratch space. Node bound arrays and sibling bases are shared until a branch changes them.', '',
      'Adaptive local-search stopping was tested and left disabled by default because it worsened planning solution quality. Feasibility-pump time is capped to leave proof-search time. This is a small root cut pool, not full GMI separation; row/column elimination and strong branching remain future work.', '',
      '## Matched ten-second measurements', '',
      paired['note'], '',
      'One alternating before/after pair per fixture and runtime, zero requested MIP gap. Parsing, process startup and checking are excluded. Every returned incumbent is checked independently. These are node-throughput ratios on different search trees, not speedups in time to prove optimality. The export baseline scarcely enters search during ten seconds, so its ratio is not comparable.', '',
      '| Problem | Runtime | Nodes before → after | B&B work rate | Objective before → after | Final bound |',
      '|---|---|---:|---:|---:|---:|']
    for r in measured:
        a,b=r['after'],r['before']
        rate = f"{r['rate']:.2f}×" if r['rate'] is not None and 'export' not in r['name'] else '—'
        lines.append(f"| {r['name']} | {r['runtime']} | {b['nodes']:,} → {a['nodes']:,} | {rate} | {fmt(b['objective'])} → {fmt(a['objective'])} | {fmt(a['dualBound'])} |")
    lines += ['', 'One eight-thread small-planning incumbent regressed from 672.79 to 678.13 (0.79%) in the matched run; all other feasible paired objectives matched. Parallel deadline-sensitive local search varies with scheduling. Single pairs do not establish a stable quality regression or improvement.', '', '## Two-minute final suite', '',
      f"All 59 fixtures attempted with eight native workers and a 120-second limit. {len(supported)} use supported variable types; {59-len(supported)} semi-domain fixture is excluded from solver-certification counts. All supported incumbents and status/bound consistency checks pass. Final Optio statuses: {counts[7]} optimal, {counts[8]} infeasible, {counts[10]} unbounded, {len(supported)-counts[7]-counts[8]-counts[10]} unresolved. Status 7 is a numerical solver certificate, not an exact rational proof.", '',
      '| Planning problem | Optio objective | Optio bound | Old nodes → final nodes | Reference result |',
      '|---|---:|---:|---:|---|']
    for r in records:
        if '/planning-' in r['file']:
            prev=old[r['file']]['optio'];ref=r['reference']
            lines.append(f"| {pathlib.Path(r['file']).stem} | {fmt(r['objective'])} | {fmt(r['dualBound'])} | {prev['nodes']:,} → {r['nodes']:,} | {ref['status']}: {fmt(ref['objective'])}; bound {fmt(ref['bound'])} |")
    lines += ['', 'Optio added two numerical optimum certificates relative to its archived run: exceeds_stack at 1,354 in 117.569 seconds (native HiGHS: 0.199 seconds), and issue-2585 at −175.91 in 0.367 milliseconds. No new planning optimum was proved.', '', 'The separate five 600-second, eight-thread SCIP searches improved planning-tight to **4,750.39**, retaining the independently checked lower bound **4,686.870000034753**. All five remain open. See [long-search report](../optimality-2026-10-11/long-search/README.md). Archived stronger reference bounds remain canonical; weak current Optio bounds never replace them.', '',
      '## Validation and payload', '',
      '42 active Rust tests pass; two pre-existing heuristic tests remain ignored. Final WASM passes all 44 seeded/classic fixtures. Both web Sudoku smoke tests pass. New tests check warm/cold LP agreement including infeasible boxes, exhaustive binary MIP optima, weighted propagation, cut validity, zero-length bound flips, rounded redundant equalities and tolerance-feasible objective cutoffs.', '',
      '| Sudoku | Runtime | Median ms |', '|---|---|---:|']
    for r in sudoku:
        lines.append(f"| {r['name']} | {r['runtime']} | {r['medianMs']:.4f} |")
    lines += ['', f"Final WASM: {size['rawBytes']:,} raw bytes / {size['brotliBytes']:,} Brotli quality-11 bytes, versus 150,941 / 58,514 at the Sudoku-only stage. Node and web bytes match.", '',
      '## Remaining priorities', '', '1. Reduce warm-start cold fallbacks on medium/large models, and reuse numeric factorizations where safe. Many remaining pivots are still feasibility work.', '2. Strengthen root and node relaxations with useful separation and broader presolve. Faster nodes have barely moved the planning proof bounds; full GMI separation and row/column elimination were not implemented here.', '3. Improve branching with reliability/strong-branching information, then retune heuristic budgets against repeated quality measurements. Adaptive early stopping remains off.', '', '## Timing interpretation and reproduction', '',
      '`timing` reports presolve, heuristic, local search, root strengthening and branch-and-bound elapsed seconds. `nodeLpWorkerSeconds` sums elapsed LP time across workers, includes scheduling/preemption, and can exceed wall time. LP counters distinguish phase 1, phase 2, warm solves and cold fallbacks. Incumbent callback timestamps indicate reporting time; they are not exact internal time-to-first-find measurements.', '',
      'Each paired record binds the executable/WASM and LP file by SHA-256. The checkout already had substantial uncommitted work; no clean historical source snapshot or commit is claimed. Frozen baseline binaries are local `/tmp/optio-planning-before` artifacts. Current paired binaries are `/tmp/optio-planning-after`. The JSON artifacts preserve the measured hashes, timings, residual checks and final full-suite primal vectors.', '',
      '```sh', 'cargo test -p optio-core --features parallel', 'cargo build --release -p optio-bench --features parallel',
      'bash crates/wasm/build.sh', 'WASM_BINDGEN_TARGET=web PKG_DIR="$PWD/pkg-web" bash crates/wasm/build.sh',
      '/tmp/optio-oracle-venv/bin/python harness/tools/planning-improvement-bench.py --time 10',
      '/tmp/optio-oracle-venv/bin/python harness/tools/native-certification-smoke.py --time 120 --threads 8',
      'python3 harness/tools/planning-improvement-report.py', '```', '']
    (OUT/'README.md').write_text('\n'.join(lines))
    native44=[r for r in records if '/lp/' in r['file']]
    (OUT/'native-regressions.json').write_text(json.dumps(native44,indent=2)+'\n')
    build={'nativeSha256':native['binarySha256'],'nativeFeatures':['parallel'],'wasm':size,
           'rustTestsPassed':42,'rustTestsIgnored':2,'wasmRegressionsPassed':44,'nativeFixturesAttempted':59,
           'supportedNativeFixtures':len(supported),'statuses':dict(counts)}
    (OUT/'build.json').write_text(json.dumps(build,indent=2)+'\n')
    rows=[]
    for r in measured:
        if r['name'] in ['planning-export','planning-infeasible']:continue
        a,b=r['after'],r['before']
        rows.append(f"<tr><td>{r['name'].removeprefix('planning-')}</td><td>{r['runtime']}</td><td>{r['rate']:.2f}×</td><td>{b['iterations']/max(b['nodes'],1):.0f} → {a['iterations']/max(a['nodes'],1):.0f}</td><td>{fmt(b['objective'])} → {fmt(a['objective'])}</td></tr>")
    page='''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Planning performance results</title><style>body{margin:0;padding:12px 0;font:15px system-ui;color:var(--foreground,#253047)}h1{font-size:23px;margin:0 0 10px}h2{font-size:18px;margin:22px 0 10px}p{line-height:1.5;color:var(--muted-foreground,#58647a)}table{border-collapse:collapse;width:100%;font-size:14px}th,td{text-align:left;padding:9px 8px;border-bottom:1px solid var(--border,#e3e8ef)}th{color:var(--muted-foreground,#526077);font-weight:600}td:nth-child(3){color:var(--success,#08735d);font-weight:600}.scroll{overflow:auto}.metrics{display:flex;gap:25px;flex-wrap:wrap}.metrics b{display:block;font-size:26px;color:var(--success,#08735d)}.metrics span{color:var(--muted-foreground,#58647a);font-size:13px}select{font:inherit;padding:5px;margin:8px 0}@media(max-width:500px){table{font-size:12px}th,td{padding:9px 4px}body:not([data-filter="all"]) td:nth-child(2),body:not([data-filter="all"]) th:nth-child(2){display:none}}</style><body><h1>Less repeated LP work, faster planning search</h1><p>Parent LP bases, weighted presolve, integer strengthening, root cuts, pseudocost branching and reused scratch arrays benefit both native and WASM.</p>'''
    page+=f'<div class="metrics"><div><b>42</b><span>Rust tests passed</span></div><div><b>44</b><span>WASM regressions passed</span></div><div><b>{counts[7]}/{len(supported)}</b><span>Optio optima proved</span></div></div>'
    page+='<h2>Matched 10-second runs</h2><p>Relative B&B nodes per second. Different trees: this measures work rate, not time to prove optimality. All returned solutions pass independent checks; objectives are shown below.</p><label>Runtime <select id="runtime"><option>wasm</option><option>native-1</option><option>native-8</option><option value="all">All</option></select></label><div class="scroll"><table><thead><tr><th>Problem</th><th>Runtime</th><th>Work rate</th><th>LP iterations/node</th><th>Objective before → after</th></tr></thead><tbody>' + ''.join(rows) + '</tbody></table></div>'
    page+=f'<h2>Proof search still trails the reference solvers</h2><p>All 59 fixtures attempted at 120 seconds / eight threads. One unsupported semi-domain fixture is excluded. New Optio certificates: exceeds_stack (1,354) and issue-2585 (−175.91). No new planning optimum certificate. The longer SCIP searches found a better tight solution: <b>4,750.39</b>, with bound <b>4,686.87</b>. Root cuts and faster nodes have not closed the remaining proof gaps.</p><p>True infeasible planning fixture: presolve rejection replaces approximately two seconds of heuristic work. Sudoku stays under 0.25 ms in measured native/WASM medians.</p><p>WASM payload: {size["rawBytes"]:,} bytes raw / {size["brotliBytes"]:,} Brotli. Adaptive early stopping remains optional because it reduced solution quality.</p><h2>Next shared-core priorities</h2><p>Reduce cold LP fallbacks and reuse numeric factorizations; strengthen relaxations with more useful cuts and broader presolve; then add reliability branching and retune heuristic budgets using repeated quality runs.</p>'
    page+='''<script>const f=document.querySelector('select');function draw(){document.body.dataset.filter=f.value;for(const r of document.querySelectorAll('tbody tr'))r.hidden=f.value!=='all'&&r.cells[1].textContent!==f.value}f.onchange=draw;draw();</script></body></html>'''
    (OUT/'comparison.html').write_text(page)
if __name__=='__main__':main()
