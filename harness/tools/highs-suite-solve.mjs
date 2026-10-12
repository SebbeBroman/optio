// One cold HiGHS WASM solve; ingestion/startup excluded from timing.
import {readFileSync} from 'node:fs';
import {performance} from 'node:perf_hooks';
import {loadHighs} from '../src/highs.mjs';
const [file, seconds] = process.argv.slice(2);
const highs = await loadHighs();
const data = JSON.parse(readFileSync(file,'utf8'));
data.matrix.numRows = data.numRows;
data.matrix.numCols = data.numCols;
const model = highs.createModel(data);
model.options.set({output_flag:false, time_limit:Number(seconds), mip_rel_gap:0, mip_abs_gap:0});
const started = performance.now();
model.run();
const ms = performance.now()-started;
const statusCode = model.getModelStatus();
const valid = model.info.get('primal_solution_status') === 2;
const finite = x => Number.isFinite(x) && Math.abs(x)<1e29 ? x : null;
console.log(JSON.stringify({statusCode, ms,
  objective:valid ? finite(model.getObjectiveValue()) : null,
  dualBound:statusCode===7 ? finite(model.getObjectiveValue()) : data.integrality.some(x=>x!==0) ? finite(model.info.get('mip_dual_bound')) : null,
  colValue:valid ? Array.from(model.getSolution().colValue) : [],
  nodes:Number(model.info.get('mip_node_count'))}));
model.dispose();
