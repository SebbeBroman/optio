// Differential oracle: run each fixture through highs-js, then independently
// verify the returned solution with `checkModel`. With --optio, also run optio
// on the same model and compare (optio returns NotSet until the solver lands).
//
// Usage:  node src/oracle.mjs [--optio]   (HIGHS_JS, TIME_LIMIT env overrides)
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { performance } from 'node:perf_hooks';

import { loadHighs, highsSource } from './highs.mjs';
import { checkModel, statusName } from './checker.mjs';
import { FIXTURES, NO_SOLUTION_STATUSES } from './fixtures.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const fixturesDir = join(here, '..', 'fixtures');
const wantOptio = process.argv.includes('--optio');
const timeLimit = Number(process.env.TIME_LIMIT ?? 0);

const pad = (s, n) => String(s).padEnd(n);
const num = (v, d = 0) => (Number.isFinite(v) ? v.toFixed(d) : String(v));

async function runHighs(lp, seconds) {
  const highs = await loadHighs();
  const t0 = performance.now();
  const model = highs.createModel({ format: 'lp', data: lp });
  try {
    model.options.set({ output_flag: false, time_limit: seconds });
    model.run();
    const status = model.getModelStatus();
    const detached = model.getModel('csr');
    const name = statusName(status);
    const expectsSolution = !NO_SOLUTION_STATUSES.has(name);
    let colValue = null;
    if (expectsSolution) {
      try {
        colValue = model.getSolution().colValue;
      } catch {
        colValue = null;
      }
    }
    const check = expectsSolution ? checkModel(detached, colValue) : null;
    return {
      status,
      name,
      objective: model.getObjectiveValue(),
      detached,
      colValue,
      check,
      ms: performance.now() - t0,
    };
  } finally {
    model.dispose?.();
  }
}

async function main() {
  let solveWithOptio = null;
  if (wantOptio) {
    try {
      ({ solveWithOptio } = await import('./optio.mjs'));
    } catch (err) {
      console.error(`--optio requested but unavailable: ${err.message}`);
    }
  }

  console.log(`highs-js: ${highsSource}\n`);
  console.log(
    pad('fixture', 20) + pad('status', 14) + pad('objective', 14) +
    pad('cols', 7) + pad('rows', 7) + pad('nz', 8) + pad('maxViol', 11) + pad('feas', 6) + pad('ms', 8),
  );
  console.log('-'.repeat(95));

  const report = [];
  for (const { file, seconds: s } of FIXTURES) {
    const path = join(fixturesDir, file);
    if (!existsSync(path)) {
      console.log(pad(file, 20) + 'MISSING');
      continue;
    }
    const lp = readFileSync(path, 'utf8');
    const seconds = timeLimit > 0 ? timeLimit : s;
    const rec = { file, seconds };
    try {
      const h = await runHighs(lp, seconds);
      const c = h.check;
      Object.assign(rec, {
        highs: {
          status: h.name,
          statusCode: h.status,
          objective: h.objective,
          feasible: c ? c.feasible : null,
          maxRowViolation: c ? c.maxRowViolation : null,
          maxBoundViolation: c ? c.maxBoundViolation : null,
          maxIntegralityViolation: c ? c.maxIntegralityViolation : null,
          error: c ? c.error : null,
          ms: h.ms,
        },
        cols: h.detached.numCols,
        rows: h.detached.numRows,
        nnz: h.detached.matrix.values.length,
      });
      const viol = c
        ? Math.max(c.maxRowViolation, c.maxBoundViolation, c.maxIntegralityViolation)
        : null;
      console.log(
        pad(file, 20) + pad(h.name, 14) + pad(num(h.objective, 4), 14) +
        pad(h.detached.numCols, 7) + pad(h.detached.numRows, 7) +
        pad(h.detached.matrix.values.length, 8) +
        pad(viol == null ? 'n/a' : viol.toExponential(1), 11) +
        pad(c ? (c.feasible ? 'yes' : 'NO') : 'n/a', 6) + pad(num(h.ms, 0), 8),
      );

      if (solveWithOptio) {
        try {
          const o = await solveWithOptio(h.detached, { seconds });
          const oname = statusName(o.status);
          const expects = !NO_SOLUTION_STATUSES.has(oname);
          rec.optio = {
            status: oname,
            statusCode: o.status,
            objective: o.objective,
            check:
              expects && o.colValue
                ? checkModel(h.detached, Array.from(o.colValue), 1e-6)
                : null,
          };
          console.log(
            pad('  optio', 20) + pad(oname, 14) + pad(num(o.objective, 4), 14),
          );
        } catch (err) {
          rec.optio = { error: String(err.message ?? err) };
        }
      }
    } catch (err) {
      rec.error = String(err?.message ?? err);
      console.log(pad(file, 20) + 'ERROR ' + rec.error);
    }
    report.push(rec);
  }

  const out = join(here, '..', 'oracle-report.json');
  writeFileSync(out, JSON.stringify({ highs: highsSource, generatedAt: new Date().toISOString(), report }, null, 2));
  console.log(`\nwrote ${out}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
