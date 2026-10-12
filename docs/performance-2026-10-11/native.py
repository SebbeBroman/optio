import sys,os,json,time,statistics,subprocess,importlib.util
import highspy,swiglpk as g
from pyscipopt import Model
root='/Users/seb/Developer/optio'
spec=importlib.util.spec_from_file_location('oracle',root+'/harness/tools/native-oracle.py'); o=importlib.util.module_from_spec(spec);spec.loader.exec_module(o)
g.glp_term_out(g.GLP_OFF)
files=['planning-s','planning-faithful','planning-tight','planning-m','planning-l','lp/sudoku-easy','lp/sudoku-hard','lp/tsp-12.mip']
records=[]
def high(path,relax,limit):
 h=highspy.Highs();h.setOptionValue('output_flag',False);h.setOptionValue('threads',1);h.setOptionValue('time_limit',limit);h.readModel(path)
 if relax:h.changeColsIntegrality(h.getNumCol(),list(range(h.getNumCol())),[highspy.HighsVarType.kContinuous]*h.getNumCol())
 t=time.perf_counter();h.run();ms=(time.perf_counter()-t)*1000
 return ms,str(h.getModelStatus()),h.getObjectiveValue(),list(h.getSolution().col_value),h.getInfo().mip_gap,h
for mode in ['lp','mip']:
 for f in (files[:5] if mode=='lp' else files):
  path=root+'/harness/fixtures/'+f+'.lp';relax=mode=='lp';limit=5.; reps=3 if relax or f.startswith('lp/') else 1
  _,_,_,_,_,h=high(path,relax,limit);mj=o.model_json(h.getLp());names=list(h.getLp().col_names_)
  modelpath='/tmp/optio-performance/'+f.replace('/','_')+'-'+mode+'.json';json.dump(mj,open(modelpath,'w'))
  for solver in ['HiGHS','Optio','GLPK','SCIP']:
   runs=[]
   for rep in range(reps):
    if solver=='HiGHS':ms,status,obj,x,gap,_=high(path,relax,limit)
    elif solver=='Optio':
     cmd=[o.BENCH,'--model-json',modelpath,'--time',str(limit)]+(['--lp'] if relax else [])
     r=json.loads(subprocess.check_output(cmd,text=True));ms=r['ms'];status=r['statusCode'];obj=r.get('objective');x=r['colValue'];gap=None
    elif solver=='GLPK':
     p=g.glp_create_prob();g.glp_read_lp(p,None,path)
     if relax:
      for i in range(1,g.glp_get_num_cols(p)+1):g.glp_set_col_kind(p,i,g.GLP_CV)
     t=time.perf_counter()
     if relax:
      c=g.glp_smcp();g.glp_init_smcp(c);c.presolve=g.GLP_ON;c.msg_lev=g.GLP_MSG_OFF;c.tm_lim=int(limit*1000);g.glp_simplex(p,c);status=g.glp_get_status(p);obj=g.glp_get_obj_val(p);vals={g.glp_get_col_name(p,i):g.glp_get_col_prim(p,i) for i in range(1,g.glp_get_num_cols(p)+1)}
     else:
      c=g.glp_iocp();g.glp_init_iocp(c);c.presolve=g.GLP_ON;c.msg_lev=g.GLP_MSG_OFF;c.tm_lim=int(limit*1000);g.glp_intopt(p,c);status=g.glp_mip_status(p);obj=g.glp_mip_obj_val(p);vals={g.glp_get_col_name(p,i):g.glp_mip_col_val(p,i) for i in range(1,g.glp_get_num_cols(p)+1)}
     ms=(time.perf_counter()-t)*1000;x=[vals[n] for n in names];gap=None;g.glp_delete_prob(p)
    else:
     p=Model();p.hideOutput();p.readProblem(path);p.setRealParam('limits/time',limit);p.setIntParam('parallel/maxnthreads',1)
     if relax:
      for v in p.getVars():p.chgVarType(v,'C')
     t=time.perf_counter();p.optimize();ms=(time.perf_counter()-t)*1000;status=str(p.getStatus());gap=p.getGap()
     if p.getNSols():obj=p.getObjVal();vals={v.name:p.getVal(v) for v in p.getVars()};x=[vals[n] for n in names]
     else:obj=None;x=[]
     p.freeProb()
    chk=o.check(mj,x);runs.append(ms)
   rec={'runtime':'native','mode':mode,'fixture':f,'solver':solver,'ms':statistics.median(runs),'runs':runs,'status':status,'objective':obj,'feasible':chk.get('feasible'),'gap':gap}
   records.append(rec);print(json.dumps(rec),flush=True);json.dump(records,open('/tmp/optio-performance/native.json','w'),indent=2)
  if mode=='mip' and not f.startswith('lp/'):
   r=json.loads(subprocess.check_output([o.BENCH,'--model-json',modelpath,'--time','1'],text=True));rec={'runtime':'native','mode':'mip-1s','fixture':f,'solver':'Optio','ms':r['ms'],'objective':r['objective'],'feasible':o.check(mj,r['colValue'])['feasible'],'status':r['statusCode']};records.append(rec);print(json.dumps(rec),flush=True)
json.dump(records,open('/tmp/optio-performance/native.json','w'),indent=2)
print('VERSIONS',h.version(),g.glp_version(),Model().getMajorVersion())
