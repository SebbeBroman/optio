// Exercise the browser ES-module API and independently check seeded/unseeded plans.
import assert from 'node:assert/strict';
import {readFileSync, writeFileSync} from 'node:fs';
import {resolve} from 'node:path';
import {checkModel} from '../src/checker.mjs';
const [directory='pkg-web', output='docs/planning-speed-2026-10-11/web-smoke.json']=process.argv.slice(2);
const source=readFileSync(resolve(directory,'optio_wasm.js'),'utf8');
const mod=await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
await mod.default({module_or_path:readFileSync(resolve(directory,'optio_wasm_bg.wasm'))});
const records=[];
for(const [name, optimum] of [['medium',3096.8],['large',4028.4]]) {
  const model=JSON.parse(readFileSync(`harness/fixtures/planning-app-${name}.model.json`,'utf8'));
  for(const seeded of [false,true]) {
    const solver=new mod.Model(), a=model.matrix;
    solver.add_model(model.numCols,Float64Array.from(model.colCost),Float64Array.from(model.colLower),Float64Array.from(model.colUpper),Int32Array.from(model.integrality),Float64Array.from(model.rowLower),Float64Array.from(model.rowUpper),Uint32Array.from(a.starts),Uint32Array.from(a.indices),Float64Array.from(a.values));
    solver.set_time_limit(2);solver.set_option('mip_rel_gap',0);solver.set_option('mip_abs_gap',0);
    if(seeded) {
      const start=JSON.parse(readFileSync(`harness/fixtures/planning-app-${name}.start.json`,'utf8')), dense=new Float64Array(model.numCols);
      for(let i=0;i<start.indices.length;i++)dense[start.indices[i]]=start.values[i];
      solver.set_solution(dense);
    }
    const incumbents=[];
    solver.run_with_callbacks({mipImprovingSolution:inc=>incumbents.push(inc.objective)});
    const point=Array.from(solver.get_solution_object().colValue), check=checkModel(model,point,1e-9);
    const objective=solver.get_objective_value(), bound=solver.get_info('mip_dual_bound');
    assert.equal(solver.get_model_status(),7);assert(check.feasible);
    assert(Math.abs(objective-optimum)<1e-8);assert(bound<=objective&&objective-bound<1e-6);
    assert.equal(incumbents.length,seeded?2:1);assert(Math.abs(incumbents.at(-1)-optimum)<1e-8);
    records.push({name,seeded,status:7,objective,bound,incumbents,check,colValue:point});
    solver.free();
  }
}
// Count-variable reformulation must stay internal to the original JS model.
const n=32, counted={numCols:n,numRows:1,sense:1,offset:0,colCost:Array(n).fill(-3),colLower:Array(n).fill(0),colUpper:Array(n).fill(1),integrality:Array(n).fill(1),rowLower:[-1e30],rowUpper:[2],matrix:{starts:[0,n],indices:Array.from({length:n},(_,j)=>j),values:Array(n).fill(1)}};
for(const seeded of [false,true]) {
  const solver=new mod.Model(), a=counted.matrix;
  solver.add_model(n,Float64Array.from(counted.colCost),Float64Array.from(counted.colLower),Float64Array.from(counted.colUpper),Int32Array.from(counted.integrality),Float64Array.from(counted.rowLower),Float64Array.from(counted.rowUpper),Uint32Array.from(a.starts),Uint32Array.from(a.indices),Float64Array.from(a.values));
  solver.set_time_limit(1);solver.set_option('mip_rel_gap',0);solver.set_option('mip_abs_gap',0);
  if(seeded){const start=new Float64Array(n);start[0]=1;start[1]=1;solver.set_solution(start);}
  const incumbents=[];
  solver.run_with_callbacks({mipImprovingSolution:inc=>{assert.equal(inc.colValue.length,n);assert(checkModel(counted,Array.from(inc.colValue),1e-9).feasible);incumbents.push(inc.objective);}});
  const point=Array.from(solver.get_solution_object().colValue),check=checkModel(counted,point,1e-9),objective=solver.get_objective_value(),bound=solver.get_info('mip_dual_bound');
  assert.equal(solver.get_model_status(),7);assert.equal(point.length,n);assert(check.feasible);assert(Math.abs(objective+6)<1e-9);assert(bound<=objective&&objective-bound<1e-9);assert.equal(solver.get_info('objective_count_columns'),1);assert(Math.abs(incumbents.at(-1)+6)<1e-9);
  records.push({name:'objective-counts',seeded,status:7,objective,bound,incumbents,check,colValue:point});solver.free();
}
writeFileSync(output,JSON.stringify(records,null,2)+'\n');
console.log(`${records.length}/6 web-module checks passed`);
