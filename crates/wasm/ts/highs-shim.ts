/**
 * `highs-shim.ts` — a thin adapter that exposes the subset of the `highs`
 * (highs-js) extended API used by `mip-plan.ts`, backed by the `optio-wasm`
 * `Model` class.
 *
 * The optio surface is `snake_case` and builder-style; highs-js is `camelCase`
 * and metadata-returning. This module hides the difference so the app can pick
 * the implementation with a feature flag and no other changes.
 *
 * It is intentionally not compiled by the Rust crate. Import the wasm module
 * with whatever loader the app already uses (bundler, `createRequire`, etc.)
 * and pass it to {@link createOptioHighs}.
 *
 * ```ts
 * import createOptioModule from '../pkg/optio_wasm.js';
 * import { createOptioHighs, optioEnabled } from './highs-shim';
 *
 * const highs = optioEnabled()
 *   ? createOptioHighs(await createOptioModule())
 *   : realHighs;
 * ```
 */

// Type-only import: erased at runtime, so the path is a compile-time hint.
// Adjust to the emitted `pkg/` location in the app.
import type { Model as OptioModel } from '../../../pkg/optio_wasm';

/** wasm-bindgen module namespace (nodejs / bundler targets both export `Model`). */
export interface OptioWasmModule {
  Model: new () => OptioModel;
  version(): string;
}

/** Progress snapshot passed to `mipLogging` / `mipInterrupt`. */
export interface MipProgress {
  runningTime: number;
  nodes: number;
  lpIterations: number;
  gap: number;
  primalBound: number;
  dualBound: number;
  hasIncumbent: boolean;
}

/** Incumbent passed to `mipImprovingSolution`. */
export interface MipIncumbent {
  objective: number;
  colValue: Float64Array;
}

/**
 * Callback map accepted by {@link HighsModel.run}. Mirrors the names the
 * planning app registers. `mipInterrupt` returns a truthy value to request an
 * early stop (see the wasm note: the core solver does not consume the request
 * yet, so it is recorded on the model).
 */
export interface HighsCallbacks {
  mipLogging?: (progress: MipProgress) => void;
  mipImprovingSolution?: (incumbent: MipIncumbent) => void;
  mipInterrupt?: (progress: MipProgress) => boolean | void;
}

export interface RunOptions {
  callbacks?: HighsCallbacks;
}

/** `options.set` facade, matching `highs-js`. Unsupported names are ignored. */
export interface HighsOptionsFacade {
  set(name: string, value: number | boolean): void;
  set(values: Record<string, number | boolean>): void;
}

/** `info.get` facade, matching `highs-js`. Unknown names return `0`. */
export interface HighsInfoFacade {
  get(name: string): number;
}

/** Sparse row/column entries accepted by `addVar`-style builders. */
export interface SparseEntries {
  indices: ArrayLike<number>;
  values: ArrayLike<number>;
}

export interface IisResult {
  col_index: Int32Array;
  row_index: Int32Array;
  /** camelCase aliases for the newer highs-js shape. */
  colIndex: Int32Array;
  rowIndex: Int32Array;
}

export interface HighsModel {
  readonly options: HighsOptionsFacade;
  readonly info: HighsInfoFacade;

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

  dispose(): void;
}

export interface HighsApi {
  createModel(): HighsModel;
  withModel<R>(operation: (model: HighsModel) => R): R;
  withModel<R>(operation: (model: HighsModel) => PromiseLike<R>): Promise<R>;
  withModel<R>(source: unknown, operation: (model: HighsModel) => R): R;
  withModel<R>(source: unknown, operation: (model: HighsModel) => PromiseLike<R>): Promise<R>;
}

function toI32(values: ArrayLike<number>): Int32Array {
  return values instanceof Int32Array ? values : Int32Array.from(values);
}

function toF64(values: ArrayLike<number>): Float64Array {
  return values instanceof Float64Array ? values : Float64Array.from(values);
}

/** One `highs-js`-shaped model wrapping an `optio-wasm` `Model`. */
class OptioHighsModel implements HighsModel {
  private readonly inner: OptioModel;
  private disposed = false;

  readonly options: HighsOptionsFacade = {
    set: (nameOrValues: string | Record<string, number | boolean>, value?: number | boolean) => {
      if (typeof nameOrValues === 'string') {
        this.setOption(nameOrValues, value as number | boolean);
      } else {
        for (const [name, v] of Object.entries(nameOrValues)) this.setOption(name, v);
      }
    },
  };

  readonly info: HighsInfoFacade = {
    get: (name: string) => this.inner.get_info(name),
  };

  constructor(inner: OptioModel) {
    this.inner = inner;
  }

  private setOption(name: string, value: number | boolean): void {
    // `output_flag`/`presolve` are booleans in highs-js; the wasm setter takes
    // a number. Everything else is already numeric.
    this.inner.set_option(name, typeof value === 'boolean' ? (value ? 1 : 0) : value);
  }

  addVar(lower: number, upper: number): number {
    return this.inner.add_var(lower, upper);
  }

