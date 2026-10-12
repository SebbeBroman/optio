// Generate feasible, planning-shaped MILP instances (LP text).
//
// Same flavour as the app's model: integer match placement over
// (unit x session x ring), per-ring session loads, hard caps, min/max/range and
// mean-deviation balancing, optional one-ring-per-unit-per-session, and ring
// restrictions (encoded by omitting variables).
//
// Feasibility is guaranteed: a random witness assignment is constructed first
// and the per-(session,ring) cap is set from its max load.
//
// Usage: node tools/gen-planning.mjs   (writes the fixture set)
import { writeFileSync } from 'node:fs';

function mulberry32(a) {
  return function () {
    a |= 0; a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const fmt = (x) => (Number.isInteger(x) ? String(x) : x.toFixed(6));
const term = (coef, name) => `${coef >= 0 ? '+' : ''}${fmt(coef)} ${name}`;

export function generate(cfg) {
  const { units: U, rings: R, days: D, perDay: P, onesplit } = cfg;
  const sessions = D * P;
  const rnd = mulberry32(cfg.seed);
  const M = Array.from({ length: U }, () => Math.max(1, Math.round(cfg.matches * (0.5 + rnd()))));
  const dur = Array.from({ length: U }, () => Math.max(30, Math.round(cfg.duration * (0.5 + rnd()))));
  const allow = Array.from({ length: U }, () =>
    Array.from({ length: R }, () => !(cfg.restrict > 0 && rnd() < cfg.restrict)),
  );
  for (let u = 0; u < U; u++) if (!allow[u].some(Boolean)) allow[u][0] = true;

  // Witness: place each unit's matches in one (session, ring). Guarantees the
  // feasibility of every constraint family below.
  const witness = Array.from({ length: sessions }, () => new Array(R).fill(0));
  for (let u = 0; u < U; u++) {
    const p = u % sessions;
    const start = Math.floor(rnd() * R);
    let r = start;
    for (let k = 0; k < R && !allow[u][r]; k++) r = (r + 1) % R;
    witness[p][r] += M[u] * dur[u];
  }
  const maxLoad = Math.max(...witness.flat());
  const cap = Math.ceil(maxLoad * cfg.capSlack);

  const x = (u, p, r) => `x_${u}_${p}_${r}`;
  const z = (u, p, r) => `z_${u}_${p}_${r}`;

  const obj = [];
  const rows = [];
  const gens = [];
  const bins = [];
  const bounds = [];

  for (let p = 0; p < sessions; p++) {
    obj.push(term(0.05, `peak_${p}`), term(1, `range_${p}`));
    for (let r = 0; r < R; r++) obj.push(term(0.01, `dev_${p}_${r}`));
  }

  const loadTerms = (p, r, scale = 1) => {
    const out = [];
    for (let u = 0; u < U; u++) if (allow[u][r]) out.push(term(dur[u] * scale, x(u, p, r)));
    return out;
  };

  for (let p = 0; p < sessions; p++) {
    const totalDef = [term(1, `total_${p}`)];
    for (let u = 0; u < U; u++)
      for (let r = 0; r < R; r++) if (allow[u][r]) totalDef.push(term(-dur[u], x(u, p, r)));
    rows.push(` totaldef_${p}: ${totalDef.join(' ')} = 0`);

    for (let r = 0; r < R; r++) {
      const load = loadTerms(p, r, 1);
      if (load.length === 0) continue;
      rows.push(` cap_${p}_${r}: ${load.join(' ')} <= ${cap}`);
      rows.push(` peakge_${p}_${r}: ${load.join(' ')} ${term(-1, `peak_${p}`)} <= 0`);
      rows.push(` minle_${p}_${r}: ${load.join(' ')} ${term(-1, `min_${p}`)} >= 0`);
      rows.push(` devhi_${p}_${r}: ${[...loadTerms(p, r, R), term(-1, `total_${p}`), term(-1, `dev_${p}_${r}`)].join(' ')} <= 0`);
      rows.push(` devlo_${p}_${r}: ${[...loadTerms(p, r, -R), term(1, `total_${p}`), term(-1, `dev_${p}_${r}`)].join(' ')} <= 0`);
    }
    rows.push(` rangerel_${p}: ${term(1, `range_${p}`)} ${term(-1, `peak_${p}`)} ${term(1, `min_${p}`)} >= 0`);
  }

  for (let u = 0; u < U; u++) {
    const tot = [];
    for (let p = 0; p < sessions; p++)
      for (let r = 0; r < R; r++) if (allow[u][r]) tot.push(term(1, x(u, p, r)));
    rows.push(` tot_${u}: ${tot.join(' ')} = ${M[u]}`);

    for (let p = 0; p < sessions; p++) {
      const zt = [];
      for (let r = 0; r < R; r++) {
        if (!allow[u][r]) continue;
        gens.push(x(u, p, r));
        bounds.push(` 0 <= ${x(u, p, r)} <= ${M[u]}`);
        if (onesplit) {
          zt.push(term(1, z(u, p, r)));
          rows.push(` link_${u}_${p}_${r}: ${term(1, x(u, p, r))} ${term(-M[u], z(u, p, r))} <= 0`);
          bins.push(z(u, p, r));
          bounds.push(` 0 <= ${z(u, p, r)} <= 1`);
        }
      }
      if (onesplit && zt.length > 0) rows.push(` onesplit_${u}_${p}: ${zt.join(' ')} <= 1`);
    }
  }

  for (let p = 0; p < sessions; p++) {
    for (const nm of [`peak_${p}`, `min_${p}`, `range_${p}`, `total_${p}`]) {
      bounds.push(` 0 <= ${nm} <= 1000000000`);
    }
    for (let r = 0; r < R; r++) bounds.push(` 0 <= dev_${p}_${r} <= 1000000000`);
  }

  const lp = [
    'min',
    ' obj: ' + obj.join(' '),
    'st',
    ...rows.map((r) => ' ' + r),
    'bounds',
    ...bounds,
    'gen',
    ...gens.map((g) => ' ' + g),
    'bin',
    ...bins.map((b) => ' ' + b),
    'end',
    '',
  ].join('\n');
  return {
    lp,
    stats: { units: U, rings: R, sessions, cap, rows: rows.length, intVars: gens.length, binVars: bins.length },
  };
}

// Faithful-family variant: mirrors the app's model structure with finals-day
// choice (y/yone), round split (xr/roundlink), day/roundday equalities, plus the
// ring assignment (x, z/link/onesplit) and balance auxiliaries. Feasible by a
// constructed witness. `rounds` is counts from the final outward.
export function generateFaithful(cfg) {
  const { units: U, rings: R, days: D, perDay: P, rounds } = cfg;
  const roundNames = ['final', 'semifinal', 'quarterfinal', 'earlier'];
  const times = [];
  for (let d = 0; d < D; d++) for (let q = 0; q < P; q++) times.push({ p: times.length, d });
  const rnd = mulberry32(cfg.seed);
  const dur = Array.from({ length: U }, () => Math.max(30, Math.round(cfg.duration * (0.6 + rnd()))));
  const allow = Array.from({ length: U }, () =>
    Array.from({ length: R }, () => !(cfg.restrict > 0 && rnd() < cfg.restrict)),
  );
  for (let u = 0; u < U; u++) if (!allow[u].some(Boolean)) allow[u][0] = true;
  const totalMatches = rounds.reduce((a, b) => a + b, 0);

  // series for a finals day f: round k (from final) plays on day f-k (floored at 0)
  const series = (f) => {
    const matches = new Array(D).fill(0);
    const byType = Array.from({ length: D }, () => ({}));
    for (let k = 0; k < rounds.length; k++) {
      const d = Math.max(0, f - k);
      const type = roundNames[k];
      matches[d] += rounds[k];
      byType[d][type] = (byType[d][type] ?? 0) + rounds[k];
    }
    return { matches, byType };
  };

  const x = (u, p, r) => `x_${u}_${p}_${r}`;
  const xr = (u, t, p, r) => `xr_${u}_${t}_${p}_${r}`;
  const z = (u, p, r) => `z_${u}_${p}_${r}`;
  const y = (u, d) => `y_${u}_${d}`;

  const obj = [];
  const rows = [];
  const gens = [];
  const bins = [];
  const bounds = [];
  for (let p = 0; p < times.length; p++) {
    obj.push(term(0.05, `peak_${p}`), term(1, `range_${p}`));
    for (let r = 0; r < R; r++) obj.push(term(0.01, `dev_${p}_${r}`));
  }
  for (let u = 0; u < U; u++) for (let d = 0; d < D; d++) obj.push(term(-0.1 * d, y(u, d)));

  // Witness: each unit plays its rounds ending on finals day d* = u % D.
  const wload = Array.from({ length: times.length }, () => new Array(R).fill(0));
  for (let u = 0; u < U; u++) {
    const dstar = u % D;
    const ss = series(dstar);
    const r = (() => {
      let rr = u % R;
      for (let k = 0; k < R && !allow[u][rr]; k++) rr = (rr + 1) % R;
      return rr;
    })();
    for (let d = 0; d < D; d++) {
      const p = times.find((t) => t.d === d);
      if (p) wload[p.p][r] += ss.matches[d] * dur[u];
    }
  }
  const cap = Math.ceil(Math.max(...wload.flat()) * cfg.capSlack);

  const loadTerms = (p, r, scale = 1) => {
    const out = [];
    for (let u = 0; u < U; u++) if (allow[u][r]) out.push(term(dur[u] * scale, x(u, times[p].p, r)));
    return out;
  };

  for (let p = 0; p < times.length; p++) {
    const totalDef = [term(1, `total_${p}`)];
    for (let u = 0; u < U; u++) for (let r = 0; r < R; r++) if (allow[u][r]) totalDef.push(term(-dur[u], x(u, p, r)));
    rows.push(` totaldef_${p}: ${totalDef.join(' ')} = 0`);
    for (let r = 0; r < R; r++) {
      const load = loadTerms(p, r, 1);
      if (load.length === 0) continue;
      rows.push(` cap_${p}_${r}: ${load.join(' ')} <= ${cap}`);
      rows.push(` peakge_${p}_${r}: ${load.join(' ')} ${term(-1, `peak_${p}`)} <= 0`);
      rows.push(` minle_${p}_${r}: ${load.join(' ')} ${term(-1, `min_${p}`)} >= 0`);
      rows.push(` devhi_${p}_${r}: ${[...loadTerms(p, r, R), term(-1, `total_${p}`), term(-1, `dev_${p}_${r}`)].join(' ')} <= 0`);
      rows.push(` devlo_${p}_${r}: ${[...loadTerms(p, r, -R), term(1, `total_${p}`), term(-1, `dev_${p}_${r}`)].join(' ')} <= 0`);
    }
    rows.push(` rangerel_${p}: ${term(1, `range_${p}`)} ${term(-1, `peak_${p}`)} ${term(1, `min_${p}`)} >= 0`);
  }

  for (let u = 0; u < U; u++) {
    const yt = [];
    for (let d = 0; d < D; d++) {
      bins.push(y(u, d));
      bounds.push(` 0 <= ${y(u, d)} <= 1`);
      yt.push(term(1, y(u, d)));
    }
    rows.push(` yone_${u}: ${yt.join(' ')} = 1`);

    const tot = [];
    for (const t of times) for (let r = 0; r < R; r++) if (allow[u][r]) tot.push(term(1, x(u, t.p, r)));
    rows.push(` tot_${u}: ${tot.join(' ')} = ${totalMatches}`);

    const dayTerms = Array.from({ length: D }, () => []);
    const roundDay = Array.from({ length: D }, () => []);
    for (const t of times) {
      for (let r = 0; r < R; r++) {
        if (!allow[u][r]) continue;
        gens.push(x(u, t.p, r));
        bounds.push(` 0 <= ${x(u, t.p, r)} <= ${totalMatches}`);
        if (cfg.onesplit) {
          bins.push(z(u, t.p, r));
          bounds.push(` 0 <= ${z(u, t.p, r)} <= 1`);
          rows.push(` link_${u}_${t.p}_${r}: ${term(1, x(u, t.p, r))} ${term(-totalMatches, z(u, t.p, r))} <= 0`);
        }
        // roundlink: x = sum of xr over round types present
        const types = new Set();
        for (let d = 0; d < D; d++) for (const ty of Object.keys(series(d).byType[d] ?? {})) types.add(ty);
        const rl = [term(1, x(u, t.p, r))];
        for (const ty of types) {
          gens.push(xr(u, ty, t.p, r));
          bounds.push(` 0 <= ${xr(u, ty, t.p, r)} <= ${totalMatches}`);
          rl.push(term(-1, xr(u, ty, t.p, r)));
          (roundDay[t.d][ty] ??= []).push(term(1, xr(u, ty, t.p, r)));
        }
        rows.push(` roundlink_${u}_${t.p}_${r}: ${rl.join(' ')} = 0`);
        dayTerms[t.d].push(term(1, x(u, t.p, r)));
      }
      if (cfg.onesplit) {
        const zt = [];
        for (let r = 0; r < R; r++) if (allow[u][r]) zt.push(term(1, z(u, t.p, r)));
        if (zt.length > 1) rows.push(` onesplit_${u}_${t.p}: ${zt.join(' ')} <= 1`);
      }
    }
    for (let d = 0; d < D; d++) {
      // day: sum x on day d - sum_f series(f).matches[d] y_f = 0
      const dt = [...dayTerms[d]];
      for (let f = 0; f < D; f++) {
        const m = series(f).matches[d];
        if (m !== 0) dt.push(term(-m, y(u, f)));
      }
      rows.push(` day_${u}_${d}: ${dt.join(' ')} = 0`);
      for (const [ty, terms] of Object.entries(roundDay[d])) {
        const rt = [...terms];
        for (let f = 0; f < D; f++) {
          const c = series(f).byType[d][ty] ?? 0;
          if (c !== 0) rt.push(term(-c, y(u, f)));
        }
        rows.push(` roundday_${u}_${d}_${ty}: ${rt.join(' ')} = 0`);
      }
    }
  }

  for (let p = 0; p < times.length; p++) {
    for (const nm of [`peak_${p}`, `min_${p}`, `range_${p}`, `total_${p}`]) bounds.push(` 0 <= ${nm} <= 1000000000`);
    for (let r = 0; r < R; r++) bounds.push(` 0 <= dev_${p}_${r} <= 1000000000`);
  }

  const lp = [
    'min',
    ' obj: ' + obj.join(' '),
    'st',
    ...rows.map((r) => ' ' + r),
    'bounds',
    ...bounds,
    'gen',
    ...gens.map((g) => ' ' + g),
    'bin',
    ...bins.map((b) => ' ' + b),
    'end',
    '',
  ].join('\n');
  return { lp, stats: { units: U, rings: R, days: D, perDay: P, rounds, rows: rows.length, intVars: gens.length, binVars: bins.length } };
}

const SET = [
  ['planning-s', { units: 24, rings: 3, days: 1, perDay: 2, matches: 3, duration: 600, capSlack: 1.6, restrict: 0, onesplit: true, seed: 1 }],
  ['planning-m', { units: 120, rings: 5, days: 3, perDay: 1, matches: 4, duration: 900, capSlack: 1.5, restrict: 0.15, onesplit: true, seed: 2 }],
  ['planning-l', { units: 200, rings: 5, days: 3, perDay: 2, matches: 5, duration: 900, capSlack: 1.5, restrict: 0.15, onesplit: true, seed: 3 }],
  ['planning-tight', { units: 120, rings: 5, days: 3, perDay: 1, matches: 4, duration: 900, capSlack: 1.02, restrict: 0.15, onesplit: true, seed: 4 }],
];

if (import.meta.url === `file://${process.argv[1]}`) {
  const outDir = new URL('../fixtures/', import.meta.url);
  for (const [name, cfg] of SET) {
    const { lp, stats } = generate(cfg);
    writeFileSync(new URL(`${name}.lp`, outDir), lp);
    console.log(`${name.padEnd(16)} ${JSON.stringify(stats)} ${(lp.length / 1024).toFixed(0)} KB`);
  }
  const faithful = generateFaithful({
    units: 10, rings: 3, days: 3, perDay: 1, rounds: [1],
    duration: 900, capSlack: 1.5, restrict: 0, onesplit: true, seed: 5,
  });
  writeFileSync(new URL('planning-faithful.lp', outDir), faithful.lp);
  console.log(`planning-faithful ${JSON.stringify(faithful.stats)} ${(faithful.lp.length / 1024).toFixed(0)} KB`);
}
