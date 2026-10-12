#!/usr/bin/env node
// Rotate frozen native builds, exclude initial samples, and check each witness.
// node harness/tools/planning-rust-stages.mjs --out /tmp/stages.json \
//   --build baseline=/tmp/before/native --build optimized=target/release/optio-bench
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { resolve } from 'node:path';
import { checkModel } from '../src/checker.mjs';
const args = process.argv.slice(2), builds = [];
let output = '/tmp/optio-rust-stages.json', repeats = 11, warmups = 2;
for (let i = 0; i < args.length; i++) {
  const key = args[i], value = args[++i];
  if (key === '--build') {
    const split = value.indexOf('=');
    if (split < 1) throw new Error('--build requires label=path');
    builds.push({ label: value.slice(0, split), path: resolve(value.slice(split + 1)) });
  } else if (key === '--out') output = value;
  else if (key === '--repeats') repeats = Number(value);
  else if (key === '--warmups') warmups = Number(value);
  else throw new Error(`Unknown option ${key}`);
}
if (!builds.length || !Number.isInteger(repeats) || repeats < 1 || !Number.isInteger(warmups) || warmups < 0) throw new Error('Invalid benchmark options');
const sha = path => createHash('sha256').update(readFileSync(path)).digest('hex');
const median = values => [...values].sort((a, b) => a - b)[Math.floor(values.length / 2)];
for (const build of builds) build.sha256 = sha(build.path);
const records = [], summary = [];
for (const [name, optimum] of [['small', 314.9], ['medium', 3096.8], ['large', 4028.4]]) {
  const file = resolve(`harness/fixtures/planning-app-${name}.model.json`);
  const model = JSON.parse(readFileSync(file, 'utf8')), modelSha256 = sha(file);
  const samples = new Map(builds.map(b => [b.label, []]));
  for (let repeat = 0; repeat < repeats + warmups; repeat++) {
    let order = builds.map((_, i) => builds[(i + repeat) % builds.length]);
    if (repeat % 2) order.reverse();
    for (const build of order) {
      const result = JSON.parse(execFileSync(build.path, ['--model-json', file, '--time', '5', '--gap', '0', '--threads', '1'], { encoding: 'utf8', maxBuffer: 1 << 26 }));
      const check = checkModel(model, result.colValue, 1e-9);
      if (!check.feasible || result.statusCode !== 7 || Math.abs(result.objective - optimum) > 1e-7 || result.dualBound > result.objective || result.objective - result.dualBound > 1e-6 || result.nodes !== 0) throw new Error(`${name}/${build.label}: incorrect result`);
      const initial = repeat < warmups;
      if (!initial) samples.get(build.label).push(result);
      if (repeat !== warmups) delete result.colValue;
      records.push({ name, build: build.label, repeat, initial, modelSha256, result, check });
    }
  }
  for (const build of builds) {
    const runs = samples.get(build.label);
    const row = { name, build: build.label, medianMs: median(runs.map(r => r.ms)), samplesMs: runs.map(r => r.ms), phaseMs: Object.fromEntries(Object.keys(runs[0].timing).map(k => [k, median(runs.map(r => r.timing[k] * 1000))])) };
    summary.push(row);
    console.log(`${name} ${build.label}: ${row.medianMs.toFixed(3)} ms`);
  }
}
writeFileSync(output, JSON.stringify({ repeats, warmups, builds, summary, records }, null, 2) + '\n');
