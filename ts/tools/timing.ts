// Print the solve phase breakdown for a detached model JSON (profiling aid).
// Runs `reps` solves and reports the minimum per phase (least noisy).
import { readFileSync } from "node:fs";
import { modelFromJson, type ModelJson } from "../src/bench.ts";
import { StdClock } from "../src/clock.ts";
import { solveWith } from "../src/solve.ts";
import { defaultOptions } from "../src/options.ts";

const file = process.argv[2];
const time = Number(process.argv[3] ?? 10);
const reps = Number(process.argv[4] ?? 5);
const v = JSON.parse(readFileSync(file, "utf8")) as ModelJson;
const model = modelFromJson(v, 1);
const opts = defaultOptions();
opts.timeLimitSecs = time;
opts.outputFlag = false;

interface Phase {
  presolve: number;
  heuristic: number;
  localSearch: number;
  cuts: number;
  boundCert: number;
  branchAndBound: number;
}
const best: Phase = {
  presolve: Infinity,
  heuristic: Infinity,
  localSearch: Infinity,
  cuts: Infinity,
  boundCert: Infinity,
  branchAndBound: Infinity,
};
let bestTotal = Infinity;
let obj = 0;
for (let i = 0; i < reps; i++) {
  const clock = new StdClock();
  const sol = solveWith(model, opts, clock, {});
  obj = sol.objective;
  const p: Phase = {
    presolve: sol.presolveSecs,
    heuristic: sol.heuristicSecs,
    localSearch: sol.localSearchSecs,
    cuts: sol.cutSecs,
    boundCert: sol.boundSecs,
    branchAndBound: sol.bbSecs,
  };
  for (const k of Object.keys(p) as (keyof Phase)[]) best[k] = Math.min(best[k], p[k]);
  bestTotal = Math.min(bestTotal, clock.nowSecs() - 0 + p.presolve + p.heuristic + p.localSearch + p.cuts + p.boundCert + p.branchAndBound);
}
const ms = (x: number) => (Number.isFinite(x) ? Math.round(x * 1000) : 0);
const total =
  best.presolve + best.heuristic + best.localSearch + best.cuts + best.boundCert + best.branchAndBound;
console.log(
  JSON.stringify({
    objective: obj,
    totalMs: ms(total),
    phaseMs: {
      presolve: ms(best.presolve),
      heuristic: ms(best.heuristic),
      localSearch: ms(best.localSearch),
      cuts: ms(best.cuts),
      boundCert: ms(best.boundCert),
      branchAndBound: ms(best.branchAndBound),
    },
  }),
);
