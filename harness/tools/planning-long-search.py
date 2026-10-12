#!/usr/bin/env python3
"""Sequential eight-thread native SCIP searches with original-model witnesses.
Recovery is a separate, bounded original-objective-cutoff feasibility search.
No solver code or fixture is changed. Native optimal statuses are numerical certificates.
"""
import argparse, hashlib, importlib.util, json, math, os, pathlib, time
import highspy
from pyscipopt import Model
ROOT=pathlib.Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('oracle',ROOT/'harness/tools/native-oracle.py');o=importlib.util.module_from_spec(spec);spec.loader.exec_module(o)
CASES=[('planning-export',536748.44),('planning-s',661.1399999997722),('planning-m',4377.47),('planning-l',9945.03),('planning-tight',4802.040000000002)]
def finite(v):return float(v) if v is not None and math.isfinite(v) and abs(v)<1e19 else None
def atomic(path,data):
 tmp=path.with_suffix(path.suffix+'.tmp');tmp.write_text(json.dumps(data,indent=2,allow_nan=False)+'\n');os.replace(tmp,path)
def checked(model,x):
 if len(x)!=model['numCols'] or any(not math.isfinite(v) for v in x):return {'feasible':False,'error':'invalid vector'}
 return o.check(model,x,1e-6)
def main():
 ap=argparse.ArgumentParser();ap.add_argument('--time',type=float,default=600);ap.add_argument('--recovery-time',type=float,default=45);ap.add_argument('--threads',type=int,default=8);a=ap.parse_args()
 out=ROOT/'docs/optimality-2026-10-11/long-search';out.mkdir(parents=True,exist_ok=True)
 results=[];started=time.time();scanned=[];candidates={name:[] for name,_ in CASES}
 # Only vectors with known original column order/names are usable; current reports contain none.
 for artifact in [ROOT/'harness/native-report.json',ROOT/'harness/oracle-report.json']:
  if not artifact.exists():continue
  scanned.append(str(artifact.relative_to(ROOT)))
  data=json.loads(artifact.read_text())
  for r in data.get('records',[]) if isinstance(data,dict) else data:
   name=pathlib.Path(r.get('file',r.get('name',''))).stem
   if name=='planning-infeasible':name='planning-export'
   if name not in candidates:continue
   for key in ['optio','highs','reference']:
    vec=r.get(key,{}).get('colValue')
    if vec:candidates[name].append((str(artifact.relative_to(ROOT))+':'+key,vec))
 metadata={'secondsPerSearch':a.time,'recoverySecondsBudget':a.recovery_time,'threads':a.threads,'highsVersion':highspy.Highs().version(),'artifactScan':scanned,'startedEpoch':started,'proofQualification':'Native solver numerical certificates, not formal mathematical proofs.'}
 def save():atomic(out/'results.json',{**metadata,'records':results,'elapsedSeconds':time.time()-started})
 for name,best in CASES:
  path=ROOT/'harness/fixtures'/f'{name}.lp'
  deadline=time.monotonic()+120
  while not path.exists():
   if time.monotonic()>deadline:raise RuntimeError('fixture rename not available')
   time.sleep(1)
  payload=path.read_bytes();sha=hashlib.sha256(payload).hexdigest();(out/f'{name}.lp').write_bytes(payload)
  h=highspy.Highs();h.setOptionValue('output_flag',False);assert h.readModel(str(out/f'{name}.lp'))==highspy.HighsStatus.kOk
  lp=h.getLp();model=o.model_json(lp);names=list(lp.col_names_);assert len(names)==model['numCols'] and len(set(names))==len(names)
  rec={'file':str(path.relative_to(ROOT)),'sha256':sha,'knownBestObjective':best,'variableNames':names,'colValue':None,'proofStatus':'pending','state':'recovering'};results.append(rec);save()
  seed=None;seed_source=None;recovery_started=time.monotonic()
  for source,vec in candidates[name]:
   chk=checked(model,vec)
   if chk['feasible'] and (seed is None or chk['objective']<checked(model,seed)['objective']):seed=list(vec);seed_source=source
  recovery={'artifactCandidates':len(candidates[name]),'strategy':'original-objective cutoff row with zero feasibility objective','cutoff':best+1e-6,'dualBoundApplicableToOriginal':False}
  if seed is None or checked(model,seed)['objective']>best+1e-6:
   # Cutoff constrains the ORIGINAL objective (including offset); its feasibility dual is never reported as a bound.
   costs=model['colCost'];indices=[i for i,v in enumerate(costs) if v];values=[costs[i] for i in indices]
   h.addRow(-highspy.kHighsInf,best+1e-6-model['offset'],len(indices),indices,values)
   h.changeColsCost(model['numCols'],list(range(model['numCols'])),[0.0]*model['numCols']);h.changeObjectiveOffset(0.0)
   for key,val in [('time_limit',a.recovery_time),('threads',a.threads),('mip_rel_gap',0.0),('mip_abs_gap',0.0),('mip_max_improving_sols',1)]:h.setOptionValue(key,val)
   print(f'{name}: live cutoff recovery, budget {a.recovery_time}s, {a.threads} threads; output {out}',flush=True)
   t=time.monotonic();h.run();recovery.update(status=str(h.getModelStatus()),solveSeconds=time.monotonic()-t)
   sol=h.getSolution();vec=list(sol.col_value) if sol.value_valid else [];chk=checked(model,vec);recovery['check']=chk
   if chk['feasible'] and (seed is None or chk['objective']<checked(model,seed)['objective']):seed=vec;seed_source='HiGHS original-objective-cutoff recovery'
  recovery['seconds']=time.monotonic()-recovery_started;rec['recovery']=recovery
  if seed is not None:atomic(out/f'{name}.seed.json',{'sha256':sha,'variableNames':names,'colValue':seed,'check':checked(model,seed),'source':seed_source})
  m=Model();m.readProblem(str(out/f'{name}.lp'));m.setLogfile(str(out/f'{name}.scip.log'));m.hideOutput()
  m.setRealParam('limits/time',a.time);m.setRealParam('limits/gap',0.0);m.setRealParam('limits/absgap',0.0);m.setIntParam('parallel/minnthreads',a.threads);m.setIntParam('parallel/maxnthreads',a.threads)
  byname={v.name:v for v in m.getVars()};accepted=False
  if seed is not None:
   sol=m.createSol()
   for n,v in zip(names,seed):m.setSolVal(sol,byname[n],v)
   accepted=bool(m.addSol(sol,free=True))
  rec.update(state='searching',seed={'source':seed_source,'objective':checked(model,seed)['objective'] if seed is not None else None,'accepted':accepted,'check':checked(model,seed) if seed is not None else None},searchStartedEpoch=time.time(),scipVersion=f'{m.getMajorVersion()}.{m.getMinorVersion()}.{m.getTechVersion()}',mode='solveConcurrent',threads=a.threads);save()
  print(f'{name}: LIVE SCIP 600s search; seed {rec["seed"]}; checkpoint {out / "results.json"}',flush=True)
  t=time.monotonic();m.solveConcurrent();dt=time.monotonic()-t
  vec=[float(m.getVal(byname[n])) for n in names] if m.getNSols() else [];chk=checked(model,vec)
  rec.update(state='complete',status=str(m.getStatus()),solverObjective=finite(m.getObjVal()) if vec else None,objective=chk.get('objective') if chk['feasible'] else None,dualBound=finite(m.getDualbound()),gap=finite(m.getGap()),seconds=dt,solverSeconds=m.getSolvingTime(),nodes=m.getNNodes(),check=chk,colValue=vec if chk['feasible'] else None,proofStatus='native numerical optimality certificate' if str(m.getStatus())=='optimal' and chk['feasible'] else 'open',totalProblemSeconds=time.monotonic()-recovery_started)
  if seed is not None and (rec['objective'] is None or checked(model,seed)['objective']<rec['objective']):rec.update(objective=checked(model,seed)['objective'],colValue=seed,check=checked(model,seed),witnessSource='recovery seed')
  atomic(out/f'{name}.json',rec);save();print(f'{name}: DONE status={rec["status"]} objective={rec["objective"]} dual={rec["dualBound"]} seconds={dt:.2f}',flush=True);m.freeProb()
 metadata['completedEpoch']=time.time();save()
 print('ALL FIVE COMPLETE',flush=True)
if __name__=='__main__':main()
