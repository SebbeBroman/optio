// Generate classic MILP encodings as `.lp` fixtures:
//   - Sudoku  : 729 binaries (value-per-cell) + 324 equality rows + givens.
//               A pure feasibility 0-1 ILP.
//   - TSP     : symmetric Euclidean TSP as a degree-2 assignment + MTZ order
//               variables (binary arcs + continuous `u`), minimising tour length.
//
// Deterministic (fixed seeds). Merges the instances into the shared
// `fixtures/lp/manifest.json` so `src/native.mjs` picks them up unchanged.
//
// Usage: node tools/gen-classics.mjs
import { readFileSync, writeFileSync, existsSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { loadHighs } from '../src/highs.mjs';
import { statusName } from '../src/checker.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const fixturesDir = join(here, '..', 'fixtures', 'lp');
const manifestPath = join(fixturesDir, 'manifest.json');
mkdirSync(fixturesDir, { recursive: true });

const pad = (s, n) => String(s).padEnd(n);
/** FNV-ish deterministic RNG. */
function rng(seed) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 4294967296;
  };
}

// ---------------------------------------------------------------------------
// Sudoku
// ---------------------------------------------------------------------------
const SUDOKU = [
  {
    name: 'sudoku-easy',
    givens: '530070000600195000098000060800060003400803001700020006060000280000419005000080079',
  },
  {
    name: 'sudoku-hard',
    givens: '000000010400000000020000000000050407008000300001090000300400200050100000000806000',
  },
];

function sudokuLp(givens) {
  const g = givens.replace(/\D/g, '');
  const varName = (r, c, d) => `v${r}_${c}_${d}`;
  const terms = (list) => list.map((t) => `+1 ${t}`).join(' ');
  const box = (r, c) => 3 * Math.floor(r / 3) + Math.floor(c / 3);

  const lines = ['min', ` obj: +0 ${varName(0, 0, 1)}`, 'st'];
  // one value per cell
  for (let r = 0; r < 9; r++)
    for (let c = 0; c < 9; c++) {
      const ts = [];
      for (let d = 1; d <= 9; d++) ts.push(varName(r, c, d));
      lines.push(` cell_${r}_${c}: ${terms(ts)} = 1`);
    }
  // each value once per row / column / box
  for (let d = 1; d <= 9; d++) {
    for (let r = 0; r < 9; r++) {
      const ts = [];
      for (let c = 0; c < 9; c++) ts.push(varName(r, c, d));
      lines.push(` row_${r}_${d}: ${terms(ts)} = 1`);
    }
    for (let c = 0; c < 9; c++) {
      const ts = [];
      for (let r = 0; r < 9; r++) ts.push(varName(r, c, d));
      lines.push(` col_${c}_${d}: ${terms(ts)} = 1`);
    }
    for (let b = 0; b < 9; b++) {
      const ts = [];
      for (let r = 0; r < 9; r++)
        for (let c = 0; c < 9; c++) if (box(r, c) === b) ts.push(varName(r, c, d));
      lines.push(` box_${b}_${d}: ${terms(ts)} = 1`);
    }
  }
  lines.push('bounds');
  const fixed = new Set();
  for (let r = 0; r < 9; r++)
    for (let c = 0; c < 9; c++) {
      const v = Number(g[r * 9 + c] || 0);
      if (v >= 1 && v <= 9) fixed.add(varName(r, c, v));
    }
  for (let r = 0; r < 9; r++)
    for (let c = 0; c < 9; c++)
      for (let d = 1; d <= 9; d++) {
        const nm = varName(r, c, d);
        lines.push(fixed.has(nm) ? ` ${nm} = 1` : ` 0 <= ${nm} <= 1`);
      }
  lines.push('bin');
  for (let r = 0; r < 9; r++)
    for (let c = 0; c < 9; c++) for (let d = 1; d <= 9; d++) lines.push(` ${varName(r, c, d)}`);
  lines.push('end');
  return { text: lines.join('\n'), cols: 729, rows: 324, nnz: 2916, name: 'sudoku' };
}

