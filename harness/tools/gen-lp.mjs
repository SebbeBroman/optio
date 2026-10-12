// Deterministic, seeded generator for the differential LP/MIP oracle suite.
//
// Writes ~24 small continuous LPs (`fixtures/lp/*.lp`) and ~15 small mixed-integer
// LPs (`fixtures/lp/*.mip.lp`), plus `fixtures/lp/manifest.json` recording per
// file: { file, kind, seconds, expect }.  `expect` is derived by actually running
// HiGHS (the oracle), never by the generator's intent.
//
// Feasibility is guaranteed by construction for the "feasible" families: a random
// witness assignment is chosen first and every row is built to be satisfied by it
// (equalities get rhs == A x*, inequalities get slack on the satisfied side).
//
// Boundedness: every variable with an infinite bound is given zero objective cost,
// and all other variables are finitely bounded.  Hence `c . d == 0` for every
// recession direction d, so the objective is bounded below (min) and an optimum
// exists.  MIP instances use finite bounds on every column.
//
// Determinism: every random draw comes from a mulberry32 stream seeded per
// instance; re-running produces byte-identical `.lp` files.
//
// Usage: node tools/gen-lp.mjs   (writes the fixture set + manifest)
import { writeFileSync, mkdirSync, readdirSync, unlinkSync, existsSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';

import { loadHighs } from '../src/highs.mjs';
import { checkModel, statusName } from '../src/checker.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const fixturesDir = join(here, '..', 'fixtures', 'lp');
const REPO_ROOT = join(here, '..', '..');

// ---------------------------------------------------------------------------
// Seeded RNG
// ---------------------------------------------------------------------------
function mulberry32(a) {
  return function () {
    a |= 0; a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function makeRng(seed) {
  const next = mulberry32(seed >>> 0);
  const int = (lo, hi) => lo + Math.floor(next() * (hi - lo + 1));
  const chance = (p) => next() < p;
  const nonzero = (max) => int(1, max) * (chance(0.5) ? 1 : -1);
  const shuffle = (arr) => {
    for (let i = arr.length - 1; i > 0; i--) {
      const j = Math.floor(next() * (i + 1));
      [arr[i], arr[j]] = [arr[j], arr[i]];
    }
    return arr;
  };
  return { next, int, chance, nonzero, shuffle };
}

// ---------------------------------------------------------------------------
// LP text rendering (CPLEX LP format, as parsed by highs-js)
// ---------------------------------------------------------------------------
const fmt = (x) => {
  const v = Object.is(x, -0) ? 0 : x;
  return Number.isInteger(v) ? String(v) : String(Number(v.toFixed(6)));
};
const term = (c, name) => `${c >= 0 ? '+' : ''}${fmt(c)} ${name}`;

function boundLine(name, lo, hi) {
  const loInf = lo === -Infinity;
  const hiInf = hi === Infinity;
  if (loInf && hiInf) return ` ${name} free`;
  if (loInf) return ` ${name} <= ${fmt(hi)}`;
  if (hiInf) return ` ${name} >= ${fmt(lo)}`;
  return ` ${fmt(lo)} <= ${name} <= ${fmt(hi)}`;
}

function rowLine(row, names) {
  const expr = row.terms.map(([c, j]) => term(c, names[j])).join(' ');
  if (Number.isFinite(row.lo) && Number.isFinite(row.hi)) {
    if (row.lo === row.hi) return ` ${row.name}: ${expr} = ${fmt(row.lo)}`;
    // Not used by the generator's row families, but keep it total.
    return ` ${row.name}: ${expr} >= ${fmt(row.lo)}`;
  }
  if (Number.isFinite(row.hi)) return ` ${row.name}: ${expr} <= ${fmt(row.hi)}`;
  return ` ${row.name}: ${expr} >= ${fmt(row.lo)}`;
}

/** Render a { cols, rows } model to CPLEX LP text. */
function renderModel(model) {
  const names = model.cols.map((_, j) => `x${j}`);
  const objTerms = model.cols
    .map((c, j) => [c.cost, names[j]])
    .filter(([c]) => c !== 0)
    .map(([c, n]) => term(c, n));
  if (objTerms.length === 0) objTerms.push(term(1, names[0] ?? 'x0'));

  const lines = ['min', ` obj: ${objTerms.join(' ')}`, 'st', ...model.rows.map((r) => rowLine(r, names)), 'bounds'];

  const gens = [];
  const bins = [];
  for (let j = 0; j < model.cols.length; j++) {
    const c = model.cols[j];
    if (c.type === 'bin') {
      bins.push(names[j]);
      continue;
    }
    lines.push(boundLine(names[j], c.lo, c.hi));
    if (c.type === 'int') gens.push(names[j]);
  }

  if (gens.length) lines.push('gen', ...gens.map((g) => ` ${g}`));
  if (bins.length) lines.push('bin', ...bins.map((b) => ` ${b}`));
  lines.push('end', '');
  return lines.join('\n');
}

// ---------------------------------------------------------------------------
// Model builders
// ---------------------------------------------------------------------------
function randomWitness(col, rng) {
  if (col.type === 'bin') return rng.int(0, 1);
  const { lo, hi } = col;
  const loInf = lo === -Infinity;
  const hiInf = hi === Infinity;
  if (!loInf && !hiInf) return rng.int(Math.ceil(lo), Math.floor(hi));
  if (loInf && !hiInf) return Math.floor(hi) - rng.int(0, 3);
  if (!loInf && hiInf) return Math.ceil(lo) + rng.int(0, 3);
  return rng.int(-5, 5);
}

function genRows(rng, n, m, witness, opts) {
  const { eqProb, maxNz, coefMax } = opts;
  const rows = [];
  const pool = Array.from({ length: n }, (_, j) => j);
  for (let r = 0; r < m; r++) {
    const nnz = rng.int(1, Math.min(maxNz, n));
    const idxs = rng.shuffle(pool.slice()).slice(0, nnz);
    const terms = idxs.map((j) => [rng.nonzero(coefMax), j]);
    const activity = terms.reduce((s, [c, j]) => s + c * witness[j], 0);
    const name = `c${r}`;
    if (rng.chance(eqProb)) {
      rows.push({ name, terms, lo: activity, hi: activity });
    } else if (rng.chance(0.5)) {
      rows.push({ name, terms, lo: -Infinity, hi: activity + rng.int(0, 6) });
    } else {
      rows.push({ name, terms, lo: activity - rng.int(0, 6), hi: Infinity });
    }
  }
  return rows;
}

/** A guaranteed-feasible, bounded continuous LP. */
function genFeasibleLP(seed, cfg) {
  const rng = makeRng(seed);
  const n = rng.int(cfg.minCols, cfg.maxCols);
  const m = rng.int(cfg.minRows, cfg.maxRows);

  const cols = [];
  for (let j = 0; j < n; j++) {
    // Column 0 is always finite so there is somewhere to put a nonzero cost.
    const infinite = j > 0 && rng.chance(cfg.infiniteProb);
    let lo;
    let hi;
    if (!infinite) {
      lo = -rng.int(0, 8);
      hi = lo + rng.int(1, 15);
    } else {
      const kind = rng.int(0, 2);
      if (kind === 0) { lo = -Infinity; hi = Infinity; }
      else if (kind === 1) { lo = -rng.int(0, 8); hi = Infinity; }
      else { lo = -Infinity; hi = rng.int(1, 15); }
    }
    const unboundedSide = lo === -Infinity || hi === Infinity;
    // Zero cost on any infinitely-bounded column keeps the objective bounded.
    const cost = unboundedSide ? 0 : rng.int(-5, 5);
    cols.push({ lo, hi, cost, type: 'cont' });
  }

  const witness = cols.map((c) => randomWitness(c, rng));
  const rows = genRows(rng, n, m, witness, { eqProb: cfg.eqProb, maxNz: 4, coefMax: 4 });

  if (cols.every((c) => c.cost === 0)) cols[0].cost = rng.nonzero(5);

  return { model: { cols, rows }, witness, stats: { cols: n, rows: m } };
}

/** A guaranteed-feasible, bounded mixed-integer LP. */
function genMIP(seed, cfg) {
  const rng = makeRng(seed);
  const n = rng.int(cfg.minCols, cfg.maxCols);
  const m = rng.int(cfg.minRows, cfg.maxRows);

  const cols = [];
  for (let j = 0; j < n; j++) {
    const roll = rng.next();
    let type = 'cont';
    let lo;
    let hi;
    if (roll < cfg.intProb) {
      type = 'int';
      lo = -rng.int(0, 4);
      hi = lo + rng.int(1, 6);
    } else if (roll < cfg.intProb + cfg.binProb) {
      type = 'bin';
      lo = 0;
      hi = 1;
    } else {
      lo = -rng.int(0, 4);
      hi = lo + rng.int(1, 8);
    }
    cols.push({ lo, hi, cost: rng.int(-5, 5), type });
  }

  const witness = cols.map((c) => randomWitness(c, rng));
  const rows = genRows(rng, n, m, witness, { eqProb: cfg.eqProb, maxNz: 4, coefMax: 4 });

  if (cols.every((c) => c.cost === 0)) cols[0].cost = rng.nonzero(5);

  const ints = cols.filter((c) => c.type === 'int').length;
  const bins = cols.filter((c) => c.type === 'bin').length;
  return { model: { cols, rows }, witness, stats: { cols: n, rows: m, ints, bins } };
}

// Deliberately infeasible / unbounded instances (hand-built, small).
function makeInfeasible1() {
  const cols = [
    { lo: 0, hi: 10, cost: 1, type: 'cont' },
    { lo: 0, hi: 10, cost: 0, type: 'cont' },
  ];
  return {
    cols,
    rows: [
      { name: 'c0', terms: [[1, 0], [1, 1]], lo: -Infinity, hi: 1 },
      { name: 'c1', terms: [[1, 0], [1, 1]], lo: 3, hi: Infinity },
    ],
  };
}

function makeInfeasible2() {
  const cols = [
    { lo: 0, hi: 10, cost: 1, type: 'cont' },
    { lo: 0, hi: 10, cost: 1, type: 'cont' },
  ];
  return {
    cols,
    rows: [
      { name: 'c0', terms: [[1, 0], [-1, 1]], lo: 5, hi: 5 },
      { name: 'c1', terms: [[1, 0], [-1, 1]], lo: 7, hi: 7 },
    ],
  };
}

function makeUnbounded1() {
  const cols = [
    { lo: 0, hi: Infinity, cost: -1, type: 'cont' },
    { lo: 0, hi: Infinity, cost: -1, type: 'cont' },
  ];
  return {
    cols,
    rows: [{ name: 'c0', terms: [[1, 0], [-1, 1]], lo: -Infinity, hi: 1 }],
  };
}

function makeUnbounded2() {
  const cols = [
    { lo: 0, hi: Infinity, cost: -1, type: 'cont' },
    { lo: 0, hi: 3, cost: 0, type: 'cont' },
    { lo: 0, hi: 3, cost: 0, type: 'cont' },
  ];
  return {
    cols,
    rows: [{ name: 'c0', terms: [[1, 1], [1, 2]], lo: -Infinity, hi: 6 }],
  };
}

// ---------------------------------------------------------------------------
// Instance catalogue
// ---------------------------------------------------------------------------
function continuousCatalogue() {
  const specs = [];
  // 20 guaranteed-feasible continuous LPs of varied size/shape.
  const shapes = [
    { minCols: 10, maxCols: 16, minRows: 5, maxRows: 10, infiniteProb: 0, eqProb: 0.2 },
    { minCols: 12, maxCols: 20, minRows: 6, maxRows: 12, infiniteProb: 0, eqProb: 0.35 },
    { minCols: 18, maxCols: 28, minRows: 8, maxRows: 16, infiniteProb: 0.25, eqProb: 0.2 },
    { minCols: 25, maxCols: 40, minRows: 10, maxRows: 24, infiniteProb: 0.15, eqProb: 0.3 },
    { minCols: 35, maxCols: 60, minRows: 15, maxRows: 40, infiniteProb: 0.3, eqProb: 0.25 },
    { minCols: 10, maxCols: 14, minRows: 5, maxRows: 8, infiniteProb: 0.4, eqProb: 0.15 },
  ];
  for (let i = 0; i < 20; i++) {
    const cfg = shapes[i % shapes.length];
    const seed = 0x1000 + i * 7;
    const { model, witness, stats } = genFeasibleLP(seed, cfg);
    specs.push({
      name: `lp-${String(i + 1).padStart(2, '0')}`,
      kind: 'lp',
      file: `lp-${String(i + 1).padStart(2, '0')}.lp`,
      model,
      witness,
      seed,
      stats,
      seconds: 5,
    });
  }
  const specials = [
    ['lp-21-infeasible', makeInfeasible1()],
    ['lp-22-infeasible', makeInfeasible2()],
    ['lp-23-unbounded', makeUnbounded1()],
    ['lp-24-unbounded', makeUnbounded2()],
  ];
  for (const [name, model] of specials) {
    specs.push({
      name,
      kind: 'lp',
      file: `${name}.lp`,
      model,
      witness: null,
      seed: 0,
      stats: { cols: model.cols.length, rows: model.rows.length },
      seconds: 5,
    });
  }
  return specs;
}

function mipCatalogue() {
  const specs = [];
  const shapes = [
    { minCols: 8, maxCols: 14, minRows: 5, maxRows: 10, intProb: 0.45, binProb: 0.3, eqProb: 0.2 },
    { minCols: 10, maxCols: 18, minRows: 6, maxRows: 12, intProb: 0.3, binProb: 0.4, eqProb: 0.25 },
    { minCols: 14, maxCols: 24, minRows: 8, maxRows: 16, intProb: 0.5, binProb: 0.2, eqProb: 0.3 },
    { minCols: 18, maxCols: 30, minRows: 10, maxRows: 20, intProb: 0.35, binProb: 0.35, eqProb: 0.2 },
    { minCols: 10, maxCols: 16, minRows: 6, maxRows: 12, intProb: 0.55, binProb: 0.25, eqProb: 0.35 },
  ];
  for (let i = 0; i < 15; i++) {
    const cfg = shapes[i % shapes.length];
    const seed = 0x2000 + i * 13;
    const { model, witness, stats } = genMIP(seed, cfg);
    specs.push({
      name: `mip-${String(i + 1).padStart(2, '0')}`,
      kind: 'mip',
      file: `mip-${String(i + 1).padStart(2, '0')}.mip.lp`,
      model,
      witness,
      seed,
      stats,
      seconds: 10,
    });
  }
  return specs;
}

// ---------------------------------------------------------------------------
// Manifest derivation (via HiGHS)
// ---------------------------------------------------------------------------
function expectFromStatus(name) {
  if (name === 'Optimal') return 'feasible';
  if (name === 'Infeasible') return 'infeasible';
  if (name === 'Unbounded' || name === 'UnboundedOrInfeasible') return 'unbounded';
  return 'feasible';
}

async function deriveExpect(highs, lp, seconds) {
  const model = highs.createModel({ format: 'lp', data: lp });
  try {
    model.options.set({ output_flag: false, time_limit: seconds });
    model.run();
    const status = model.getModelStatus();
    const name = statusName(status);
    const detached = model.getModel('csr');
    let objective = null;
    let feasible = null;
    try {
      objective = model.getObjectiveValue();
    } catch { /* ignore */ }
    if (name === 'Optimal') {
      try {
        const check = checkModel(detached, model.getSolution().colValue, 1e-6);
        feasible = check.feasible;
      } catch { /* ignore */ }
    }
    return { status: name, statusCode: status, objective, feasible, detached };
  } finally {
    model.dispose?.();
  }
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------
export async function generateAll({ log = console.log } = {}) {
  mkdirSync(fixturesDir, { recursive: true });
  // Clear previously generated .lp files so the directory matches the manifest.
  if (existsSync(fixturesDir)) {
    for (const f of readdirSync(fixturesDir)) {
      if (f.endsWith('.lp')) unlinkSync(join(fixturesDir, f));
    }
  }

  const specs = [...continuousCatalogue(), ...mipCatalogue()];
  const highs = await loadHighs();
  const manifest = [];

  for (const spec of specs) {
    const lp = renderModel(spec.model);
    writeFileSync(join(fixturesDir, spec.file), lp);

    const ref = await deriveExpect(highs, lp, spec.seconds);
    const expect = expectFromStatus(ref.status);
    if (expect === 'infeasible' || expect === 'unbounded') {
      log(`${spec.name.padEnd(18)} ${spec.kind.padEnd(3)} ${spec.file.padEnd(18)} ${ref.status.padEnd(14)} expect=${expect}`);
    } else if (ref.feasible === false) {
      log(`WARN ${spec.name}: HiGHS claims Optimal but its own solution is infeasible (generator bug?)`);
    } else {
      log(
        `${spec.name.padEnd(18)} ${spec.kind.padEnd(3)} ${spec.file.padEnd(18)} ${ref.status.padEnd(14)} ` +
        `obj=${ref.objective == null ? 'n/a' : ref.objective.toFixed(4)} cols=${ref.detached.numCols} ` +
        `rows=${ref.detached.numRows} nz=${ref.detached.matrix.values.length} expect=${expect}`,
      );
    }

    manifest.push({
      name: spec.name,
      file: spec.file,
      kind: spec.kind,
      seconds: spec.seconds,
      expect,
      seed: spec.seed,
      cols: ref.detached.numCols,
      rows: ref.detached.numRows,
      nnz: ref.detached.matrix.values.length,
      highsStatus: ref.status,
      highsObjective: ref.objective,
    });
  }

  const manifestPath = join(fixturesDir, 'manifest.json');
  writeFileSync(manifestPath, JSON.stringify({ generatedBy: 'tools/gen-lp.mjs', repo: REPO_ROOT, instances: manifest }, null, 2));
  const nLp = manifest.filter((m) => m.kind === 'lp').length;
  const nMip = manifest.filter((m) => m.kind === 'mip').length;
  log(`\nwrote ${manifest.length} fixtures (${nLp} lp, ${nMip} mip) -> ${manifestPath}`);
  return manifest;
}

async function main() {
  await generateAll();
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((err) => {
    console.error(err);
    process.exit(1);
  });
}
