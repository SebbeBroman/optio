// Reproducible native/WASM Sudoku A/B; independent checker validates every run.
import {readFileSync,writeFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {execFileSync} from 'node:child_process';
import {performance} from 'node:perf_hooks';
import {checkModel} from '../src/checker.mjs';
const require=createRequire(import.meta.url);
const mod=require(process.env.OPTIO_WASM ?? '../../pkg/optio_wasm.js');
const dir=process.argv[2] ?? '/tmp/optio-certification-models';
const out=process.argv[3] ?? 'docs/optimality-2026-10-11/sudoku.json';
const records=[];
for(const name of ['sudoku-easy','sudoku-hard']) {
 const path=`${dir}/${name}.json`,m=JSON.parse(readFileSync(path,'utf8'));
 for(const runtime of ['native-1','native-8','wasm']) for(const presolve of [false,true]) {
  const runs=[];let last;
  for(let rep=0;rep<11;rep++) {
   if(runtime.startsWith('native')) {
    const r=JSON.parse(execFileSync('target/release/optio-bench',['--model-json',path,'--time','120','--gap','0','--threads',runtime.split('-')[1],...(presolve?[]:['--no-presolve'])],{encoding:'utf8'}));
    last=r;
   } else {
    const model=new mod.Model();const a=m.matrix;
    model.add_model(m.numCols,Float64Array.from(m.colCost),Float64Array.from(m.colLower),Float64Array.from(m.colUpper),Int32Array.from(m.integrality),Float64Array.from(m.rowLower),Float64Array.from(m.rowUpper),Uint32Array.from(a.starts),Uint32Array.from(a.indices),Float64Array.from(a.values));
    model.set_option('presolve',presolve?1:0);model.set_time_limit(120);
    const t=performance.now();model.run();const ms=performance.now()-t;
    last={ms,statusCode:model.get_model_status(),colValue:Array.from(model.get_solution_object().colValue),iterations:model.get_info('simplex_iteration_count'),nodes:model.get_info('mip_node_count'),dualBound:model.get_info('mip_dual_bound')};model.free();
   }
   if(!checkModel(m,last.colValue,1e-6).feasible||last.statusCode!==7)throw Error(`failed ${runtime} ${name}`);
   if(rep>1)runs.push(last.ms);
  }
  runs.sort((a,b)=>a-b);const r={name,runtime,presolve,medianMs:runs[Math.floor(runs.length/2)],runs,iterations:last.iterations,nodes:last.nodes,dualBound:last.dualBound,feasible:true};records.push(r);console.log(JSON.stringify(r));
 }
}
writeFileSync(out,JSON.stringify(records,null,2)+'\n');
