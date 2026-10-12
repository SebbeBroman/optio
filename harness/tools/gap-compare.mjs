// Compare optio's objective / status / time across `mip_rel_gap` values for a
// single LP, with the option of a native-HiGHS reference. Useful to decide how
// loose a MIP gap tolerance can be before the plan actually changes.
//
// Usage:
//   node tools/gap-compare.mjs <file.lp> [--time 20] [--gaps 0,0.01,0.02,0.05,0.1]
//
// `--gaps 0` means "prove optimal" (gap 0). Times are wall-clock, single run each.
import { readFileSync, existsSync, mkdtempSync, rmSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, basename, join } from 'node:path';
import { tmpdir } from 'node:os';
import { performance } from 'node:perf_hooks';

import { loadHighs, highsSource } from '../src/highs.mjs';
import { solveWithOptio } from '../src/optio.mjs';
import { checkModel, statusName } from '../src/checker.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const venvPython = join(here, 'oracle-venv', 'bin', 'python');

const args = process.argv.slice(2);
const lpPath = args.find((a) => !a.startsWith('--'));
const flag = (name, fallback) => {
	const i = args.indexOf(`--${name}`);
	return i >= 0 && args[i + 1] ? args[i + 1] : fallback;
};
const seconds = Number(flag('time', '20'));
const gaps = flag('gaps', '0,0.01,0.02,0.05,0.1')
	.split(',')
	.map((g) => Number(g))
	.filter((g) => Number.isFinite(g));

if (!lpPath || !existsSync(lpPath)) {
	console.error(`usage: node tools/gap-compare.mjs <file.lp> [--time 20] [--gaps 0,0.05]`);
	process.exit(2);
}

const pad = (s, n) => String(s).padEnd(n);
const padL = (s, n) => String(s).padStart(n);
const num = (v, d = 2) => (Number.isFinite(v) ? v.toFixed(d) : 'n/a');
const pct = (a, b) => (Number.isFinite(a) && Number.isFinite(b) && b !== 0 ? ((a - b) / Math.abs(b)) * 100 : null);

/** Native HiGHS reference objective, via the existing ref-only oracle. */
function nativeReference(lp) {
	if (!existsSync(venvPython)) return null;
	const dir = mkdtempSync(join(tmpdir(), 'gap-ref-'));
	const out = join(dir, 'ref.json');
	try {
		execFileSync(
			venvPython,
			[join(here, 'native-oracle.py'), lp, '--ref-only', '--time', String(Math.max(seconds, 30)), '--json-out', out],
			{ stdio: 'ignore' },
		);
		const records = JSON.parse(readFileSync(out, 'utf8'));
		return records[0]?.reference ?? null;
	} catch {
		return null;
	} finally {
		rmSync(dir, { recursive: true, force: true });
	}
}

async function main() {
	const lp = readFileSync(lpPath, 'utf8');
	const highs = await loadHighs();
	const model = highs.createModel({ format: 'lp', data: lp });
	const detached = model.getModel('csr');
	model.dispose?.();

	console.log(`model:   ${basename(lpPath)}  (${detached.numCols} cols, ${detached.numRows} rows, ${detached.matrix.values.length} nz)`);
	console.log(`highs-js reference build: ${highsSource}`);
	console.log(`time limit: ${seconds}s per run\n`);

	const ref = nativeReference(lpPath);
	const refObj = ref?.objective ?? null;

	console.log(
		pad('mip_rel_gap', 12) + pad('status', 14) + padL('objective', 14) +
		padL('time', 9) + padL('feas', 6) + padL('vs tight', 10) + padL('vs native', 11),
	);
	console.log('-'.repeat(76));

	let tight = null;
	for (const gap of gaps) {
		const t0 = performance.now();
		const res = await solveWithOptio(detached, { seconds, gap, absGap: 0 });
		const ms = performance.now() - t0;
		const status = statusName(res.status);
		const check =
			res.colValue && res.colValue.length >= detached.numCols
				? checkModel(detached, res.colValue, 1e-6)
				: null;
		if (gap === 0 && res.colValue) tight = res.objective;
		const vsTight = tight != null && gap !== 0 ? pct(res.objective, tight) : null;
		const vsNative = refObj != null ? pct(res.objective, refObj) : null;
		const fmtPct = (v) => (v == null ? '—' : `${v.toFixed(2)}%`);
		console.log(
			pad(String(gap), 12) + pad(status, 14) + padL(num(res.objective), 14) +
			padL(`${(ms / 1000).toFixed(2)}s`, 9) +
			padL(check ? (check.feasible ? 'yes' : 'NO') : 'n/a', 6) +
			padL(fmtPct(vsTight), 10) +
			padL(fmtPct(vsNative), 11),
		);
	}

	if (refObj != null) console.log(`\nnative HiGHS: ${num(refObj)} (${ref.status ?? ''})`);
}

main().catch((err) => {
	console.error(err);
	process.exit(1);
});