  addRow(lower: number, upper: number, entriesOrIndices: SparseEntries | ArrayLike<number>, values?: ArrayLike<number>): void {
    if (typeof (entriesOrIndices as SparseEntries).indices !== 'undefined') {
      const entries = entriesOrIndices as SparseEntries;
      this.inner.add_row(lower, upper, toI32(entries.indices), toF64(entries.values));
    } else {
      this.inner.add_row(lower, upper, toI32(entriesOrIndices as ArrayLike<number>), toF64(values as ArrayLike<number>));
    }
  }

  changeColCost(col: number, cost: number): void {
    this.inner.change_col_cost(col, cost);
  }

  changeColIntegrality(col: number, type: number): void {
    this.inner.change_col_integrality(col, type);
  }

  changeColLower(col: number, lower: number): void {
    this.inner.change_col_lower(col, lower);
  }

  changeColUpper(col: number, upper: number): void {
    this.inner.change_col_upper(col, upper);
  }

  changeColBounds(col: number, lower: number, upper: number): void {
    this.inner.change_col_bounds(col, lower, upper);
  }

  changeRowBounds(row: number, lower: number, upper: number): void {
    this.inner.change_row_bounds(row, lower, upper);
  }

  passColName(col: number, name: string): void {
    this.inner.pass_col_name(col, name);
  }

  passRowName(row: number, name: string): void {
    this.inner.pass_row_name(row, name);
  }

  setSolution(solution: { colValue: ArrayLike<number> } | ArrayLike<number>): void {
    const values = Array.isArray(solution) || ArrayBuffer.isView(solution)
      ? (solution as ArrayLike<number>)
      : (solution as { colValue: ArrayLike<number> }).colValue;
    this.inner.set_solution(toF64(values));
  }

  run(options?: RunOptions): void {
    const callbacks = options?.callbacks;
    if (!callbacks) {
      this.inner.run();
      return;
    }
    this.inner.run_with_callbacks({
      mipLogging: callbacks.mipLogging ? (p: MipProgress) => callbacks.mipLogging!(p) : undefined,
      mipImprovingSolution: callbacks.mipImprovingSolution
        ? (i: MipIncumbent) => callbacks.mipImprovingSolution!(i)
        : undefined,
      mipInterrupt: callbacks.mipInterrupt ? (p: MipProgress) => callbacks.mipInterrupt!(p) : undefined,
    });
  }

  getModelStatus(): number {
    return this.inner.get_model_status();
  }

  getObjectiveValue(): number {
    return this.inner.get_objective_value();
  }

  getSolution(): { colValue: Float64Array } {
    return this.inner.get_solution_object() as { colValue: Float64Array };
  }

  getIis(): IisResult {
    const raw = this.inner.get_iis() as { col_index: Int32Array; row_index: Int32Array };
    return {
      col_index: raw.col_index,
      row_index: raw.row_index,
      colIndex: raw.col_index,
      rowIndex: raw.row_index,
    };
  }

  exportModel(format: 'lp' | 'mps'): string {
    return this.inner.export_model(format);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.inner.free();
  }
}

/**
 * Build the `highs`-shaped API from a loaded `optio-wasm` module.
 *
 * `createModel()` returns an empty builder. `withModel(op)` scopes the model
 * and disposes it in `finally`. The two `withModel(source, op)` overloads exist
 * only for call-site compatibility: optio has no LP/MPS *reader*, so passing a
 * source throws a clear error (use `exportModel` for the write direction).
 */
export function createOptioHighs(wasm: OptioWasmModule): HighsApi {
  const createModel = (): HighsModel => new OptioHighsModel(new wasm.Model());

  function withModel<R>(arg1: unknown, arg2?: unknown): R | Promise<R> {
    let operation: (model: HighsModel) => R | PromiseLike<R>;
    if (typeof arg1 === 'function') {
      operation = arg1 as (model: HighsModel) => R | PromiseLike<R>;
    } else {
      throw new Error(
        'optio: withModel(source, op) cannot parse LP/MPS text (no reader in optio-wasm). ' +
          'Build the model with addVar/addRow instead.',
      );
    }
    const model = createModel();
    let result: R | PromiseLike<R>;
    try {
      result = operation(model);
    } catch (err) {
      model.dispose();
      throw err;
    }
    if (result && typeof (result as PromiseLike<R>).then === 'function') {
      return Promise.resolve(result).finally(() => model.dispose());
    }
    model.dispose();
    return result as R;
  }

  return {
    createModel,
    withModel: withModel as HighsApi['withModel'],
  };
}

/**
 * Feature flag. `true` when `OPTIO_WASM=1` / `OPTIO_WASM=true` (Node/`process`)
 * or `globalThis.__USE_OPTIO__ === true` (browser), unless an explicit override
 * is supplied.
 */
export function optioEnabled(override?: boolean): boolean {
  if (typeof override === 'boolean') return override;
  const flag = (globalThis as { __USE_OPTIO__?: unknown }).__USE_OPTIO__;
  if (flag === true) return true;
  const env = (globalThis as { process?: { env?: Record<string, string | undefined> } }).process?.env;
  return env?.OPTIO_WASM === '1' || env?.OPTIO_WASM === 'true';
}

/**
 * Pick between the real `highs` API and the optio shim at one boundary. Keep
 * the real implementation as the fallback so hard models can still be solved.
 */
export function selectHighs(real: HighsApi, optio: HighsApi, override?: boolean): HighsApi {
  return optioEnabled(override) ? optio : real;
}
