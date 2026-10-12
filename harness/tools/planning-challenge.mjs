#!/usr/bin/env node
// Measure time to first candidate/quality target and independently check every
// final witness. Solves run sequentially; parsing and ingestion are excluded.
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { resolve } from 'node:path';
import { checkModel } from '../src/checker.mjs';

const options = { repeats: 3, reference: 536748.44, limits: '2,5,20' };
for (let i = 2; i < process.argv.length; i += 2) {
  const key = process.argv[i].replace(/^--/, '');
  if (!['native', 'wasm', 'model', 'out', 'repeats', 'reference', 'limits'].includes(key)) throw Error(`Unknown option ${key}`);
  options[key] = process.argv[i + 1];
}
if (!options.model || !options.out || (!options.native && !options.wasm)) throw Error('--model, --out and --native and/or --wasm required');
const limits = options.limits.split(',').map(Number), repeats = Number(options.repeats);
if (!Number.isInteger(repeats) || repeats < 1 || limits.some(x => !Number.isFinite(x) || x <= 0)) throw Error('Invalid limits/repeats');
const modelFile = resolve(options.model), model = JSON.parse(readFileSync(modelFile, 'utf8'));
const sha = file => createHash('sha256').update(readFileSync(file)).digest('hex');
const threshold = Number(options.reference) * 1.005;
const report = { modelFile, modelSha256: sha(modelFile), referenceObjective: Number(options.reference), goodThreshold: threshold,
  goodDefinition: 'Within 0.5% of the archived best checked objective; not a claim of optimality',
  method: 'Sequential fresh processes, solver timing only; final witnesses independently checked at 1e-9. No warmup exclusion.', builds: {}, records: [] };
for (const runtime of ['native', 'wasm']) {
  if (!options[runtime]) continue;
  const path = resolve(options[runtime]);
  report.builds[runtime] = { path, sha256: sha(runtime === 'native' ? path : path.replace(/\.js$/, '_bg.wasm')) };
  for (const limit of limits) {
    for (let repeat = 0; repeat < repeats; repeat++) {
      const command = runtime === 'native' ? path : 'node';
      const args = runtime === 'native' ? ['--model-json', modelFile, '--time', String(limit), '--gap', '0', '--threads', '1']
        : ['harness/tools/planning-timing.mjs', path, modelFile, String(limit)];
      const result = JSON.parse(execFileSync(command, args, { encoding: 'utf8', maxBuffer: 1 << 27 }));
      const check = result.colValue?.length ? checkModel(model, result.colValue, 1e-9) : null;
      if (result.objective != null && (!check?.feasible || Math.abs(check.objective - result.objective) > 1e-6)) throw Error(`${runtime}/${limit}: invalid witness`);
      if (result.statusCode === 7 && (result.objective == null || result.dualBound == null || result.dualBound > result.objective + 1e-6)) throw Error('Invalid optimal result');
      const first = result.incumbents.find(x => Number.isFinite(x.objective));
      const good = result.incumbents.find(x => x.objective <= threshold);
      const record = { runtime, limit, repeat, firstCandidateMs: first?.ms ?? null, firstGoodMs: good?.ms ?? null,
        relativeGap: result.objective == null || result.dualBound == null ? null : (result.objective - result.dualBound) / Math.max(1, Math.abs(result.objective)), check, result };
      report.records.push(record);
      writeFileSync(options.out, JSON.stringify(report, null, 2) + '\n');
      console.log(`${runtime} ${limit}s #${repeat}: candidate=${record.firstCandidateMs?.toFixed(1)}ms good=${record.firstGoodMs?.toFixed(1)}ms objective=${result.objective} gap=${record.relativeGap} status=${result.statusCode}`);
    }
  }
}
