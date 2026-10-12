"""Summarize checked suite results, rejecting contradicted proof claims."""
import json, pathlib, statistics, sys
p=pathlib.Path(sys.argv[1]);r=json.loads(p.read_text())
refs={x['file']:x for x in json.loads((pathlib.Path(__file__).resolve().parents[2]/'docs/optimality-2026-10-11/bounds.json').read_text())}
keys=['optioNative','optioWasm','highsNative','highsWasm']
records=[x for x in r['records'] if x['supported']]
def feasible(v):return bool(v.get('check') and v['check'].get('feasible') and v.get('objective') is not None)
for x in records:
    objs=[v['objective'] for k in keys if feasible(v:=x[k])]
    ref=refs[x['file']]
    if ref.get('objective') is not None:objs.append(ref['objective'])
    best=(min(objs) if x['sense']>0 else max(objs)) if objs else None
    x['bestCheckedObjective']=best
    tol=1e-6*(1+abs(best or 0))
    for k in keys:
        v=x[k];err=[];s=v.get('statusCode')
        if v.get('objective') is not None and not feasible(v):err.append('invalid incumbent')
        if s==7:
            if not feasible(v):err.append('optimal without checked witness')
            elif best is not None and x['sense']*(v['objective']-best)>tol:err.append('optimal contradicted by better checked witness')
        if s==8 and best is not None:err.append('infeasible contradicted by checked witness')
        if s==10 and ref['status']=='Optimal':err.append('unbounded contradicts archived proof')
        if v.get('dualBound') is not None and best is not None and x['sense']*(v['dualBound']-best)>tol:err.append('bound excludes checked witness')
        v['errors']=err
        v['validatedOptimal']=s==7 and not err
        v['validatedClassification']=not err and (s==7 or (s==8 and ref['status']=='Infeasible') or (s==10 and ref['status']=='Unbounded'))
summary={'supportedFixtures':len(records),'excluded':[x['file'] for x in r['records'] if not x['supported']], 'solvers':{},'pairs':{}}
for k in keys:
    vs=[x[k] for x in records]
    summary['solvers'][k]={
      'reportedOptimal':sum(v.get('statusCode')==7 for v in vs),
      'validatedOptimal':sum(v['validatedOptimal'] for v in vs),
      'validatedClassifications':sum(v['validatedClassification'] for v in vs),
      'checkedIncumbents':sum(feasible(v) for v in vs),
      'contradictions':sum(bool(v['errors']) for v in vs),
      'totalSolveSeconds':sum(v.get('ms',0) for v in vs)/1000,
      'open':[x['file'] for x in records if not x[k]['validatedClassification']],
      'issues':[{'file':x['file'],'errors':x[k]['errors']} for x in records if x[k]['errors']]}
for label,k,highs_key in [('optioNative','optioNative','highsNative'),('optioWasm','optioWasm','highsNative'),('wasmVsWasm','optioWasm','highsWasm')]:
    pair={'optioBetterObjective':0,'highsBetterObjective':0,'equalObjective':0,'optioOnlyIncumbent':0,'highsOnlyIncumbent':0,'neitherIncumbent':0,
      'commonClassifications':0,'optioFaster':0,'highsFaster':0}
    ratios=[]
    for x in records:
        a,b=x[k],x[highs_key];fa,fb=feasible(a),feasible(b)
        if fa and fb:
            d=x['sense']*(a['objective']-b['objective']);tol=1e-6*(1+abs(b['objective']))
            pair['equalObjective' if abs(d)<=tol else 'optioBetterObjective' if d<0 else 'highsBetterObjective']+=1
        else:pair['optioOnlyIncumbent' if fa else 'highsOnlyIncumbent' if fb else 'neitherIncumbent']+=1
        if a['validatedClassification'] and b['validatedClassification']:
            pair['commonClassifications']+=1
            pair['optioFaster' if a['ms']<b['ms'] else 'highsFaster']+=1
            ratios.append(a['ms']/b['ms'])
    pair['medianOptioToHighsTimeRatioOnCommonClassifications']=statistics.median(ratios)
    summary['pairs'][label]=pair
(p.parent/'summary.json').write_text(json.dumps(summary,indent=2)+'\n')
(p.parent/'validated-results.json').write_text(json.dumps(r,indent=2)+'\n')
print(json.dumps(summary,indent=2))
