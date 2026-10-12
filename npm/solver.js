/**
 * `solver.js` — the high-level surface of the optio wasm package, inspired by
 * the HiGHS JS API so migrating an existing integration is straightforward.
 *
 * ```js
 * import { init, createSolver } from 'optio';
 * await init();
 *
 * const solver = createSolver();
 * const model = solver.createModel();
 * model.addVar(0, 3.5);
 * // ...
 * model.run({
 *   callbacks: {
 *     onProgress: (p) => console.log(p),
 *     onIncumbent: (inc) => console.log(inc.objective, inc.colValue),
 *     interrupt: () => stopSolving(),
 *   },
 * });
 * model.dispose();
 * ```
 *
 * Ported from the app-side shim in `crates/wasm/ts/`; the package
 * version guards against use before `init()` and exposes `wasInterrupted()`.
 */

import { Model } from './wasm/optio_wasm.js';
import { state } from './state.js';

/** Progress snapshot passed to `onProgress` / `interrupt`. */
/**
 * @typedef {Object} Progress
 * @property {number} runningTime
 * @property {number} nodes
 * @property {number} lpIterations
 * @property {number} gap
 * @property {number} primalBound
 * @property {number} dualBound
 * @property {boolean} hasIncumbent
 */

/** Incumbent passed to `onIncumbent`. */
/**
 * @typedef {Object} Incumbent
 * @property {number} objective
 * @property {Float64Array} colValue
 */

/**
 * Callback map accepted by `model.run` — the partial-solution surface. You
 * receive every improving incumbent as it is found. `interrupt` returns a
 * truthy value to request an early stop; the stop is honoured by the branch &
 * bound loop and reported through `model.wasInterrupted()` and the `Interrupt`
 * model status.
 *
 * @typedef {Object} SolveCallbacks
 * @property {(progress: Progress) => void} [onProgress]
 * @property {(incumbent: Incumbent) => void} [onIncumbent]
 * @property {(progress: Progress) => boolean | void} [interrupt]
 */

/**
 * @typedef {Object} SparseEntries
 * @property {ArrayLike<number>} indices
 * @property {ArrayLike<number>} values
 */

/**
 * @typedef {Object} IisResult
 * @property {Int32Array} col_index
 * @property {Int32Array} row_index
 * @property {Int32Array} colIndex
 * @property {Int32Array} rowIndex
 */

/**
 * `options.set` facade. Unsupported names are ignored.
 *
 * @typedef {Object} OptionsFacade
 * @property {(name: string, value: number | boolean) => void} set
 * @property {(values: Record<string, number | boolean>) => void} set
 */

/**
 * `info.get` facade. Unknown names return `0`.
 *
 * @typedef {Object} InfoFacade
 * @property {(name: string) => number} get
 */

/**
 * One model under construction or holding a last-solve result.
 *
 * @typedef {Object} SolverModel
 * @property {OptionsFacade} options
 * @property {InfoFacade} info
 * @property {(lower: number, upper: number) => number} addVar
 * @property {(lower: number, upper: number, entries: SparseEntries | ArrayLike<number>, values?: ArrayLike<number>) => void} addRow
 * @property {(col: number, cost: number) => void} changeColCost
 * @property {(col: number, type: number) => void} changeColIntegrality
 * @property {(col: number, lower: number) => void} changeColLower
 * @property {(col: number, upper: number) => void} changeColUpper
 * @property {(col: number, lower: number, upper: number) => void} changeColBounds
 * @property {(row: number, lower: number, upper: number) => void} changeRowBounds
 * @property {(col: number, name: string) => void} passColName
 * @property {(row: number, name: string) => void} passRowName
 * @property {(solution: { colValue: ArrayLike<number> } | ArrayLike<number>) => void} setSolution
 * @property {(options?: { callbacks?: SolveCallbacks }) => void} run
 * @property {() => number} getModelStatus
 * @property {() => number} getObjectiveValue
 * @property {() => { colValue: Float64Array }} getSolution
 * @property {() => IisResult} getIis
 * @property {(format: 'lp' | 'mps') => string} exportModel
 * @property {() => boolean} wasInterrupted
 * @property {() => void} dispose
 */

/**
 * The solver API. `createModel()` returns an empty builder; `withModel(op)`
 * scopes a model and disposes it in `finally`. The `withModel(source, op)`
 * overload exists only for call-site compatibility: optio has no LP/MPS
 * *reader*, so passing a source throws a clear error (use `exportModel` for
 * the write direction).
 *
 * @typedef {Object} Solver
 * @property {() => SolverModel} createModel
 * @property {<R>(operation: (model: SolverModel) => R) => R} withModel
 * @property {<R>(source: unknown, operation: (model: SolverModel) => R) => R} withModel
 */

function toI32(values) {
  return values instanceof Int32Array ? values : Int32Array.from(values);
}

function toF64(values) {
  return values instanceof Float64Array ? values : Float64Array.from(values);
}

/** One high-level model wrapping an optio wasm `Model`. */
class WasmModel {
  /** @param {InstanceType<typeof Model>} inner */
  constructor(inner) {
    this.inner = inner;
    this.disposed = false;
  }

