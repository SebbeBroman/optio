"""Cold, sequential native/WASM Optio/HiGHS comparison with checkpoints."""
import argparse, hashlib, importlib.util, json, math, pathlib, subprocess, time
import highspy
ROOT = pathlib.Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('oracle', ROOT/'harness/tools/native-oracle.py')
o = importlib.util.module_from_spec(spec); spec.loader.exec_module(o)
spec = importlib.util.spec_from_file_location('cert', ROOT/'harness/tools/optimality-bench.py')
cert = importlib.util.module_from_spec(spec); spec.loader.exec_module(cert)
def finite(v):
    return float(v) if v is not None and math.isfinite(v) and abs(v)<1e29 else None
def main():
    p=argparse.ArgumentParser()
    p.add_argument('--time', type=float, default=20)
    p.add_argument('--native', required=True); p.add_argument('--wasm', required=True)
    p.add_argument('--out', required=True); p.add_argument('--resume', action='store_true')
    a=p.parse_args(); out=pathlib.Path(a.out); out.parent.mkdir(parents=True,exist_ok=True)
    models=out.parent/'models'; models.mkdir(exist_ok=True)
    sha=lambda f:hashlib.sha256(pathlib.Path(f).read_bytes()).hexdigest()
    report={'secondsPerSolver':a.time, 'threads':1, 'warmStarts':False,
      'highsVersion':highspy.Highs().version(),
      'nativeSha256':sha(a.native), 'wasmSha256':sha(pathlib.Path(a.wasm).with_name('optio_wasm_bg.wasm')),
      'highsWasmSha256':sha(ROOT.parent/'highs-js/build/highs.wasm'), 'records':[]}
    if a.resume and out.exists():
        old=json.loads(out.read_text())
        for k in report:
            if k!='records': assert old[k]==report[k], k
        report=old
    def save():
        tmp=out.with_suffix('.tmp');tmp.write_text(json.dumps(report,indent=2,allow_nan=False)+'\n');tmp.replace(out)
    def run(args):
        try:return json.loads(subprocess.check_output(args,text=True,timeout=a.time+15,cwd=ROOT))
        except (subprocess.SubprocessError,json.JSONDecodeError) as e:return {'error':str(e)}
    files=sorted((ROOT/'harness/fixtures').glob('*.lp'))+sorted((ROOT/'harness/fixtures/lp').glob('*.lp'))
    for i,f in enumerate(files):
        name=str(f.relative_to(ROOT))
        rec=next((r for r in report['records'] if r['file']==name),None)
        if rec is not None and all(k in rec for k in ['optioNative','optioWasm','highsNative','highsWasm']):continue
        h=highspy.Highs();h.setOptionValue('output_flag',False)
        h.setOptionValue('threads',1);h.setOptionValue('time_limit',a.time)
        h.setOptionValue('mip_rel_gap',0.);h.setOptionValue('mip_abs_gap',0.)
        assert h.readModel(str(f))==highspy.HighsStatus.kOk
        h.ensureColwise();m=o.model_json(h.getLp());types=[str(t) for t in h.getLp().integrality_]
        model=models/(f.stem+'.json');model.write_text(json.dumps(m))
        highs_model=models/(f.stem+'.highs.json')
        hm=dict(m);hm['integrality']=[int(t) for t in h.getLp().integrality_] if types else [0]*m['numCols']
        highs_model.write_text(json.dumps(hm))
        if rec is None:
            rec={'file':name,'sha256':sha(f),'sense':m['sense'],'integer':any(m['integrality']),
              'supported':not any('Semi' in t for t in types)}
            report['records'].append(rec)
        jobs=[('optioNative',[a.native,'--model-json',str(model.resolve()),'--time',str(a.time),'--threads','1','--gap','0']),
          ('optioWasm',['node','harness/tools/planning-timing.mjs',a.wasm,str(model.resolve()),str(a.time)]),
          ('highsNative',None),('highsWasm',['node','harness/tools/highs-suite-solve.mjs',str(highs_model.resolve()),str(a.time)])]
        for key,args in jobs:
            if key in rec:continue
            if args is None:
                t=time.perf_counter();h.run();ms=(time.perf_counter()-t)*1000
                info=h.getInfo();sol=h.getSolution();status=int(h.getModelStatus())
                obj=finite(h.getObjectiveValue()) if sol.value_valid else None
                r={'statusCode':status,'ms':ms,'objective':obj,
                  'dualBound':obj if status==7 else finite(info.mip_dual_bound) if any(m['integrality']) else None,
                  'colValue':list(sol.col_value) if sol.value_valid else [],'nodes':info.mip_node_count}
            else:r=run(args)
            x=r.get('colValue',[])
            r['check']=cert.domain_check(m,x,types) if x else None
            rec[key]=r;save()
            print(f'{i+1}/{len(files)} {f.name} {key}: status={r.get("statusCode")} objective={r.get("objective")} ms={r.get("ms")} feasible={(r.get("check") or {}).get("feasible")}',flush=True)
if __name__=='__main__':main()
