// HiGHS WASM Sudoku has objective zero; every feasible point proves optimality.
import {readFileSync,writeFileSync} from 'node:fs';
import {performance} from 'node:perf_hooks';
import {loadHighs} from '../src/highs.mjs';
import {checkModel} from '../src/checker.mjs';
const highs=await loadHighs(),path='docs/optimality-2026-10-11/sudoku-highs.json';
const records=JSON.parse(readFileSync(path,'utf8')).filter(r=>r.runtime!=='wasm');
for(const name of ['sudoku-easy','sudoku-hard']){
 const lp=readFileSync(`harness/fixtures/lp/${name}.lp`,'utf8'),runs=[];
 for(let i=0;i<11;i++){
  const m=highs.createModel({format:'lp',data:lp});m.options.set({output_flag:false,time_limit:120});
  const t=performance.now();m.run();const ms=performance.now()-t;
  const check=checkModel(m.getModel('csr'),m.getSolution().colValue,1e-6);
  if(m.getModelStatus()!==7||!check.feasible||m.getObjectiveValue()!==0)throw Error(name+' failed');
  if(i>1)runs.push(ms);m.dispose();
 }
 runs.sort((a,b)=>a-b);records.push({name,runtime:'wasm',solver:'HiGHS',medianMs:runs[4],runs,feasible:true});
}
writeFileSync(path,JSON.stringify(records,null,2)+'\n');console.log(JSON.stringify(records));
