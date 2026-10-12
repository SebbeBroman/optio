#!/usr/bin/env python3
"""Full fixture certification run: Optio then native HiGHS, 120s each, 8 workers.
HiGHS receives Optio's independently validated incumbent. Results checkpoint per solve.
"""
import argparse, hashlib, importlib.util, json, math, pathlib, subprocess, tempfile, time
import highspy
ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('oracle', ROOT/'harness/tools/native-oracle.py')
o = importlib.util.module_from_spec(spec); spec.loader.exec_module(o)
def finite(v):
    return float(v) if v is not None and math.isfinite(v) and abs(v) < 1e29 else None

def domain_check(model, x, types):
    if any(not math.isfinite(v) for v in x): return {"feasible":False,"error":"nonfinite solution"}
    original = dict(model)
    original['colLower'] = model['colLower'][:]
    original['integrality'] = model['integrality'][:]
    for j, ty in enumerate(types):
        if 'Semi' in ty and x[j] == 0.0: original['colLower'][j] = 0.0
        if 'SemiContinuous' in ty: original['integrality'][j] = 0
    return o.check(original, x)

def main():
    ap=argparse.ArgumentParser(); ap.add_argument('--time',type=float,default=120);ap.add_argument('--threads',type=int,default=8)
    ap.add_argument('--out',default=str(ROOT/'docs/optimality-2026-10-11/results.json'))
    ap.add_argument("--models-out",default="");ap.add_argument("--resume",action="store_true");args=ap.parse_args()
    files=sorted((ROOT/'harness/fixtures').glob('*.lp'))+sorted((ROOT/'harness/fixtures/lp').glob('*.lp'))
    old=json.loads(pathlib.Path(args.out).read_text()) if args.resume and pathlib.Path(args.out).exists() else {"records":[]}
    records=[r for r in old["records"] if "highs" in r]
    completed={r["file"] for r in records}
    build_hash=hashlib.sha256(pathlib.Path(o.BENCH).read_bytes()).hexdigest()
    def save():
        target=pathlib.Path(args.out);tmp=target.with_suffix('.tmp');tmp.write_text(json.dumps({'secondsPerSolver':args.time,'threads':args.threads,'highsVersion':highspy.Highs().version(),'records':records},indent=2,allow_nan=False)+'\n');tmp.replace(target)
    for path in files:
        if str(path.relative_to(ROOT)) in completed: continue
        h=highspy.Highs();h.setOptionValue('output_flag',False);h.setOptionValue('threads',args.threads)
        h.setOptionValue('time_limit',args.time);h.setOptionValue('mip_rel_gap',0.0);h.setOptionValue('mip_abs_gap',0.0)
        if h.readModel(str(path)) != highspy.HighsStatus.kOk: raise RuntimeError(f'parse failed: {path}')
        model=o.model_json(h.getLp());
        if args.models_out:
            model_dir=pathlib.Path(args.models_out);model_dir.mkdir(parents=True,exist_ok=True);(model_dir/(path.stem+".json")).write_text(json.dumps(model))
        types=[str(t) for t in h.getLp().integrality_]
        supported=all('Semi' not in t for t in types)
        with tempfile.NamedTemporaryFile('w',suffix='.json') as f:
            json.dump(model,f);f.flush()
            t=time.perf_counter()
            try:
                r=json.loads(subprocess.check_output([o.BENCH,'--model-json',f.name,'--time',str(args.time),'--threads',str(args.threads),'--gap','0'],text=True,timeout=args.time+5))
            except (subprocess.TimeoutExpired,subprocess.CalledProcessError) as e:r={'error':str(e),'ms':(time.perf_counter()-t)*1000}
        x=r.pop('colValue',[]);chk=o.check(model,x) if x else None
        if chk and chk.get('feasible') and supported:
            h.setSolution(len(x),list(range(len(x))),x)
        if x: r['colValue']=x
        rec={'nativeBuildSha256':build_hash,'file':str(path.relative_to(ROOT)),'optio':r,'check':chk,'optioSupportsVariableTypes':supported}
        records.append(rec);save()
        print(path.name,'Optio',r.get('statusCode',r.get('error')),r.get('objective'),'LB',r.get('dualBound'),'ms',round(r.get('ms',0),2),flush=True)
        t=time.perf_counter();h.run();dt=time.perf_counter()-t
        info=h.getInfo();status=str(h.getModelStatus());sol=h.getSolution()
        refcheck=domain_check(model,list(sol.col_value),types) if sol.value_valid else None
        ref={'status':status,'objective':finite(h.getObjectiveValue()) if sol.value_valid else None,'dualBound':finite(info.mip_dual_bound),'gap':finite(info.mip_gap),'seconds':dt,'nodes':info.mip_node_count,'check':refcheck,'warmStartedFromOptio':bool(chk and chk.get('feasible') and supported)}
        if not any(model['integrality']) and 'kOptimal' in status:ref['dualBound']=ref['objective']
        if sol.value_valid: ref['colValue']=list(sol.col_value)
        rec['highs']=ref
        sign=model['sense']
        bounds=[b for b in [r.get('dualBound') if supported else None,ref['dualBound']] if b is not None]
        bestbound=(max(bounds) if sign>0 else min(bounds)) if bounds else None
        objs=[obj for obj,c,sup in [(r.get('objective'),chk,supported),(ref['objective'],refcheck,True)] if obj is not None and c and c.get('feasible') and sup]
        bestobj=(min(objs) if sign>0 else max(objs)) if objs else None
        rec['bestObjective']=bestobj;rec['bestBound']=bestbound
        rec['absoluteGap']=max(0,sign*(bestobj-bestbound)) if bestobj is not None and bestbound is not None else None
        rec['provenOptimal']=bool(bestobj is not None and ('kOptimal' in status or (supported and r.get('statusCode')==7)) and (ref['objective'] is None or abs(bestobj-ref['objective']) <= 1e-5*max(1,abs(bestobj))))
        save();print('  HiGHS',status,ref['objective'],'LB',ref['dualBound'],'seconds',round(dt,2),flush=True)
if __name__=='__main__':main()
