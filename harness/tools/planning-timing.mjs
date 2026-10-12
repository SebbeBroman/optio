// One independently checked WASM solve, matching native bench JSON.
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {performance} from 'node:perf_hooks';
import {checkModel} from '../src/checker.mjs';
const [pkg,modelFile,limit,...flags]=process.argv.slice(2);
const mod=createRequire(import.meta.url)(pkg),m=JSON.parse(readFileSync(modelFile,'utf8'));
const s=new mod.Model(),a=m.matrix,sign=m.sense??1,offset=m.offset??0;
s.add_model(m.numCols,Float64Array.from(m.colCost,c=>sign*c),Float64Array.from(m.colLower),Float64Array.from(m.colUpper),Int32Array.from(m.integrality),Float64Array.from(m.rowLower),Float64Array.from(m.rowUpper),Uint32Array.from(a.starts),Uint32Array.from(a.indices),Float64Array.from(a.values));
s.set_time_limit(Number(limit));s.set_option('mip_rel_gap',0);if(flags.includes('--adaptive'))s.set_option('adaptive_search',1);
for(const [flag,option]of[['--cold-nodes','lp_warm_start'],['--no-cuts','mip_cuts'],['--no-pseudocost','mip_pseudocost'],['--no-presolve','presolve'],['--no-node-presolve','mip_node_presolve'],['--no-column-compression','mip_column_compression'],['--no-objective-counts','mip_objective_counts'],['--no-objective-lattice','mip_objective_lattice'],['--no-strong-branch','mip_strong_branch'],['--no-adaptive','adaptive_search']])if(flags.includes(flag))s.set_option(option,0);
if(flags.includes('--column-compression')&&!flags.includes('--no-column-compression'))s.set_option('mip_column_compression',1);
const startFlag=flags.indexOf('--start');
if(startFlag>=0){const start=JSON.parse(readFileSync(flags[startFlag+1],'utf8')),dense=new Float64Array(m.numCols);if(Array.isArray(start))dense.set(start);else for(let k=0;k<start.indices.length;k++)dense[start.indices[k]]=start.values[k];s.set_solution(dense);}
const incumbents=[],started=performance.now();
s.run_with_callbacks({mipImprovingSolution:inc=>incumbents.push({ms:performance.now()-started,objective:sign*inc.objective+offset})});
const ms=performance.now()-started,x=Array.from(s.get_solution_object().colValue),check=x.length?checkModel(m,x,1e-6):null;
const info=k=>s.get_info(k);
const r={statusCode:s.get_model_status(),objective:check?.feasible?sign*s.get_objective_value()+offset:null,dualBound:sign*info('mip_dual_bound')+offset,ms,nodes:info('mip_node_count'),iterations:info('simplex_iteration_count'),check,colValue:x,incumbents,timing:{presolve:info('time_presolve_secs'),heuristic:info('time_heuristic_secs'),localSearch:info('time_local_search_secs'),cuts:info('time_root_cuts_secs'),boundCertificate:info('time_bound_certificate_secs'),branchAndBound:info('time_branch_and_bound_secs'),nodeLpWorkerSeconds:info('time_node_lp_worker_secs')},lpStats:{solves:info('node_lp_solves'),phase1Iterations:info('node_lp_phase1_iterations'),phase2Iterations:info('node_lp_phase2_iterations'),warmSolves:info('node_lp_warm_solves'),warmFallbacks:info('node_lp_warm_fallbacks')},rootCuts:info('mip_root_cuts'),inferredIntegerCols:info('mip_inferred_integer_cols')};
for(const field of ['invalid_basis','singular_basis','residual_failures','dual_failures','no_pivot','pivot_failures','iteration_limits','bound_failures','bound_flips']) {
  const key='warm'+field.split('_').map(x=>x[0].toUpperCase()+x.slice(1)).join('');
  r.lpStats[key]=info('node_lp_warm_'+field);
}
r.mipStats={compressedColumns:info('compressed_columns'),objectiveCountColumns:info('objective_count_columns'),propagationPrunes:info('node_propagation_prunes'),propagationSeconds:info('time_node_propagation_secs'),strongBranchProbes:info('strong_branch_probes'),numericalRetries:info('numerical_retries')};
s.free();console.log(JSON.stringify(r));
