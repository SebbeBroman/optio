#!/usr/bin/env python3
"""Structural aggregate-load relaxations, for bound experiments, not witnesses.

Every added total/capacity-subset row is a positive combination of original
day-demand equalities plus nonnegative omitted loads. Local placement rows are
relaxed. An optimum here is only a lower bound on the original minimization.
"""
import argparse
import hashlib
import importlib.util
import json
import math
import pathlib
import subprocess
import time
from collections import defaultdict
import highspy

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('oracle', ROOT / 'harness/tools/native-oracle.py')
oracle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oracle)


def aggregate(m, integer_loads, restrictions):
    assert m['sense'] == 1
    a = m['matrix']
    rows = [list(zip(a['indices'][a['starts'][r]:a['starts'][r+1]], a['values'][a['starts'][r]:a['starts'][r+1]])) for r in range(m['numRows'])]
    groups, group_of, duration, sessions = [], {}, {}, defaultdict(list)
    for r, terms in enumerate(rows):
        cont = [(j, v) for j, v in terms if m['integrality'][j] == 0]
        ints = [(j, v) for j, v in terms if m['integrality'][j] != 0]
        if m['rowUpper'][r] != 0 or m['rowLower'][r] > -1e29 or len(cont) != 1 or cont[0][1] != -1 or m['colCost'][cont[0][0]] <= 0 or not ints or any(v <= 0 for _, v in ints):
            continue
        assert all(j not in group_of for j, _ in ints)
        assert all(v == int(v) and v <= 2**53 and m['colLower'][j] >= 0 for j, v in ints)
        g = len(groups)
        unit = math.gcd(*(int(v) for _, v in ints))
        groups.append({'feeders': dict(ints), 'unit': unit, 'peak': cont[0][0]})
        sessions[cont[0][0]].append(g)
        for j, v in ints:
            group_of[j], duration[j] = g, v
    assert groups
    choice_rows = [r for r, terms in enumerate(rows) if terms and m['rowLower'][r] == m['rowUpper'][r] == 1 and all(v == 1 and m['integrality'][j] and j not in group_of and m['colLower'][j] >= 0 and m['colUpper'][j] <= 1 for j, v in terms)]
    choices = {j for r in choice_rows for j, _ in rows[r]}
    kept = sorted(choices | {j for j in range(m['numCols']) if not m['integrality'][j]})
    assert all(m['colCost'][j] == 0 for j in range(m['numCols']) if j not in kept)
    col_map = {j: k for k, j in enumerate(kept)}
    h = highspy.Highs()
    h.setOptionValue('output_flag', False)
    h.changeObjectiveOffset(m.get('offset', 0.))
    for j in kept:
        h.addVar(m['colLower'][j], m['colUpper'][j])
        h.changeColCost(col_map[j], m['colCost'][j])
        if m['integrality'][j]: h.changeColIntegrality(col_map[j], highspy.HighsVarType.kInteger)
    load_cols = []
    for group in groups:
        unit = group['unit']
        j = h.getNumCol()
        load_cols.append(j)
        h.addVar(sum(d*m['colLower'][x] for x,d in group['feeders'].items()) / unit,
                 sum(d*m['colUpper'][x] for x,d in group['feeders'].items()) / unit)
        if integer_loads: h.changeColIntegrality(j, highspy.HighsVarType.kInteger)

    def add(lo, hi, coeff):
        terms = [(j, v) for j, v in sorted(coeff.items()) if v]
        h.addRow(lo, hi, len(terms), [j for j,_ in terms], [v for _,v in terms])

    retained, dropped = [], []
    for r, terms in enumerate(rows):
        coeff, by_group, valid = defaultdict(float), defaultdict(list), True
        for j, v in terms:
            if j in col_map: coeff[col_map[j]] += v
            elif j in group_of: by_group[group_of[j]].append((j, v))
            else: valid = False; break
        for g, entries in by_group.items():
            ratio = entries[0][1] / duration[entries[0][0]]
            if len(entries) != len(groups[g]['feeders']) or any(v / duration[j] != ratio for j,v in entries): valid = False; break
            coeff[load_cols[g]] += ratio * groups[g]['unit']
        if valid:
            add(m['rowLower'][r], m['rowUpper'][r], coeff)
            retained.append(r)
        else: dropped.append(r)

    demands, covered = [], set()
    for r, terms in enumerate(rows):
        if m['rowLower'][r] != 0 or m['rowUpper'][r] != 0: continue
        feeds = [(j,v) for j,v in terms if j in group_of]
        if not feeds or any(v != 1 for _,v in feeds) or any(j not in group_of and (j not in choices or v >= 0) for j,v in terms): continue
        d = duration[feeds[0][0]]
        assert all(duration[j] == d for j,_ in feeds)
        eligible = {group_of[j] for j,_ in feeds}
        peaks = {groups[g]['peak'] for g in eligible}
        assert len(peaks) == 1
        for j,_ in feeds:
            assert j not in covered
            covered.add(j)
        demands.append({'row': r, 'peak': next(iter(peaks)), 'eligible': eligible,
                        'choice': {col_map[j]: -d*v for j,v in terms if j in choices}, 'duration': d})
    assert covered == set(group_of), 'every feeder needs exactly one original day-demand equality'
    additions = []
    for peak, gs in sessions.items():
        ds = [d for d in demands if d['peak'] == peak]
        coeff = {load_cols[g]: groups[g]['unit'] for g in gs}
        for demand in ds:
            for j,v in demand['choice'].items(): coeff[j] = coeff.get(j,0.) - v
        add(0., 0., coeff)
        additions.append({'kind': 'session-total', 'sourceRows': [d['row'] for d in ds], 'multipliers': [d['duration'] for d in ds]})
        if restrictions:
            assert len(gs) <= 12
            for mask in range(1, (1 << len(gs))-1):
                subset = {g for k,g in enumerate(gs) if mask & (1 << k)}
                mandatory = [d for d in ds if d['eligible'] <= subset]
                if not mandatory: continue
                coeff = {load_cols[g]: groups[g]['unit'] for g in subset}
                for demand in mandatory:
                    for j,v in demand['choice'].items(): coeff[j] = coeff.get(j,0.) - v
                add(0., highspy.kHighsInf, coeff)
                additions.append({'kind': 'restricted-subset', 'groups': sorted(subset), 'sourceRows': [d['row'] for d in mandatory], 'multipliers': [d['duration'] for d in mandatory]})
    metadata = {'columns': h.getNumCol(), 'rows': h.getNumRow(), 'loadUnits': [g['unit'] for g in groups],
                'retainedOriginalRows': retained, 'droppedOriginalRows': dropped, 'addedRows': additions,
                'keptColumns': kept, 'loadColumns': load_cols, 'feeders': [g['feeders'] for g in groups]}
    return h, metadata


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--model', required=True)
    p.add_argument('--out', required=True)
    p.add_argument('--time', type=float, default=20.)
    p.add_argument('--witness', required=True, help='Optio benchmark result containing original colValue')
    p.add_argument('--optio', default=str(ROOT/'target/release/optio-bench'))
    args = p.parse_args()
    model = json.loads(pathlib.Path(args.model).read_text())
    x = json.loads(pathlib.Path(args.witness).read_text())['colValue']
    original_check = oracle.check(model, x, 1e-9)
    assert original_check['feasible']
    out = pathlib.Path(args.out); out.mkdir(parents=True, exist_ok=True)
    records = []
    for label, ints, restrictions in [('continuous-loads',False,False), ('integer-loads',True,False), ('integer-loads-subsets',True,True)]:
        started = time.monotonic()
        h, meta = aggregate(model, ints, restrictions)
        build_secs = time.monotonic()-started
        h.ensureColwise()
        reduced = oracle.model_json(h.getLp())
        projected = [x[j] for j in meta['keptColumns']] + [sum(x[int(j)]*d for j,d in feeders.items())/unit for feeders,unit in zip(meta['feeders'],meta['loadUnits'])]
        projection_check = oracle.check(reduced, projected, 1e-9)
        assert projection_check['feasible'] and abs(projection_check['objective']-original_check['objective']) < 1e-7
        path = out/f'{label}.model.json'; path.write_text(json.dumps(reduced)+'\n')
        (out/f'{label}.derivation.json').write_text(json.dumps(meta,indent=2)+'\n')
        h.setOptionValue('threads',1);h.setOptionValue('time_limit',args.time)
        h.setOptionValue('mip_rel_gap',0.);h.setOptionValue('mip_abs_gap',0.)
        h.setSolution(len(projected),list(range(len(projected))),projected)
        started = time.monotonic();h.run();elapsed=time.monotonic()-started
        info=h.getInfo(); solution=list(h.getSolution().col_value)
        record={'label':label,'buildSecs':build_secs,'solveSecs':elapsed,'columns':meta['columns'],'rows':meta['rows'],
                'status':str(h.getModelStatus()),'objective':info.objective_function_value,'bound':info.mip_dual_bound,
                'nodes':info.mip_node_count,'iterations':info.simplex_iteration_count,'check':oracle.check(reduced,solution,1e-7),
                'colValue':solution,'projectionCheck':projection_check}
        if info.mip_dual_bound > original_check['objective']+1e-6: raise AssertionError('bound contradicts original witness')
        records.append(record)
        (out/'results.json').write_text(json.dumps({'modelSha256':hashlib.sha256(pathlib.Path(args.model).read_bytes()).hexdigest(),'originalCheck':original_check,'records':records},indent=2)+'\n')
        print(label,meta['columns'],meta['rows'],record['status'],'seconds',elapsed,'obj',record['objective'],'bound',record['bound'],flush=True)
        result = json.loads(subprocess.check_output([args.optio,'--model-json',str(path),'--time',str(args.time),'--gap','0','--threads','1'],text=True))
        result['check']=oracle.check(reduced,result['colValue'],1e-7) if result['colValue'] else None
        (out/f'{label}.optio.json').write_text(json.dumps(result,indent=2)+'\n')
        print('Optio',label,'ms',result['ms'],'obj',result['objective'],'bound',result['dualBound'],'status',result['statusCode'],flush=True)

if __name__ == '__main__': main()
