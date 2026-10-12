// Self-contained repro for the "losses" in ../../LOSSES.md: run the *real*
// planning models through optio and compare against native HiGHS,
// inside the optio repo (no external checkout needed).
//
// Fixtures (committed):
//   planning-app-<s>.lp           the model, for the native-HiGHS reference
//   planning-app-<s>.model.json   the same model as detached CSR, for optio
//   planning-app-<s>.start.json   the sparse greedy MIP start the app hands over
// They were exported once from the planning app.
//
// The reference is **native HiGHS** (highspy), not the in-repo highs-js wasm —
// that build mis-solves MIPs (see native-oracle.py). Set it up once with:
//   bash harness/tools/setup-oracle.sh
//
// Usage:
//   node tools/planning-app-loss.mjs            # seeded optio, native reference
//   NO_SEED=1 node tools/planning-app-loss.mjs  # drop the seed (shows the L2 loss)
//   TIME_LIMIT=30 node tools/planning-app-loss.mjs
import { readFileSync, writeFileSync, existsSync, mkdtempSync, rmSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, basename, join } from 'node:path';
import { tmpdir } from 'node:os';
import { performance } from 'node:perf_hooks';

import { solveWithOptio } from '../src/optio.mjs';
import { checkModel, statusName } from '../src/checker.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const harnessDir = join(here, '..');
const fixturesDir = join(harnessDir, 'fixtures');
const venvPython = join(here, 'oracle-venv', 'bin', 'python');

const SCENARIOS = [
  { name: 'small', seconds: 5 },
  { name: 'medium', seconds: 10 },
  { name: 'large', seconds: 20 },
];

const TIME_LIMIT = Number(process.env.TIME_LIMIT ?? 0);
const USE_SEED = process.env.NO_SEED !== '1';

const pad = (s, n) => String(s).padEnd(n);
const padL = (s, n) => String(s).padStart(n);
const num = (v, d = 2) => (Number.isFinite(v) ? v.toFixed(d) : 'n/a');
const secs = (ms) => (ms == null ? '—' : `${(ms / 1000).toFixed(2)}s`);

function toDense(sparse, numCols) {
  const dense = new Float64Array(numCols);
  const idx = sparse?.indices ?? [];
  const val = sparse?.values ?? [];
  for (let k = 0; k < idx.length; k++) dense[idx[k]] = val[k];
  return dense;
}

/** Native-HiGHS reference for every scenario's LP, via the existing oracle. */
function nativeReferences(scenarios) {
  if (!existsSync(venvPython)) {
    console.error('native HiGHS venv missing — run `bash harness/tools/setup-oracle.sh` first.');
    process.exit(2);
  }
  const dir = mkdtempSync(join(tmpdir(), 'planning-app-ref-'));
  const out = join(dir, 'ref.json');
  try {
    const records=[];
    for(const scenario of scenarios){
      execFileSync(venvPython,[join(here,'native-oracle.py'),join(fixturesDir,`planning-app-${scenario.name}.lp`),'--ref-only','--time',String(TIME_LIMIT||scenario.seconds),'--ref-gap','0','--json-out',out],{stdio:'ignore'});
      records.push(...JSON.parse(readFileSync(out,'utf8')));
    }
    const byName = new Map(records.map((r) => [basename(r.file, '.lp'), r.reference]));
    return { byName, dir };
  } catch (err) {
    rmSync(dir, { recursive: true, force: true });
    throw err;
  }
}

async function main() {
  console.log('reference: native HiGHS (highspy)');
  console.log(`seed: ${USE_SEED ? 'greedy MIP start from the app' : 'none (NO_SEED=1)'}\n`);

  const { byName, dir } = nativeReferences(SCENARIOS);
  try {
    console.log(
      pad('scenario', 10) + pad('native HiGHS', 15) + pad('optio', 14) +
      padL('optio obj', 12) + padL('first-inc', 11) + padL(' feas', 6) + padL('gap', 9),
    );
    console.log('-'.repeat(78));

    const records=[];
    for (const scenario of SCENARIOS) {
      const modelPath = join(fixturesDir, `planning-app-${scenario.name}.model.json`);
      if (!existsSync(modelPath)) {
        console.log(pad(scenario.name, 10) + 'MISSING ' + modelPath);
        continue;
      }
      const model = JSON.parse(readFileSync(modelPath, 'utf8'));
      const startPath = join(fixturesDir, `planning-app-${scenario.name}.start.json`);
      const seed =
        USE_SEED && existsSync(startPath)
          ? toDense(JSON.parse(readFileSync(startPath, 'utf8')), model.numCols)
          : null;
      const seconds = TIME_LIMIT > 0 ? TIME_LIMIT : scenario.seconds;

      const t0 = performance.now();
      const result = await solveWithOptio(model, {
        seconds,
        warmStart: seed ?? undefined,
        onIncumbent: () => {},
      });
      const oMs = performance.now() - t0;
      const check =
        result.colValue && result.colValue.length >= model.numCols
          ? checkModel(model, result.colValue, 1e-6)
          : null;
      const status = statusName(result.status);

      const ref = byName.get(`planning-app-${scenario.name}`);
      const refObj = ref?.objective ?? null;
      const gap =
        Number.isFinite(refObj) && refObj !== 0 && result.colValue
          ? (result.objective - refObj) / Math.abs(refObj)
          : null;

      records.push({scenario:scenario.name,seconds,seeded:USE_SEED,reference:ref,optio:{...result,colValue:undefined},check,gap});
      console.log(
        pad(scenario.name, 10) +
        pad(num(refObj), 15) +
        pad(status, 14) +
        padL(num(result.objective), 12) +
        padL(secs(result.firstIncumbentMs), 11) +
        padL(check ? (check.feasible ? 'yes' : 'NO') : 'n/a', 6) +
        padL(gap == null ? 'n/a' : `${(gap * 100).toFixed(1)}%`, 9) +
        `  (${num(oMs, 0)}ms, ${result.incumbents ?? 0} incumbents)`,
      );
    }
    if(process.env.JSON_OUT)writeFileSync(process.env.JSON_OUT,JSON.stringify(records,null,2)+'\n');
    console.log(`\n${records.filter(r=>r.check?.feasible).length}/${records.length} checked feasible; ${records.filter(r=>r.optio.status===7).length}/${records.length} Optio proofs.`);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }


}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
