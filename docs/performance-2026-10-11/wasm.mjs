import fs from 'node:fs';import {performance} from 'node:perf_hooks';import {createRequire} from 'node:module';
import loadHighs from '/Users/seb/Developer/highs-js/build/highs.mjs';
import GLPK from '/tmp/optio-perf-deps/node_modules/glpk.js/dist/glpk.js';
import {checkModel} from '/Users/seb/Developer/optio/harness/src/checker.mjs';
const require=createRequire(import.meta.url),opt=require('/tmp/optio-perf-wasm/optio_wasm.js'),hi=await loadHighs(),glpk=await GLPK();
const files=['planning-s','planning-faithful','planning-tight','planning-m','planning-l','lp/sudoku-easy','lp/sudoku-hard','lp/tsp-12.mip'];const records=[];
const median=x=>x.sort((a,b)=>a-b)[Math.floor(x.length/2)];
function bnd(lo,up){return {type:lo<=-1e30?(up>=1e30?glpk.GLP_FR:glpk.GLP_UP):up>=1e30?glpk.GLP_LO:lo===up?glpk.GLP_FX:glpk.GLP_DB,lb:lo,ub:up}}
for(const mode of ['lp','mip']) for(const f of mode==='lp'?files.slice(0,5):files){
 const m=JSON.parse(fs.readFileSync(`/tmp/optio-performance/${f.replace('/','_')}-${mode}.json`));const relax=mode==='lp', reps=relax||f.startsWith('lp/')?3:1;
 for(const solver of ['HiGHS','Optio','GLPK']){
  const runs=[];let status,objective,x,check,buildMs;
  for(let i=0;i<reps;i++){
   if(solver==='HiGHS'){
    const p=hi.createModel({format:'lp',data:fs.readFileSync(`/Users/seb/Developer/optio/harness/fixtures/${f}.lp`,'utf8')});p.options.set({output_flag:false,time_limit:5});
    if(relax)for(let j=0;j<m.numCols;j++)p.changeColIntegrality(j,0);
    const t=performance.now();p.run();runs.push(performance.now()-t);status=p.getModelStatus();objective=p.getObjectiveValue();x=Array.from(p.getSolution().colValue);p.dispose();
   }else if(solver==='Optio'){
    const p=new opt.Model();let t=performance.now();p.add_model(m.numCols,Float64Array.from(m.colCost),Float64Array.from(m.colLower),Float64Array.from(m.colUpper),Int32Array.from(m.integrality),Float64Array.from(m.rowLower),Float64Array.from(m.rowUpper),Uint32Array.from(m.matrix.starts),Uint32Array.from(m.matrix.indices),Float64Array.from(m.matrix.values));buildMs=performance.now()-t;p.set_time_limit(5);t=performance.now();p.run();runs.push(performance.now()-t);status=p.get_model_status();objective=p.get_objective_value();x=Array.from(p.get_solution_object().colValue);p.free();
   }else{
    const names=Array.from({length:m.numCols},(_,i)=>'x'+i),lp={name:f,objective:{direction:glpk.GLP_MIN,name:'obj',vars:names.map((name,i)=>({name,coef:m.colCost[i]}))},bounds:names.map((name,i)=>({name,...bnd(m.colLower[i],m.colUpper[i])})),subjectTo:Array.from({length:m.numRows},(_,r)=>({name:'r'+r,vars:Array.from({length:m.matrix.starts[r+1]-m.matrix.starts[r]},(_,j)=>{let k=m.matrix.starts[r]+j;return{name:names[m.matrix.indices[k]],coef:m.matrix.values[k]}}),bnds:bnd(m.rowLower[r],m.rowUpper[r])})),generals:names.filter((_,i)=>m.integrality[i]!==0)};
    const t=performance.now(),r=glpk.solve(lp,{msglev:glpk.GLP_MSG_OFF,presol:true,tmlim:5});runs.push(performance.now()-t);status=r.result.status;objective=r.result.z;x=names.map(n=>r.result.vars[n]??0);
   }
   check=checkModel(m,x,1e-6);
  }
  const r={runtime:'wasm',mode,fixture:f,solver,ms:median(runs),runs,status,objective,feasible:check.feasible,check,buildMs};records.push(r);console.log(JSON.stringify(r));fs.writeFileSync('/tmp/optio-performance/wasm.json',JSON.stringify(records,null,2));
 }
}
console.log('GLPK version',glpk.version);
