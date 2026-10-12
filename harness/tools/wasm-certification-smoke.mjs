// All 44 seeded/classic fixtures against their native-checked reference objectives.
import {readFileSync,writeFileSync,existsSync} from 'node:fs';
import {createRequire} from 'node:module';
import {performance} from 'node:perf_hooks';
import {checkModel} from '../src/checker.mjs';
const require=createRequire(import.meta.url);
const mod=require(process.env.OPTIO_WASM ?? '../../pkg/optio_wasm.js');
const dir=process.argv[2] ?? '/tmp/optio-certification-models';
const fixtures=JSON.parse(readFileSync('harness/fixtures/lp/manifest.json','utf8')).instances;
const records=[];
for(const f of fixtures){
 const modelFile=`${dir}/${f.file.slice(0,-3)}.json`;
 const m=JSON.parse(readFileSync(existsSync(modelFile)?modelFile:`${dir}/${f.name}.json`,'utf8')),s=new mod.Model(),a=m.matrix;
 s.add_model(m.numCols,Float64Array.from(m.colCost,c=>m.sense*c),Float64Array.from(m.colLower),Float64Array.from(m.colUpper),Int32Array.from(m.integrality),Float64Array.from(m.rowLower),Float64Array.from(m.rowUpper),Uint32Array.from(a.starts),Uint32Array.from(a.indices),Float64Array.from(a.values));
 s.set_time_limit(120);s.set_option('mip_rel_gap',0);
 const t=performance.now();s.run();const ms=performance.now()-t;
 const status=s.get_model_status(),x=Array.from(s.get_solution_object().colValue),check=x.length?checkModel(m,x,1e-6):null;
 const objective=m.sense*s.get_objective_value()+(m.offset??0);
 const pass=f.expect==='feasible'?Boolean(check?.feasible&&Math.abs(objective-f.highsObjective)<=1e-6*Math.max(1,Math.abs(f.highsObjective))&&[7,16].includes(status)):f.expect==='infeasible'?[8,9].includes(status):[9,10].includes(status);
 records.push({name:f.name,status,objective:x.length?objective:null,bound:s.get_info('mip_dual_bound'),ms,pass,check});s.free();
}
writeFileSync(process.env.OPTIO_REGRESSION_OUT??'docs/optimality-2026-10-11/wasm-regressions.json',JSON.stringify(records,null,2)+'\n');
console.log(`${records.filter(r=>r.pass).length}/${records.length} pass`);
for(const r of records.filter(r=>!r.pass))console.log(JSON.stringify(r));
if(records.some(r=>!r.pass))process.exitCode=1;
