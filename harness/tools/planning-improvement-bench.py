#!/usr/bin/env python3
"""Paired before/after planning benchmarks with independent witness checking."""
import argparse,hashlib,importlib.util,json,pathlib,subprocess,time
ROOT=pathlib.Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('oracle',ROOT/'harness/tools/native-oracle.py');o=importlib.util.module_from_spec(spec);spec.loader.exec_module(o)
def main():
 p=argparse.ArgumentParser();p.add_argument('--time',type=float,default=10);p.add_argument('--reps',type=int,default=1);p.add_argument('--names',nargs='+',default=['planning-s','planning-faithful','planning-m','planning-tight','planning-l','planning-export','planning-infeasible']);p.add_argument('--runtimes',nargs='+',default=['native-1','native-8','wasm']);p.add_argument('--out',default=str(ROOT/'docs/planning-improvements-2026-10-11/paired.json'));p.add_argument('--after-native',default=str(ROOT/'target/release/optio-bench'));p.add_argument('--after-wasm',default='/tmp/optio-planning-after/pkg/optio_wasm.js');a=p.parse_args()
 result={'secondsPerSolve':a.time,'repetitions':a.reps,'note':'Alternating paired solves, no compilation during measurements. Reference proof jobs finished before this run; no other benchmark or compilation runs concurrently. Absolute timing and multithread search are load-sensitive. LP seconds sum across workers.','records':[]}
 for name in a.names:
  model=pathlib.Path('/tmp/optio-planning-models')/(name+'.json');m=json.loads(model.read_text())
  for runtime in a.runtimes:
   for rep in range(a.reps):
    variants=['before','after'] if rep%2==0 else ['after','before']
    for variant in variants:
     exe=('/tmp/optio-planning-before/optio-bench' if variant=='before' else a.after_native) if runtime!='wasm' else ('/tmp/optio-planning-before/pkg-timed/optio_wasm.js' if variant=='before' else a.after_wasm)
     cmd=[exe,'--model-json',str(model),'--time',str(a.time),'--threads',runtime.split('-')[1],'--gap','0'] if runtime!='wasm' else ['node',str(ROOT/'harness/tools/planning-timing.mjs'),exe,str(model),str(a.time)]
     r=json.loads(subprocess.check_output(cmd,text=True,timeout=a.time+30));x=r.pop('colValue',[]);check=o.check(m,x) if x else None
     if r.get('objective') is not None and not (check or {}).get('feasible'):raise RuntimeError(f'{name} {variant} {runtime}: infeasible incumbent')
     r.update(name=name,variant=variant,runtime=runtime,repetition=rep,check=check,binarySha256=hashlib.sha256(pathlib.Path(exe if runtime!='wasm' else exe.replace('.js','_bg.wasm')).read_bytes()).hexdigest(),fixtureSha256=hashlib.sha256((ROOT/'harness/fixtures'/f'{name}.lp').read_bytes()).hexdigest())
     result['records'].append(r);target=pathlib.Path(a.out);target.parent.mkdir(parents=True,exist_ok=True);tmp=target.with_suffix('.tmp');tmp.write_text(json.dumps(result,indent=2,allow_nan=False)+'\n');tmp.replace(target)
     print(name,runtime,variant,'obj',r['objective'],'bound',r['dualBound'],'nodes',r['nodes'],'iters',r['iterations'],'timing',r.get('timing'),'LP',r.get('lpStats'),flush=True)
if __name__=='__main__':main()
