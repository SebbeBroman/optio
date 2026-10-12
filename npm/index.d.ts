/**
 * optio — a data-oriented LP/MIP solver compiled to WebAssembly.
 *
 * Browser-first single package; also runs in Node and Bun. The public surface
 * is a solver API inspired by HiGHS (`createSolver`) plus the raw
 * wasm-bindgen `Model`.
 */

// ---------------------------------------------------------------------------
// Raw wasm surface
// ---------------------------------------------------------------------------

/** Acceptable inputs for {@link init}. */
export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;
/** Acceptable inputs for {@link initSync} (must be available synchronously). */
export type SyncInitInput = BufferSource | WebAssembly.Module;

/** Raw wasm exports (mostly useful for debugging). */
export interface InitOutput {
  readonly memory: WebAssembly.Memory;
}

/**
 * Instantiate the wasm module. Idempotent; concurrent callers share one load.
 *
 * Without arguments the module is loaded automatically: read from disk in
 * Node/Bun, fetched via `import.meta.url` in browsers and bundlers.
 */
export declare function init(input?: InitInput | { module_or_path?: InitInput }): Promise<InitOutput>;

/**
 * Synchronous variant of {@link init}. Without arguments it reads the shipped
 * .wasm from disk (Node/Bun only); in the browser pass bytes or a
 * precompiled `WebAssembly.Module`.
 */
export declare function initSync(input?: SyncInitInput | { module?: SyncInitInput }): InitOutput;

/** True once the wasm module has been instantiated. */
export declare function isReady(): boolean;

/**
 * A model under construction or with a stored last-solve result. Build with
 * `addVar`/`addRow` (or bulk-load with `add_model`), solve with `run`, then
 * read results. Method names follow common solver conventions for easy
 * migration.
 */
export declare class Model {
  constructor();

  /** Bulk-load a whole model in one call from flat CSR arrays (the wasm fast path). Columns are `0..num_cols`; `starts` has `row_lower.length + 1` entries. */
  add_model(num_cols: number, col_cost: Float64Array, col_lower: Float64Array, col_upper: Float64Array, integrality: Int32Array, row_lower: Float64Array, row_upper: Float64Array, starts: Uint32Array, indices: Uint32Array, values: Float64Array): void;
  /** Append a column; returns its index. */
  add_var(lower: number, upper: number): number;
  /** Append a row from parallel index/value arrays. */
  add_row(lower: number, upper: number, indices: Int32Array | number[], values: Float64Array | number[]): void;

  change_col_cost(col: number, cost: number): void;
  /** `ty`: `0` continuous, any nonzero value integer. */
  change_col_integrality(col: number, ty: number): void;
  change_col_lower(col: number, lower: number): void;
  change_col_upper(col: number, upper: number): void;
  change_col_bounds(col: number, lower: number, upper: number): void;
  change_row_bounds(row: number, lower: number, upper: number): void;

  /** Store a column name (also accepted before the column exists). */
  pass_col_name(col: number, name: string): void;
  /** Store a row name. */
  pass_row_name(row: number, name: string): void;
  col_name(col: number): string;
  row_name(row: number): string;

  /** Store a warm-start column vector; validated and polished by the core before use. */
  set_solution(col_value: Float64Array | number[]): void;
  get_warm_start(): Float64Array;
  has_warm_start(): boolean;

  /** Generic option setter; unknown names are ignored (see README for the list). */
  set_option(name: string, value: number): void;
  set_time_limit(secs: number): void;
  set_mip_rel_gap(gap: number): void;
  set_output_flag(on: boolean): void;

  /** Solve. */
  run(): void;
  /**
   * Solve with JS callbacks — the partial-solution surface (these are the
   * raw wasm ABI names; the high-level `SolverModel.run` maps them to
   * `onProgress` / `onIncumbent` / `interrupt`):
   *
   * * `mipLogging(progress)` — progress snapshot;
   * * `mipImprovingSolution(incumbent)` — new incumbent `{ objective, colValue }`;
   * * `mipInterrupt(progress)` — return a truthy value to request an early stop.
   */
  run_with_callbacks(callbacks: {
    mipLogging?: (progress: Progress) => void;
    mipImprovingSolution?: (incumbent: Incumbent) => void;
    mipInterrupt?: (progress: Progress) => boolean | void;
  }): void;
  /** True if a `mipInterrupt` callback requested a stop during the last solve. */
  was_interrupted(): boolean;

  get_model_status(): number;
  get_objective_value(): number;
  /** Column values (copied). */
  get_solution(): Float64Array;
  /** `{ colValue: Float64Array }` — the array is a zero-copy view into wasm memory; copy it if you keep it. */
  get_solution_object(): { colValue: Float64Array };
  /** Zero-copy view of the last solution; invalidated by the next mutating call. */
  get_solution_view(): Float64Array;
  /** Info keys (see README for the table); unknown names return `0`. */
  get_info(name: string): number;
  /** Infeasibility certificate (empty until the core computes an IIS). */
  get_iis(): { col_index: Int32Array; row_index: Int32Array };
  /** Export the model: `"lp"` (CPLEX LP) or `"mps"` (free MPS); other values return `""`. */
  export_model(what: string): string;

