// Anonymize a planning LP and derive variants.
//
// - Loads the source LP with highs-js.
// - Optionally mutates session-cap row bounds (identified by row name).
// - Renames every column to `c#` and every row to `r#` (drops all IDs).
// - Re-exports LP into ../fixtures/<name>.lp
//
// Usage: node tools/build-fixtures.mjs "/path/to/planning-model.lp" [seconds]
import { readFileSync, writeFileSync } from 'node:fs';
import { loadHighs } from '../src/highs.mjs';
import { checkModel, statusName } from '../src/checker.mjs';

const src = process.argv[2];
if (!src) throw new Error('usage: node tools/build-fixtures.mjs <source.lp> [seconds]');
const seconds = Number(process.argv[3] ?? 60);
const outDir = new URL('../fixtures/', import.meta.url);
const lp = readFileSync(src, 'utf8');
const highs = await loadHighs();

function loadModel() {
  return highs.createModel({ format: 'lp', data: lp });
}

// Which rows are session caps? hard: `cap_*`, soft: `softcap_*`.
const isCap = (name) => name.startsWith('softcap_') || name.startsWith('cap_');

async function build(name, capFactor) {
  const model = loadModel();
  try {
    const before = model.getModel('csr');
    const n = before.numCols;
    const m = before.numRows;

    let caps = 0;
    if (capFactor !== 1) {
      for (let j = 0; j < m; j++) {
        const rn = model.getRowName(j);
        if (!isCap(rn)) continue;
        const hi = before.rowUpper[j];
        if (!Number.isFinite(hi) || hi >= 1e30) continue;
        const next = capFactor === Infinity ? 1e30 : hi * capFactor;
        model.changeRowBounds(j, before.rowLower[j], next);
        caps += 1;
      }
    }

    for (let i = 0; i < n; i++) model.passColName(i, `c${i}`);
    for (let j = 0; j < m; j++) model.passRowName(j, `r${j}`);

    const text = model.exportModel('lp');
    writeFileSync(new URL(`${name}.lp`, outDir), text);
    console.log(
      `${name.padEnd(18)} cols=${n} rows=${m} nnz=${before.matrix.values.length} caps=${caps} ${(text.length / 1024).toFixed(0)} KB`,
    );
  } finally {
    model.dispose?.();
  }
}

// Baseline stats + solve status (informational).
{
  const model = loadModel();
  try {
    const d = model.getModel('csr');
    const ints = Array.from(d.integrality).filter((v) => v !== 0).length;
    model.options.set({ output_flag: false, time_limit: seconds });
    model.run();
    const status = statusName(model.getModelStatus());
    let obj = model.getObjectiveValue();
    let feasible = null;
    try {
      const check = checkModel(d, model.getSolution().colValue, 1e-6);
      feasible = check.feasible;
    } catch {}
    console.log(
      `source: cols=${d.numCols} rows=${d.numRows} nnz=${d.matrix.values.length} integer=${ints} status=${status} obj=${obj} feas=${feasible}`,
    );
  } finally {
    model.dispose?.();
  }
}

// Exports from the app's infeasible/IIS branch can still be feasible.
// Preserve them under a neutral name; native solvers determine their status.
await build('planning-export', 1);
