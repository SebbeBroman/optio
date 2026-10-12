#!/usr/bin/env python3
"""Matched real-app benchmark: frozen/current WASM and native HiGHS on identical CSR."""
import argparse, hashlib, json, pathlib, subprocess, time, math
import highspy
ROOT=pathlib.Path(__file__).resolve().parents[2]
p=argparse.ArgumentParser();p.add_argument('--before',required=True);p.add_argument('--after',required=True);p.add_argument('--out',required=True);p.add_argument('--reuse-before');p.add_argument('--reuse-reference');args=p.parse_args()
out=pathlib.Path(args.out);out.mkdir(parents=True,exist_ok=True)
def read(path):return json.loads(path.read_text())
def sha(path):return hashlib.sha256(path.read_bytes()).hexdigest()
def check(m,x):
 if len(x)!=m['numCols'] or not all(math.isfinite(v)for v in x):return {'feasible':False}
 a=m['matrix'];rows=[math.fsum(a['values'][k]*x[a['indices'][k]]for k in range(a['starts'][r],a['starts'][r+1]))for r in range(m['numRows'])]
 rb=max([0.]+[max(lo-v,v-hi)for v,lo,hi in zip(rows,m['rowLower'],m['rowUpper'])]);cb=max([0.]+[max(lo-v,v-hi)for v,lo,hi in zip(x,m['colLower'],m['colUpper'])]);ib=max([0.]+[abs(v-round(v))for v,t in zip(x,m['integrality'])if t])
 return {'feasible':max(rb,cb,ib)<=1e-6,'maxRowViolation':rb,'maxBoundViolation':cb,'maxIntegralityViolation':ib,'objective':math.fsum(c*v for c,v in zip(m['colCost'],x))}
def reference(m,start,seconds,threads):
 highspy.Highs.resetGlobalScheduler(True)
 h=highspy.Highs();h.setOptionValue('output_flag',False);h.setOptionValue('threads',threads);h.setOptionValue('time_limit',seconds);h.setOptionValue('mip_rel_gap',0.);h.setOptionValue('mip_abs_gap',0.)
 lp=highspy.HighsLp();lp.num_col_=m['numCols'];lp.num_row_=m['numRows'];lp.col_cost_=m['colCost'];lp.col_lower_=m['colLower'];lp.col_upper_=m['colUpper'];lp.row_lower_=m['rowLower'];lp.row_upper_=m['rowUpper'];lp.integrality_=[highspy.HighsVarType.kInteger if t else highspy.HighsVarType.kContinuous for t in m['integrality']]
 a=m['matrix'];lp.a_matrix_.format_=highspy.MatrixFormat.kRowwise;lp.a_matrix_.start_=a['starts'];lp.a_matrix_.index_=a['indices'];lp.a_matrix_.value_=a['values'];assert h.passModel(lp)==highspy.HighsStatus.kOk
 if start:assert h.setSolution(len(start['indices']),start['indices'],start['values'])==highspy.HighsStatus.kOk
 t=time.perf_counter();h.run();ms=(time.perf_counter()-t)*1000;info=h.getInfo();sol=h.getSolution()
 return {'status':str(h.getModelStatus()),'objective':h.getObjectiveValue() if sol.value_valid else None,'dualBound':info.mip_dual_bound,'ms':ms,'threads':threads,'gap':info.mip_gap,'colValue':list(sol.col_value) if sol.value_valid else [],'check':check(m,list(sol.col_value)) if sol.value_valid else None}
prior=json.loads(pathlib.Path(args.reuse_before).read_text()) if args.reuse_before else []
prior_reference=json.loads(pathlib.Path(args.reuse_reference).read_text()) if args.reuse_reference else []
records=[]
for name,seconds in [('small',5),('medium',10),('large',20)]:
 path=ROOT/f'harness/fixtures/planning-app-{name}.model.json';seedpath=path.with_name(f'planning-app-{name}.start.json');m=read(path)
 for seeded in [True,False]:
  entry={'scenario':name,'seconds':seconds,'seeded':seeded,'modelSha256':sha(path),'seedSha256':sha(seedpath),'buildSha256':{'before':sha(pathlib.Path(args.before).with_name('optio_wasm_bg.wasm')),'after':sha(pathlib.Path(args.after).with_name('optio_wasm_bg.wasm'))},'referenceVersion':highspy.Highs().version(),'referenceThreads':1,'requestedGap':0}
  for tag,pkg in [('before',args.before),('after',args.after)]:
   reused=next((r for r in prior if r['scenario']==name and r['seeded']==seeded and r['seconds']==seconds and r['modelSha256']==sha(path) and r['seedSha256']==sha(seedpath) and r.get('buildSha256',{}).get('before')==entry['buildSha256']['before']),None) if tag=='before' else None
   if reused:entry[tag]=reused[tag];continue
   cmd=['node',str(ROOT/'harness/tools/planning-timing.mjs'),str(pathlib.Path(pkg).resolve()),str(path),str(seconds)]
   if seeded:cmd+=['--start',str(seedpath)]
   r=json.loads(subprocess.check_output(cmd,cwd=ROOT,text=True));entry[tag]=r
  ref=next((r for r in prior_reference if r['scenario']==name and r['seeded']==seeded and r['seconds']==seconds and r['modelSha256']==sha(path) and r['seedSha256']==sha(seedpath) and r.get('referenceVersion')==entry['referenceVersion'] and r.get('referenceThreads')==1 and r.get('requestedGap')==0),None)
  entry['highs']=ref['highs'] if ref else reference(m,read(seedpath) if seeded else None,seconds,1)
  records.append(entry);(out/'results.json').write_text(json.dumps(records,indent=2)+'\n')
  print(name,'seed' if seeded else 'no seed',[(tag,entry[tag]['objective'],round(entry[tag]['ms'],1))for tag in ['before','after','highs']],flush=True)
# Native parallel checks are independent of the seeded WASM comparison.
native=[]
for name,seconds in [('small',5),('medium',10),('large',20)]:
 for threads in [1,8]:
  cmd=[str(ROOT/'target/release/optio-bench'),'--model-json',str(ROOT/f'harness/fixtures/planning-app-{name}.model.json'),'--time',str(seconds),'--gap','0','--threads',str(threads)]
  native.append({'scenario':name,'threads':threads,'result':json.loads(subprocess.check_output(cmd,cwd=ROOT,text=True))})
(out/'native.json').write_text(json.dumps(native,indent=2)+'\n')
(out/'metadata.json').write_text(json.dumps({'highsVersion':highspy.Highs().version(),'beforeWasmSha256':sha(pathlib.Path(args.before).with_name('optio_wasm_bg.wasm')),'afterWasmSha256':sha(pathlib.Path(args.after).with_name('optio_wasm_bg.wasm')),'nativeSha256':sha(ROOT/'target/release/optio-bench'),'relativeGap':0,'absoluteGap':0},indent=2)+'\n')
