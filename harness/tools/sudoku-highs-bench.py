#!/usr/bin/env python3
"""Idle-machine native HiGHS timing, solver time only, checked after every solve."""
import importlib.util,json,pathlib,statistics,time,highspy
spec=importlib.util.spec_from_file_location('oracle',pathlib.Path(__file__).with_name('native-oracle.py'));o=importlib.util.module_from_spec(spec);spec.loader.exec_module(o)
records=[]
for name in ['sudoku-easy','sudoku-hard']:
 runs=[]
 for rep in range(11):
  h=highspy.Highs();h.setOptionValue('output_flag',False);h.setOptionValue('threads',8);h.setOptionValue('time_limit',120);h.readModel(str(pathlib.Path(o.REPO)/'harness/fixtures/lp'/f'{name}.lp'))
  start=time.perf_counter();h.run();ms=(time.perf_counter()-start)*1000
  check=o.check(o.model_json(h.getLp()),list(h.getSolution().col_value))
  assert h.getModelStatus()==highspy.HighsModelStatus.kOptimal and check['feasible'] and check['objective']==0
  if rep>1:runs.append(ms)
 records.append({'name':name,'runtime':'native-8','solver':'HiGHS','medianMs':statistics.median(runs),'runs':runs,'iterations':h.getInfo().simplex_iteration_count,'nodes':h.getInfo().mip_node_count,'feasible':True})
pathlib.Path(o.REPO,'docs/optimality-2026-10-11/sudoku-highs.json').write_text(json.dumps(records,indent=2)+'\n')
print(json.dumps(records))