// ---------------------------------------------------------------------------
// TSP (symmetric, DFJ subtour-elimination constraints enumerated for small n)
// ---------------------------------------------------------------------------
function tspInstance(name, n, seed) {
  const r = rng(seed);
  const xs = [], ys = [];
  for (let i = 0; i < n; i++) {
    xs.push(Math.round(r() * 100));
    ys.push(Math.round(r() * 100));
  }
  const dist = (i, j) => Math.round(Math.hypot(xs[i] - xs[j], ys[i] - ys[j]));
  const arc = (i, j) => (i < j ? `x${i}_${j}` : `x${j}_${i}`);

  const lines = ['min'];
  const obj = [];
  for (let i = 0; i < n; i++)
    for (let j = i + 1; j < n; j++) obj.push(`+${dist(i, j)} ${arc(i, j)}`);
  lines.push(' obj: ' + obj.join(' '));
  lines.push('st');
  // Degree constraints: each city has degree exactly 2.
  for (let i = 0; i < n; i++) {
    const ts = [];
    for (let j = 0; j < n; j++) if (i !== j) ts.push(`+1 ${arc(i, j)}`);
    lines.push(` deg_${i}: ${ts.join(' ')} = 2`);
  }
  // Subtour elimination: for every S with 0∈S, 2<=|S|<=n-1, at least two edges
  // cross the cut.
  for (let mask = 1; mask < (1 << n); mask++) {
    if (!(mask & 1)) continue; // require node 0 in S
    const members = [];
    for (let i = 0; i < n; i++) if (mask & (1 << i)) members.push(i);
    if (members.length < 2 || members.length > n - 1) continue;
    const inS = new Set(members);
    const ts = [];
    for (const i of members)
      for (let j = 0; j < n; j++) if (!inS.has(j)) ts.push(`+1 ${arc(i, j)}`);
    lines.push(` sec_${members.join('_')}: ${ts.join(' ')} >= 2`);
  }
  lines.push('bounds');
  for (let i = 0; i < n; i++)
    for (let j = i + 1; j < n; j++) lines.push(` 0 <= ${arc(i, j)} <= 1`);
  lines.push('bin');
  for (let i = 0; i < n; i++) for (let j = i + 1; j < n; j++) lines.push(` ${arc(i, j)}`);
  lines.push('end');

  const ncols = (n * (n - 1)) / 2;
  let nrows = n;
  let nnz = 0; // deg rows
  for (let i = 0; i < n; i++) nnz += n - 1;
  for (let mask = 1; mask < (1 << n); mask++) {
    if (!(mask & 1)) continue;
    const k = popcount(mask);
    if (k < 2 || k > n - 1) continue;
    nrows += 1;
    nnz += k * (n - k);
  }
  return { text: lines.join('\n'), cols: ncols, rows: nrows, nnz, name };
}

function popcount(x) {
  let c = 0;
  while (x) { c += x & 1; x >>= 1; }
  return c;
}

// ---------------------------------------------------------------------------
// Write + derive reference from HiGHS
// ---------------------------------------------------------------------------
const highs = await loadHighs();

function reference(text, seconds = 30) {
  const m = highs.createModel({ format: 'lp', data: text });
  m.options.set({ output_flag: false, time_limit: seconds });
  m.run();
  const status = statusName(m.getModelStatus());
  const objective = m.getObjectiveValue();
  m.dispose?.();
  const expect = /Infeasible/i.test(status) ? 'infeasible' : /Unbounded/i.test(status) ? 'unbounded' : 'feasible';
  return { status, objective, expect };
}

const generated = [];
for (const s of SUDOKU) {
  const lp = sudokuLp(s.givens);
  const file = `${s.name}.lp`;
  writeFileSync(join(fixturesDir, file), lp.text);
  const ref = reference(lp.text, 30);
  generated.push({ name: s.name, file, kind: 'mip', seconds: 20, expect: ref.expect,
    cols: lp.cols, rows: lp.rows, nnz: lp.nnz, highsStatus: ref.status, highsObjective: ref.objective });
  console.log(pad(s.name, 16), ref.status, ref.objective);
}
for (const [name, n, seed] of [['tsp-8', 8, 11], ['tsp-10', 10, 22], ['tsp-12', 12, 33]]) {
  const lp = tspInstance(name, n, seed);
  const file = `${name}.mip.lp`;
  writeFileSync(join(fixturesDir, file), lp.text);
  const ref = reference(lp.text, 30);
  generated.push({ name, file, kind: 'mip', seconds: 20, expect: ref.expect,
    cols: lp.cols, rows: lp.rows, nnz: lp.nnz, highsStatus: ref.status, highsObjective: ref.objective });
  console.log(pad(name, 16), ref.status, ref.objective);
}

// Merge into the manifest, preserving non-classic entries.
let manifest = { generatedBy: 'tools/gen-lp.mjs', repo: process.cwd(), instances: [] };
if (existsSync(manifestPath)) {
  try { manifest = JSON.parse(readFileSync(manifestPath, 'utf8')); } catch {}
}
const names = new Set(generated.map((g) => g.name));
manifest.instances = (manifest.instances || []).filter((i) => !names.has(i.name)).concat(generated);
manifest.classicsBy = 'tools/gen-classics.mjs';
writeFileSync(manifestPath, JSON.stringify(manifest, null, 2));
console.log(`\nwrote ${generated.length} classics; manifest now has ${manifest.instances.length} instances`);
