"""Summarize the checked proof-foundation sweep and its archived baseline."""
import json
import pathlib
import sys

p = pathlib.Path(sys.argv[1])
r = json.loads(p.read_text())
records = r['records']
keys = ['baselineNative', 'native', 'baselineWasm', 'wasm', 'referenceHighsNative']

def feasible(v):
    return bool(v.get('check') and v['check'].get('feasible') and v.get('objective') is not None)

s = {'fixtures': len(records), 'solvers': {}, 'changes': {}}
for k in keys:
    vs = [x[k] for x in records]
    s['solvers'][k] = {
        'optimal': sum(v['statusCode'] == 7 for v in vs),
        'completed': sum(v['statusCode'] in [7, 8, 10] for v in vs),
        'incumbents': sum(feasible(v) for v in vs),
        'seconds': sum(v['ms'] for v in vs) / 1000,
        'unresolved': [x['file'] for x in records if x[k]['statusCode'] not in [7, 8, 10]],
    }
for k, old in [('native', 'baselineNative'), ('wasm', 'baselineWasm')]:
    c = {'betterObjective': [], 'worseObjective': [], 'newIncumbent': [], 'lostIncumbent': [],
         'newProof': [], 'lostProof': []}
    for x in records:
        v, b = x[k], x[old]
        assert not v['errors']
        if feasible(v) and feasible(b):
            d = x['sense'] * (v['objective'] - b['objective'])
            tol = 1e-6 * (1 + abs(b['objective']))
            if d < -tol: c['betterObjective'].append(x['file'])
            if d > tol: c['worseObjective'].append(x['file'])
        if feasible(v) and not feasible(b): c['newIncumbent'].append(x['file'])
        if feasible(b) and not feasible(v): c['lostIncumbent'].append(x['file'])
        if v['statusCode'] == 7 and b['statusCode'] != 7: c['newProof'].append(x['file'])
        if b['statusCode'] == 7 and v['statusCode'] != 7: c['lostProof'].append(x['file'])
    s['changes'][k] = c
for k in ['native', 'wasm']:
    c = {'better': [], 'worse': [], 'equal': [], 'referenceOnlyWitness': []}
    for x in records:
        v, b = x[k], x['referenceHighsNative']
        if feasible(v) and feasible(b):
            d = x['sense'] * (v['objective'] - b['objective'])
            tol = 1e-6 * (1 + abs(b['objective']))
            c['better' if d < -tol else 'worse' if d > tol else 'equal'].append(x['file'])
        elif feasible(b): c['referenceOnlyWitness'].append(x['file'])
    s.setdefault('objectiveVsNativeHighs', {})[k] = c
p.with_name('summary.json').write_text(json.dumps(s, indent=2) + '\n')
print(json.dumps(s, indent=2))
