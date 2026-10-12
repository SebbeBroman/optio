#!/usr/bin/env python3
"""SCIP's concurrent portfolio attempts remaining full-suite proofs, 120s/problem."""
import argparse, importlib.util, json, math, pathlib, subprocess, tempfile, time
from pyscipopt import Model
import highspy
ROOT=pathlib.Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('oracle',ROOT/'harness/tools/native-oracle.py');o=importlib.util.module_from_spec(spec);spec.loader.exec_module(o)
def finite(x):return float(x) if math.isfinite(x) and abs(x)<1e19 else None

def main():
 ap=argparse.ArgumentParser();ap.add_argument('--results',default=str(ROOT/'docs/optimality-2026-10-11/results.json'));ap.add_argument('--time',type=float,default=120);ap.add_argument('--threads',type=int,default=8);ap.add_argument('--wait',action='store_true');a=ap.parse_args()
 p=pathlib.Path(a.results);expected=len(list((ROOT/'harness/fixtures').rglob('*.lp')))
 deadline=time.monotonic()+3600
 while True:
  try:data=json.loads(p.read_text())
  except (FileNotFoundError,json.JSONDecodeError):data={'records':[]}
  if len(data['records'])==expected and all('highs' in r for r in data['records']):break
  if not a.wait or time.monotonic()>deadline:raise RuntimeError('full run is incomplete')
  time.sleep(5)
 out=p.with_name('scip.json');records=[]
 for rec in data['records']:
  if rec.get('provenOptimal') or any(s in rec['highs']['status'] for s in ['kInfeasible','kUnbounded']):continue
  path=ROOT/rec['file'];h=highspy.Highs();h.setOptionValue('output_flag',False);h.readModel(str(path));model=o.model_json(h.getLp());names=list(h.getLp().col_names_)
  # Cheap, validated seed. Its budget is deducted from SCIP's 120 seconds.
  started=time.monotonic();warm=None
  with tempfile.NamedTemporaryFile('w',suffix='.json') as f:
   json.dump(model,f);f.flush()
   try:
    warm=json.loads(subprocess.check_output([o.BENCH,'--model-json',f.name,'--time','1','--threads',str(a.threads),'--gap','0'],text=True,timeout=5)).get('colValue')
   except Exception:pass
  m=Model();m.hideOutput();m.readProblem(str(path));m.setRealParam('limits/time',max(0.01,a.time-(time.monotonic()-started)));m.setRealParam('limits/gap',0);m.setRealParam('limits/absgap',0);m.setIntParam('parallel/minnthreads',a.threads);m.setIntParam('parallel/maxnthreads',a.threads)
  byname={v.name:v for v in m.getVars()};seeded=bool(warm and o.check(model,warm).get('feasible'))
  if seeded:
   sol=m.createSol()
   for name,val in zip(names,warm):m.setSolVal(sol,byname[name],val)
   seeded=bool(m.addSol(sol,free=True))
  print('SCIP concurrent',path.name,'seeded',seeded,flush=True)
  t=time.monotonic();m.solveConcurrent();seconds=time.monotonic()-t
  x=[m.getVal(byname[n]) for n in names] if m.getNSols() else []
  record={'file':rec['file'],'status':str(m.getStatus()),'objective':finite(m.getObjVal()) if x else None,'dualBound':finite(m.getDualbound()),'gap':finite(m.getGap()),'seconds':seconds,'threads':a.threads,'mode':'solveConcurrent','seeded':seeded,'check':o.check(model,x) if x else None}
  records.append(record);out.write_text(json.dumps(records,indent=2,allow_nan=False)+'\n');print(json.dumps(record),flush=True);m.freeProb()
 out.write_text(json.dumps(records,indent=2,allow_nan=False)+'\n')
if __name__=='__main__':main()
