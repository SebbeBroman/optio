"""Check current Optio builds against archived matched-budget suite results."""
import argparse, hashlib, importlib.util, json, pathlib, subprocess
import highspy
ROOT=pathlib.Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('oracle',ROOT/'harness/tools/native-oracle.py')
o=importlib.util.module_from_spec(spec);spec.loader.exec_module(o)
def main():
    p=argparse.ArgumentParser();p.add_argument('--baseline',required=True)
    p.add_argument('--native',required=True);p.add_argument('--wasm',required=True)
    p.add_argument('--out',required=True);p.add_argument('--time',type=float,default=20)
    p.add_argument('--names',default='');p.add_argument('--resume',action='store_true')
    a=p.parse_args();baseline=json.loads(pathlib.Path(a.baseline).read_text())
    refs={x['file']:x for x in json.loads((ROOT/'docs/optimality-2026-10-11/bounds.json').read_text())}
    out=pathlib.Path(a.out);out.parent.mkdir(parents=True,exist_ok=True)
    models=pathlib.Path('/tmp/optio-proof-foundation-models');models.mkdir(exist_ok=True)
    sha=lambda f:hashlib.sha256(pathlib.Path(f).read_bytes()).hexdigest()
    r={'secondsPerSolver':a.time,'threads':1,'baselineReport':a.baseline,'baselineReportSha256':sha(a.baseline),
       'nativeSha256':sha(a.native),'wasmSha256':sha(pathlib.Path(a.wasm).with_name('optio_wasm_bg.wasm')),'records':[]}
    if a.resume and out.exists():
        old=json.loads(out.read_text());assert all(old[k]==r[k] for k in r if k!='records');r=old
    def save():
        tmp=out.with_suffix('.tmp');tmp.write_text(json.dumps(r,indent=2,allow_nan=False)+'\n');tmp.replace(out)
    names=set(a.names.split(',')) if a.names else None
    for base in baseline['records']:
        if not base['supported'] or (names and pathlib.Path(base['file']).stem not in names):continue
        file=ROOT/base['file'];assert sha(file)==base['sha256']
        h=highspy.Highs();h.setOptionValue('output_flag',False);h.readModel(str(file));h.ensureColwise()
        m=o.model_json(h.getLp());model=models/(file.stem+'.json');model.write_text(json.dumps(m))
        rec=next((x for x in r['records'] if x['file']==base['file']),None)
        if rec is None:
            rec={'file':base['file'],'sha256':base['sha256'],'sense':m['sense'],'baselineNative':base['optioNative'],'baselineWasm':base['optioWasm'],'referenceHighsNative':base['highsNative']};r['records'].append(rec)
        for runtime in ['native','wasm']:
            if runtime in rec:continue
            args=[a.native,'--model-json',str(model),'--time',str(a.time),'--gap','0','--threads','1'] if runtime=='native' else ['node','harness/tools/planning-timing.mjs',a.wasm,str(model),str(a.time)]
            v=json.loads(subprocess.check_output(args,text=True,timeout=a.time+15,cwd=ROOT));x=v.get('colValue',[])
            check=o.check(m,x) if x else None;v['check']=check;errors=[]
            if v.get('objective') is not None and not (check or {}).get('feasible'):errors.append('invalid incumbent')
            known=[z['objective'] for k in ['optioNative','optioWasm','highsNative'] if (z:=base[k]).get('objective') is not None and (z.get('check') or {}).get('feasible')]
            ref=refs[base['file']]
            if ref.get('objective') is not None:known.append(ref['objective'])
            best=(min(known) if m['sense']>0 else max(known)) if known else None
            tol=1e-6*(1+abs(best or 0))
            if v.get('dualBound') is not None and best is not None and m['sense']*(v['dualBound']-best)>tol:errors.append('bound excludes reference witness')
            if v['statusCode']==7:
                if not (check or {}).get('feasible'):errors.append('Optimal without checked witness')
                elif best is not None and m['sense']*(v['objective']-best)>tol:errors.append('incorrect Optimal objective')
                if (any(m['integrality']) and v.get('dualBound') is None) or (v.get('dualBound') is not None and (v.get('objective') is None or abs(v['objective']-v['dualBound'])>1e-7*(1+abs(v.get('objective') or 0)))):errors.append('unclosed Optimal bound')
                if ref['status'] in ['Infeasible','Unbounded']:errors.append('Optimal contradicts reference')
            if v['statusCode']==8 and ref['status']!='Infeasible':errors.append('incorrect infeasibility')
            if v['statusCode']==10 and ref['status']!='Unbounded':errors.append('incorrect unboundedness')
            v['errors']=errors;rec[runtime]=v;save()
            print(file.name,runtime,'status',v['statusCode'],'obj',v.get('objective'),'bound',v.get('dualBound'),'ms',round(v['ms'],2),'nodes',v['nodes'],'phase1',v.get('lpStats',{}).get('phase1Iterations'),'errors',errors,flush=True)
            if errors:raise RuntimeError(errors)
if __name__=='__main__':main()
