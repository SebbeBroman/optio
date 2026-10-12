#!/usr/bin/env python3
"""Recheck long-search witnesses and merge only verified baseline improvements."""
import hashlib, importlib.util, json, math, pathlib
import highspy
ROOT=pathlib.Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('search',ROOT/'harness/tools/planning-long-search.py');s=importlib.util.module_from_spec(spec);spec.loader.exec_module(s)
OUT=ROOT/'docs/optimality-2026-10-11/long-search'
def main():
 data=json.loads((OUT/'results.json').read_text());assert len(data['records'])==5 and 'completedEpoch' in data
 verification=[]
 for r in data['records']:
  assert r['state']=='complete'
  lp=ROOT/r['file'];assert hashlib.sha256(lp.read_bytes()).hexdigest()==r['sha256']
  assert hashlib.sha256((OUT/lp.name).read_bytes()).hexdigest()==r['sha256']
  h=highspy.Highs();h.setOptionValue('output_flag',False);assert h.readModel(str(lp))==highspy.HighsStatus.kOk
  names=list(h.getLp().col_names_);assert names==r['variableNames'];model=s.o.model_json(h.getLp())
  chk=s.checked(model,r['colValue'] or []);assert chk['feasible'],(lp,chk)
  assert abs(chk['objective']-r['objective'])<1e-6
  obj=math.fsum(c*v for c,v in zip(model['colCost'],r['colValue']))+model['offset'];assert abs(obj-r['objective'])<1e-6
  rec={'file':r['file'],'sha256':r['sha256'],'columns':len(names),'check':chk,'fsumObjective':obj,'finalSolverStatus':r['status'],'finalApiDualBound':r['dualBound'],'proofStatus':r['proofStatus']}
  seed=OUT/(lp.stem+'.seed.json')
  if seed.exists():
   sd=json.loads(seed.read_text());assert sd['sha256']==r['sha256'] and sd['variableNames']==names
   check=s.checked(model,sd['colValue']);assert check['feasible'];rec['seedCheck']=check
  verification.append(rec)
 s.atomic(OUT/'verification.json',verification)
 changes=[];bounds_path=ROOT/'docs/optimality-2026-10-11/bounds.json';cert_path=ROOT/'harness/fixtures/certified-bounds.json';scip_path=ROOT/'docs/optimality-2026-10-11/scip.json'
 bounds=json.loads(bounds_path.read_text());cert=json.loads(cert_path.read_text());scip=json.loads(scip_path.read_text())
 for label,value in [('prior-bounds',bounds),('prior-certified-bounds',cert),('prior-scip',scip)]:
  p=OUT/(label+'.json')
  if not p.exists():s.atomic(p,value)
 byfile={r['file']:r for r in data['records']}
 for b in bounds:
  r=byfile.get(b['file'])
  if not r:continue
  assert b['sha256']==r['sha256']
  obj_improved=r['objective']<b['objective']-1e-6
  bound_improved=r['dualBound'] is not None and r['dualBound']>b['bound']+1e-6
  if not obj_improved and not bound_improved:continue
  old=dict(b)
  if obj_improved:b.update(objective=r['objective'],objectiveSource='SCIP',objectiveArtifact=str((OUT/(pathlib.Path(r['file']).stem+'.json')).relative_to(ROOT)))
  if bound_improved:b.update(bound=r['dualBound'],boundSource='SCIP')
  b.update(absoluteGap=max(0,b['objective']-b['bound']),relativeGap=max(0,b['objective']-b['bound'])/abs(b['objective']),longSearchSeconds=r['seconds'])
  b['solverStatuses']['SCIP']=r['status']
  if r['status']=='optimal' and b['absoluteGap']<=1e-6:b['status']='Optimal'
  changes.append({'file':b['file'],'before':old,'after':dict(b)})
  for i,c in enumerate(cert['records']):
   if c['file']==b['file']:assert c['sha256']==r['sha256'];cert['records'][i]=dict(b);break
  for i,c in enumerate(scip):
   if c['file']==r['file']:
    scip[i]={k:r[k] for k in ['file','sha256','status','objective','dualBound','gap','seconds','threads','mode','check','proofStatus','seed','recovery']}
    scip[i].update(seeded=r['seed']['accepted'],witnessArtifact=b.get('objectiveArtifact'),previousRun=c)
    break
 if changes:
  cert['method']+=' Independently rechecked long-search improvements supplement these baselines: 600-second SCIP 10.0.2 concurrent searches, eight threads, with separately timed cutoff recovery; witness artifacts record exact LP SHA-256 and full original vectors.'
  s.atomic(scip_path,scip);s.atomic(bounds_path,bounds);s.atomic(cert_path,cert)
 s.atomic(OUT/'baseline-updates.json',changes)
 lines=['# Planning long searches — 2026-10-11','','Five sequential SCIP 10.0.2 concurrent searches, eight threads, 600 seconds per original model. Separate HiGHS 1.15.1 original-objective-cutoff feasibility recovery was capped at 45 seconds per model. Existing native/oracle reports and the certification JSON artifacts did not retain usable solution vectors. No best vector was claimed as a seed without reconstruction and an independent check.','',f"Total elapsed: {data['elapsedSeconds']:.3f} seconds. No timeout is interpreted as infeasible. Native solver numerical certificates are not formal mathematical proofs.",'','## Results','']
 for r in data['records']:
  seed=r['seed'];lines.append(f"- **{pathlib.Path(r['file']).stem}**: {r['status']}; checked objective {r['objective']:.12g}; final API dual bound {r['dualBound']:.12g}; actual search {r['seconds']:.3f}s; recovery {r['recovery']['seconds']:.3f}s; seed objective {seed['objective']}, accepted {seed['accepted']}; proof status {r['proofStatus']}.")
 lines+=['','The concurrent live progress bound can be stronger than SCIP’s final returned solver bound (observed especially for planning-s). The persisted `dualBound` is `getDualbound()` after completion and matches the final solver summary. Rounded live display values are preserved in logs, and were not promoted into canonical baselines.','','Every final witness and saved seed was independently checked again against the current original LP, including exact hash, variable names, row feasibility, bounds, integrality, and objective. Complete vectors are in per-instance JSON and `results.json`; `verification.json` records the second checks. LP snapshots, SCIP logs, and recovery seed vectors are retained. Atomic checkpoints were written before recovery, before the main search, and after each completed search.','','## Baseline changes','']
 lines += [f"- {c['file']}: objective {c['before']['objective']} → {c['after']['objective']}; bound {c['before']['bound']} → {c['after']['bound']}; status {c['after']['status']}." for c in changes] or ['None.']
 lines+=['','Other historical incumbents and stronger historical dual bounds were preserved. `prior-*.json` and `baseline-updates.json` retain the pre-update baselines and exact changes. Solver code and fixture names were not changed. No commit was created.','','Reproduce searches:','','```sh','/tmp/optio-oracle-venv/bin/python harness/tools/planning-long-search.py','/tmp/optio-oracle-venv/bin/python harness/tools/planning-long-search-report.py','```','']
 (OUT/'README.md').write_text('\n'.join(lines));print(json.dumps({'verified':len(verification),'baselineUpdates':changes,'totalSeconds':data['elapsedSeconds']},indent=2))
if __name__=='__main__':main()