  /** @type {OptionsFacade} */
  get options() {
    const self = this;
    return {
      set(/** @type {string | Record<string, number | boolean>} */ nameOrValues, /** @type {number | boolean | undefined} */ value) {
        if (typeof nameOrValues === 'string') {
          self.setOption(nameOrValues, /** @type {number | boolean} */ (value));
        } else {
          for (const [name, v] of Object.entries(nameOrValues)) self.setOption(name, v);
        }
      },
    };
  }

  /** @type {InfoFacade} */
  get info() {
    const self = this;
    return {
      get: (name) => self.inner.get_info(name),
    };
  }

  setOption(name, value) {
    // Boolean options are passed through as 0/1; the wasm setter is numeric.
    this.inner.set_option(name, typeof value === 'boolean' ? (value ? 1 : 0) : value);
  }

  addVar(lower, upper) {
    return this.inner.add_var(lower, upper);
  }

  addRow(lower, upper, entriesOrIndices, values) {
    if (typeof (/** @type {SparseEntries} */ (entriesOrIndices).indices) !== 'undefined') {
      const entries = /** @type {SparseEntries} */ (entriesOrIndices);
      this.inner.add_row(lower, upper, toI32(entries.indices), toF64(entries.values));
    } else {
      this.inner.add_row(
        lower,
        upper,
        toI32(/** @type {ArrayLike<number>} */ (entriesOrIndices)),
        toF64(/** @type {ArrayLike<number>} */ (values)),
      );
    }
  }

  changeColCost(col, cost) {
    this.inner.change_col_cost(col, cost);
  }

  changeColIntegrality(col, type) {
    this.inner.change_col_integrality(col, type);
  }

  changeColLower(col, lower) {
    this.inner.change_col_lower(col, lower);
  }

  changeColUpper(col, upper) {
    this.inner.change_col_upper(col, upper);
  }

  changeColBounds(col, lower, upper) {
    this.inner.change_col_bounds(col, lower, upper);
  }

  changeRowBounds(row, lower, upper) {
    this.inner.change_row_bounds(row, lower, upper);
  }

  passColName(col, name) {
    this.inner.pass_col_name(col, name);
  }

  passRowName(row, name) {
    this.inner.pass_row_name(row, name);
  }

  setSolution(solution) {
    const values = Array.isArray(solution) || ArrayBuffer.isView(solution)
      ? /** @type {ArrayLike<number>} */ (solution)
      : (/** @type {{ colValue: ArrayLike<number> }} */ (solution)).colValue;
    this.inner.set_solution(toF64(values));
  }

  /** @param {{ callbacks?: SolveCallbacks }} [options] */
  run(options) {
    const callbacks = options?.callbacks;
    if (!callbacks) {
      this.inner.run();
      return;
    }
    this.inner.run_with_callbacks({
      mipLogging: callbacks.onProgress ? (p) => callbacks.onProgress?.(p) : undefined,
      mipImprovingSolution: callbacks.onIncumbent
        ? (i) => callbacks.onIncumbent?.(i)
        : undefined,
      mipInterrupt: callbacks.interrupt ? (p) => callbacks.interrupt?.(p) : undefined,
    });
  }

  getModelStatus() {
    return this.inner.get_model_status();
  }

  getObjectiveValue() {
    return this.inner.get_objective_value();
  }

  /** @returns {{ colValue: Float64Array }} */
  getSolution() {
    return /** @type {{ colValue: Float64Array }} */ (this.inner.get_solution_object());
  }

  /** @returns {IisResult} */
  getIis() {
    const raw = /** @type {{ col_index: Int32Array, row_index: Int32Array }} */ (this.inner.get_iis());
    return {
      col_index: raw.col_index,
      row_index: raw.row_index,
      colIndex: raw.col_index,
      rowIndex: raw.row_index,
    };
  }

  /** @param {'lp' | 'mps'} format */
  exportModel(format) {
    return this.inner.export_model(format);
  }

  wasInterrupted() {
    return this.inner.was_interrupted();
  }

  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.inner.free();
  }
}

/**
 * Build the high-level solver API over the package's wasm module.
 *
 * Must be called after `await init()`; `createModel()` enforces that with a
 * clear error instead of a bare wasm TypeError.
 *
 * @returns {Solver}
 */
export function createSolver() {
  const createModel = () => {
    if (!state.ready) {
      throw new Error(
        'optio: the wasm module is not initialized yet. Call `await init()` before `createModel()`.',
      );
    }
    return /** @type {SolverModel} */ (new WasmModel(new Model()));
  };

  /**
   * @param {unknown} arg1
   * @param {unknown} arg2
   */
  function withModel(arg1, arg2) {
    let operation = /** @type {(model: SolverModel) => unknown} */ (arg2);
    if (typeof arg1 === 'function') {
      operation = /** @type {(model: SolverModel) => unknown} */ (arg1);
    } else {
      throw new Error(
        'optio: withModel(source, op) cannot parse LP/MPS text (no reader in optio). ' +
          'Build the model with addVar/addRow instead.',
      );
    }
    const model = createModel();
    let result;
    try {
      result = operation(model);
    } catch (err) {
      model.dispose();
      throw err;
    }
    if (result && typeof (/** @type {PromiseLike<unknown>} */ (result)).then === 'function') {
      return Promise.resolve(result).finally(() => model.dispose());
    }
    model.dispose();
    return result;
  }

  return {
    createModel,
    withModel: /** @type {Solver['withModel']} */ (withModel),
  };
}
