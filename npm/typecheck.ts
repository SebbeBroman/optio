// Type-level check of npm/index.d.ts (not shipped). Run with:
//   <tsc> --noEmit --strict --target es2022 --module esnext --moduleResolution bundler typecheck.ts
import {
  init,
  initSync,
  isReady,
  createSolver,
  Model,
  ModelStatus,
  statusName,
  version,
  type SolverModel,
  type Progress,
  type Incumbent,
} from './index.js';

export async function check() {
  await init();
  await init(new URL('https://example.com/x.wasm'));
  await init(new Uint8Array(8));
  initSync(new Uint8Array(8));
  initSync({ module: new WebAssembly.Module(new Uint8Array(8)) });
  const ready: boolean = isReady();
  const v: string = version();

  const solver = createSolver();
  const model: SolverModel = solver.createModel();
  const i: number = model.addVar(0, 3.5);
  model.addRow(0, 1, { indices: [i], values: [1] });
  model.addRow(0, 1, [i], [1]);
  model.options.set('time_limit_secs', 5);
  model.options.set({ output_flag: false });
  model.run({
    callbacks: {
      onProgress: (p: Progress) => p.hasIncumbent,
      onIncumbent: (inc: Incumbent) => inc.colValue.length,
      interrupt: (p: Progress) => p.nodes > 100,
    },
  });
  const status: number = model.getModelStatus();
  const name: string = statusName(status);
  const optimal: 7 = ModelStatus.Optimal;
  const sol: Float64Array = model.getSolution().colValue;
  const lp: string = model.exportModel('lp');
  const interrupted: boolean = model.wasInterrupted();
  model.dispose();
  solver.withModel((m) => m.getObjectiveValue());
  await solver.withModel(async (m) => m.getObjectiveValue());

  const raw = new Model();
  raw.add_model(
    1,
    new Float64Array(1),
    new Float64Array(1),
    new Float64Array(1),
    new Int32Array(1),
    new Float64Array(1),
    new Float64Array(1),
    new Uint32Array([0, 1]),
    new Uint32Array(1),
    new Float64Array(1),
  );
  raw.run_with_callbacks({
    mipImprovingSolution: (inc) => inc.objective,
  });
  raw.free();

  return [ready, v, i, status, name, optimal, sol, lp, interrupted] as const;
}