  num_col(): number;
  num_row(): number;

  /** Free the model. Prefer `using`/`Symbol.dispose` where available. */
  free(): void;
  [Symbol.dispose](): void;
}

/** Solver version (the wasm crate's version). */
export declare function version(): string;

// ---------------------------------------------------------------------------
// Statuses
// ---------------------------------------------------------------------------

/** Model status codes (stable numeric IDs; see README for the table). */
export declare const ModelStatus: {
  readonly NotSet: 0;
  readonly LoadError: 1;
  readonly ModelError: 2;
  readonly PresolveError: 3;
  readonly SolveError: 4;
  readonly PostsolveError: 5;
  readonly Empty: 6;
  readonly Optimal: 7;
  readonly Infeasible: 8;
  readonly UnboundedOrInfeasible: 9;
  readonly Unbounded: 10;
  readonly ObjectiveBound: 11;
  readonly ObjectiveTarget: 12;
  readonly TimeLimit: 13;
  readonly IterationLimit: 14;
  readonly Unknown: 15;
  readonly SolutionLimit: 16;
  readonly Interrupt: 17;
};

/** Status name of a model status code, e.g. `statusName(7)` → `"Optimal"`. */
export declare function statusName(status: number): string;

// ---------------------------------------------------------------------------
// High-level solver surface (inspired by the HiGHS JS API)
// ---------------------------------------------------------------------------

/** Progress snapshot passed to `onProgress` / `interrupt` callbacks. */
export interface Progress {
  runningTime: number;
  nodes: number;
  lpIterations: number;
  gap: number;
  primalBound: number;
  dualBound: number;
  hasIncumbent: boolean;
}

/** Incumbent passed to `onIncumbent` — every improving solution found. */
export interface Incumbent {
  objective: number;
  colValue: Float64Array;
}

/** Callback map accepted by `SolverModel.run` — the partial-solution surface. */
export interface SolveCallbacks {
  onProgress?: (progress: Progress) => void;
  onIncumbent?: (incumbent: Incumbent) => void;
  /** Return a truthy value to request an early stop. */
  interrupt?: (progress: Progress) => boolean | void;
}

export interface RunOptions {
  callbacks?: SolveCallbacks;
}

/** `options.set` facade. Unsupported names are ignored. */
export interface OptionsFacade {
  set(name: string, value: number | boolean): void;
  set(values: Record<string, number | boolean>): void;
}

/** `info.get` facade. Unknown names return `0`. */
export interface InfoFacade {
  get(name: string): number;
}

/** Sparse row entries accepted by `addRow`. */
export interface SparseEntries {
  indices: ArrayLike<number>;
  values: ArrayLike<number>;
}

export interface IisResult {
  col_index: Int32Array;
  row_index: Int32Array;
  /** camelCase aliases. */
  colIndex: Int32Array;
  rowIndex: Int32Array;
}

/** One high-level model wrapping a wasm `Model`. */
export interface SolverModel {
  readonly options: OptionsFacade;
  readonly info: InfoFacade;

  addVar(lower: number, upper: number): number;
  addRow(lower: number, upper: number, entries: SparseEntries): void;
  addRow(lower: number, upper: number, indices: ArrayLike<number>, values: ArrayLike<number>): void;

  changeColCost(col: number, cost: number): void;
  changeColIntegrality(col: number, type: number): void;
  changeColLower(col: number, lower: number): void;
  changeColUpper(col: number, upper: number): void;
  changeColBounds(col: number, lower: number, upper: number): void;
  changeRowBounds(row: number, lower: number, upper: number): void;

  passColName(col: number, name: string): void;
  passRowName(row: number, name: string): void;

  setSolution(solution: { colValue: ArrayLike<number> } | ArrayLike<number>): void;

  run(options?: RunOptions): void;

  getModelStatus(): number;
  getObjectiveValue(): number;
  getSolution(): { colValue: Float64Array };
  getIis(): IisResult;
  exportModel(format: 'lp' | 'mps'): string;
  /** True if an `interrupt` callback requested a stop during the last solve. */
  wasInterrupted(): boolean;

  dispose(): void;
}

/** The solver API (`createModel` / `withModel`). */
export interface Solver {
  createModel(): SolverModel;
  withModel<R>(operation: (model: SolverModel) => R): R;
  withModel<R>(operation: (model: SolverModel) => PromiseLike<R>): Promise<R>;
  withModel<R>(source: unknown, operation: (model: SolverModel) => R): R;
  withModel<R>(source: unknown, operation: (model: SolverModel) => PromiseLike<R>): Promise<R>;
}

/**
 * Build the high-level solver API over the package's wasm module.
 * Must be called after `await init()`.
 */
export declare function createSolver(): Solver;

/** `import init from 'optio'` — the wasm-bindgen style default export. */
export default init;
