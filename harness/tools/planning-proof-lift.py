#!/usr/bin/env python3
"""Recover an original witness from aggregate choices/loads using assignment MIPs."""
import argparse
import importlib.util
import json
import pathlib
import subprocess
import time
from collections import defaultdict
import highspy

ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('oracle', ROOT/'harness/tools/native-oracle.py')
oracle = importlib.util.module_from_spec(spec);spec.loader.exec_module(oracle)


def main():
    p=argparse.ArgumentParser();p.add_argument('--model',required=True);p.add_argument('--aggregate-dir',required=True)
    p.add_argument('--label',default='integer-loads');p.add_argument('--time',type=float,default=15.)
    p.add_argument('--optio',default=str(ROOT/'target/release/optio-bench'));p.add_argument('--grouped',action='store_true');args=p.parse_args()
    m=json.loads(pathlib.Path(args.model).read_text());directory=pathlib.Path(args.aggregate_dir)
    meta=json.loads((directory/f'{args.label}.derivation.json').read_text())
    reduced=json.loads((directory/f'{args.label}.optio.json').read_text());solution=reduced['colValue']
    assert reduced['statusCode']==7
    a=m['matrix'];rows=[list(zip(a['indices'][a['starts'][r]:a['starts'][r+1]],a['values'][a['starts'][r]:a['starts'][r+1]])) for r in range(m['numRows'])]
    x=[0.]*m['numCols']
    for j,v in zip(meta['keptColumns'],solution):x[j]=round(v) if m['integrality'][j] else v
    feeder={int(j):g for g,terms in enumerate(meta['feeders']) for j in terms}
    durations={int(j):d for terms in meta['feeders'] for j,d in terms.items()}
    h=highspy.Highs();h.setOptionValue('output_flag',False)
    demands=[];load_terms=[[] for _ in meta['feeders']]
    for total in [r for r in meta['addedRows'] if r['kind']=='session-total']:
        for r in total['sourceRows']:
            terms=rows[r];q=-sum(v*x[j] for j,v in terms if j not in feeder)
            assert abs(q-round(q))<1e-7
            q=round(q)
            if not q:continue
            options=[]
            for j,v in terms:
                if j not in feeder:continue
                assert v==1
                if not (m['colLower'][j]<=q<=m['colUpper'][j]):continue
                col=h.getNumCol()
                if not args.grouped:
                    h.addVar(0.,1.);h.changeColIntegrality(col,highspy.HighsVarType.kInteger)
                options.append((col,j));load_terms[feeder[j]].append((col,q*durations[j]))
            assert options
            if not args.grouped:h.addRow(1.,1.,len(options),[c for c,j in options],[1.]*len(options))
            demands.append((r,q,options))
    batches=defaultdict(list)
    if args.grouped:
        load_terms=[[] for _ in meta['feeders']]
        for r,q,options in demands:
            key=tuple(sorted((feeder[j],q*durations[j]) for c,j in options))
            batches[key].append((r,q,options))
        batch_options=[]
        for key,items in batches.items():
            options=[]
            for g,weight in key:
                col=h.getNumCol();h.addVar(0.,float(len(items)));h.changeColIntegrality(col,highspy.HighsVarType.kInteger)
                options.append((col,g));load_terms[g].append((col,weight/meta['loadUnits'][g]))
            h.addRow(len(items),len(items),len(options),[c for c,g in options],[1.]*len(options))
            batch_options.append((items,options))
    targets=[]
    for g,terms in enumerate(load_terms):
        target=round(solution[meta['loadColumns'][g]])*meta['loadUnits'][g];targets.append(target)
        rhs=target/meta['loadUnits'][g] if args.grouped else target
        h.addRow(rhs,rhs,len(terms),[j for j,v in terms],[v for j,v in terms])
    h.ensureColwise();assignment=oracle.model_json(h.getLp())
    suffix='grouped-assignment' if args.grouped else 'assignment'
    path=directory/f'{args.label}.{suffix}.model.json';path.write_text(json.dumps(assignment)+'\n')
    h.setOptionValue('threads',1);h.setOptionValue('time_limit',args.time);h.setOptionValue('mip_rel_gap',0.);h.setOptionValue('mip_abs_gap',0.)
    started=time.monotonic();h.run();elapsed=time.monotonic()-started
    print('HiGHS assignment',h.getNumCol(),h.getNumRow(),h.getModelStatus(),'seconds',elapsed,flush=True)
    reference={'status':str(h.getModelStatus()),'seconds':elapsed,'nodes':h.getInfo().mip_node_count,'check':oracle.check(assignment,list(h.getSolution().col_value),1e-7)}
    native=json.loads(subprocess.check_output([args.optio,'--model-json',str(path),'--time',str(args.time),'--gap','0','--threads','1'],text=True))
    native['check']=oracle.check(assignment,native['colValue'],1e-7) if native['colValue'] else None
    (directory/f'{args.label}.{suffix}-results.json').write_text(json.dumps({'reference':reference,'optio':native},indent=2)+'\n')
    print('Optio assignment','ms',native['ms'],'status',native['statusCode'],'nodes',native['nodes'], 'feasible',native['check'],flush=True)
    # Use only Optio's assignment to establish an Optio-only witness chain.
    if not native['check'] or not native['check']['feasible']:return
    chosen=[]
    if args.grouped:
        demands=[]
        for items,options in batch_options:
            assignments=[]
            for c,g in options:assignments.extend([g]*round(native['colValue'][c]))
            assert len(assignments)==len(items)
            for (r,q,item_options),g in zip(items,assignments):
                j=next(j for _,j in item_options if feeder[j]==g)
                x[j]=q;chosen.append(j)
    for r,q,options in demands:
        assigned=[j for c,j in options if native['colValue'][c]>0.5]
        assert len(assigned)==1
        x[assigned[0]]=q;chosen.append(assigned[0])
    round_to_x={};choice_cols={j for j in meta['keptColumns'] if m['integrality'][j]}
    for r,terms in enumerate(rows):
        if m['rowLower'][r]!=0 or m['rowUpper'][r]!=0:continue
        feeds=[j for j,v in terms if j in feeder and v==1]
        if len(feeds)!=1 or any(j!=feeds[0] and (v!=-1 or j in feeder or j in choice_cols or not m['integrality'][j]) for j,v in terms):continue
        for j,v in terms:
            if j!=feeds[0]: round_to_x[j]=feeds[0]
    for r,terms in enumerate(rows):
        if m['rowLower'][r]!=0 or m['rowUpper'][r]!=0:continue
        allocations=[j for j,v in terms if j in round_to_x and v==1]
        if not allocations or any(j not in round_to_x and j not in choice_cols for j,v in terms):continue
        q=-sum(v*x[j] for j,v in terms if j in choice_cols)
        if q:
            candidates=[j for j in allocations if round_to_x[j] in chosen]
            assert len(candidates)==1
            x[candidates[0]]=round(q)
    for r,terms in enumerate(rows):
        if len(terms)!=2 or m['rowUpper'][r]!=0 or m['rowLower'][r]>-1e29:continue
        feeds=[j for j,v in terms if j in feeder and v>0]
        if len(feeds)!=1:continue
        for j,v in terms:
            if j not in feeder and m['integrality'][j] and v<0 and m['colUpper'][j]<=1:
                x[j]=float(x[feeds[0]]>0)
    check=oracle.check(m,x,1e-9)
    print('original witness',check, 'bound',reduced['dualBound'],flush=True)
    (directory/f'{args.label}.lifted.json').write_text(json.dumps({'check':check,'objective':check['objective'],'bound':reduced['dualBound'],'colValue':x},indent=2)+'\n')
    assert check['feasible'] and abs(check['objective']-reduced['objective'])<1e-6

if __name__=='__main__':main()
