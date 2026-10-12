// Matched warm WASM solves; Node/V8, not a browser measurement.
import {readFileSync,writeFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {performance} from 'node:perf_hooks';
import {checkModel} from '../src/checker.mjs';
import {fileURLToPath} from 'node:url';
import {resolve} from 'node:path';
const root=fileURLToPath(new URL('../../',import.meta.url)),require=createRequire(import.meta.url);
const [before,after,output]=process.argv.slice(2);
if(!before||!after||!output)throw new Error('Usage: planning-warm-compare.mjs before.js after.js output.json');
const modules={before:require(resolve(before)),after:require(resolve(after))};
const records=[],summary=[],median=a=>[...a].sort((x,y)=>x-y)[Math.floor(a.length/2)];
for(const name of ['small','medium','large']){
 const d=JSON.parse(readFileSync(`${root}/harness/fixtures/planning-app-${name}.model.json`,'utf8'));
 const arrays=[d.numCols,Float64Array.from(d.colCost),Float64Array.from(d.colLower),Float64Array.from(d.colUpper),Int32Array.from(d.integrality),Float64Array.from(d.rowLower),Float64Array.from(d.rowUpper),Uint32Array.from(d.matrix.starts),Uint32Array.from(d.matrix.indices),Float64Array.from(d.matrix.values)];
 const samples={before:[],after:[]};
 for(let repeat=0;repeat<30;repeat++){
  for(const build of repeat%2?['after','before']:['before','after']){
   const m=new modules[build].Model();m.add_model(...arrays);m.set_time_limit(5);m.set_option('mip_rel_gap',0);
   const t0=performance.now();m.run();const ms=performance.now()-t0;
   const x=Array.from(m.get_solution_object().colValue),check=checkModel(d,x,1e-9),objective=m.get_objective_value(),bound=m.get_info('mip_dual_bound');
   if(!check.feasible||m.get_model_status()!==7||bound>objective||objective-bound>1e-6)throw Error(`${name}/${build} failed`);
   records.push({name,build,repeat,initial:repeat<15,ms,objective,bound,check});if(repeat>=15)samples[build].push(ms);m.free();
  }
 }
 const r={name,medianMs:Object.fromEntries(Object.entries(samples).map(([k,a])=>[k,median(a)])),samplesMs:samples};summary.push(r);console.log(JSON.stringify(r));
}
writeFileSync(output,JSON.stringify({warmups:15,repeats:15,summary,records},null,2)+'\n');
