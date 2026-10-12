#!/usr/bin/env python3
"""Recheck every fixture against archived native proofs, without rerunning the oracle."""
import argparse,hashlib,importlib.util,json,pathlib,subprocess,time
import highspy
ROOT=pathlib.Path(__file__).resolve().parents[2]
s=importlib.util.spec_from_file_location('oracle',ROOT/'harness/tools/native-oracle.py');o=importlib.util.module_from_spec(s);s.loader.exec_module(o)
def main():
 p=argparse.ArgumentParser();p.add_argument('--time',type=float,default=120);p.add_argument('--threads',type=int,default=8);p.add_argument('--binary',default=str(ROOT/'target/release/optio-bench'));p.add_argument('--out',default=str(ROOT/'docs/planning-improvements-2026-10-11/final-native.json'));p.add_argument('--archived-only',action='store_true',help='only fixtures with an archived reference');a=p.parse_args()
 refs={r['file']:r for r in json.loads((ROOT/'docs/optimality-2026-10-11/bounds.json').read_text())};records=[]
 files=sorted((ROOT/'harness/fixtures').glob('*.lp'))+sorted((ROOT/'harness/fixtures/lp').glob('*.lp'))
 for f in files:
  rel=str(f.relative_to(ROOT));sha=hashlib.sha256(f.read_bytes()).hexdigest()
  if rel not in refs and a.archived_only:continue
  ref=refs[rel];assert sha==ref['sha256']
  h=highspy.Highs();h.setOptionValue('output_flag',False);h.readModel(str(f));m=o.model_json(h.getLp());supported=all('Semi' not in str(t) for t in h.getLp().integrality_)
  model=pathlib.Path('/tmp/optio-planning-models')/(f.stem+'.json');model.parent.mkdir(exist_ok=True);model.write_text(json.dumps(m))
  r=json.loads(subprocess.check_output([a.binary,'--model-json',str(model),'--time',str(a.time),'--threads',str(a.threads),'--gap','0'],text=True,timeout=a.time+30));x=r.pop('colValue',[]);check=o.check(m,x) if x else None
  tol=1e-7*(1+abs(ref['objective'] or 0));passed=True;errors=[]
  if supported:
   if r['objective'] is not None and not (check or {}).get('feasible'):errors.append('invalid incumbent')
   if r['statusCode']==7:
    if ref['status']=='Optimal' and abs(r['objective']-ref['objective'])>tol:errors.append('incorrect optimality claim')
    elif ref['status'] in ['Infeasible','Unbounded']:errors.append('optimal status contradicts oracle')
    elif ref['status']=='Open' and (r['dualBound'] is None or abs(r['objective']-r['dualBound'])>tol or (ref['objective'] is not None and m['sense']*(r['objective']-ref['objective'])>tol)):errors.append('unsubstantiated new optimality claim')
   if r['statusCode']==8 and ref['status']!='Infeasible':errors.append('incorrect infeasibility claim')
   if r['statusCode']==10 and ref['status']!='Unbounded':errors.append('incorrect unboundedness claim')
   if r['dualBound'] is not None and ref['objective'] is not None and m['sense']*(r['dualBound']-ref['objective'])>tol:errors.append('bound exceeds checked reference witness')
   if ref['status']=='Infeasible' and r['statusCode'] not in [8,9]:errors.append('lost infeasibility classification')
  r.update(file=rel,sha256=sha,check=check,colValue=x,reference=ref,optioSupportsVariableTypes=supported,errors=errors)
  r["pass"]=not errors
  records.append(r);target=pathlib.Path(a.out);tmp=target.with_suffix('.tmp');tmp.write_text(json.dumps({'secondsPerSolve':a.time,'threads':a.threads,'binarySha256':hashlib.sha256(pathlib.Path(a.binary).read_bytes()).hexdigest(),'records':records},indent=2,allow_nan=False)+'\n');tmp.replace(target)
  print(f.name,'status',r['statusCode'],'obj',r['objective'],'bound',r['dualBound'],'seconds',r['ms']/1000,'PASS' if not errors else errors,flush=True)
 assert all(r['pass'] for r in records)
if __name__=='__main__':main()
