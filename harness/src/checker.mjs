// Independent feasibility checker. Mirrors `optio_core::check_solution` and is
// the harness oracle: it recomputes constraints and the objective from the model
// arrays and never trusts a solver's reported status.

/**
 * @param {object} model detached model (getModel('csr')):
 *   { numCols, numRows, colCost, colLower, colUpper, rowLower, rowUpper,
 *     integrality, matrix: { format, starts, indices, values } }
 * @param {ArrayLike<number>|null} x
 * @param {number} tol
 */
export function checkModel(model, x, tol = 1e-6) {
  const out = {
    feasible: false,
    objective: Number.NaN,
    maxRowViolation: Number.NaN,
    maxBoundViolation: Number.NaN,
    maxIntegralityViolation: Number.NaN,
    error: null,
  };
  const n = model.numCols;
  if (!x || x.length < n) {
    out.error = `missing/short solution (${x ? x.length : 'null'} < ${n})`;
    return out;
  }

  let objective = 0;
  let maxBound = 0;
  let maxIntegrality = 0;
  for (let c = 0; c < n; c++) {
    const v = x[c];
    objective += model.colCost[c] * v;
    maxBound = Math.max(maxBound, model.colLower[c] - v, v - model.colUpper[c]);
    if (model.integrality[c] !== 0) {
      maxIntegrality = Math.max(maxIntegrality, Math.abs(v - Math.round(v)));
    }
  }

  const { starts, indices, values, format } = model.matrix;
  let maxRow = 0;
  if (format === 'csr') {
    for (let r = 0; r < model.numRows; r++) {
      let lhs = 0;
      for (let k = starts[r]; k < starts[r + 1]; k++) lhs += values[k] * x[indices[k]];
      maxRow = Math.max(maxRow, model.rowLower[r] - lhs, lhs - model.rowUpper[r]);
    }
  } else {
    const row = new Float64Array(model.numRows);
    for (let c = 0; c < n; c++) {
      for (let k = starts[c]; k < starts[c + 1]; k++) row[indices[k]] += values[k] * x[c];
    }
    for (let r = 0; r < model.numRows; r++) {
      maxRow = Math.max(maxRow, model.rowLower[r] - row[r], row[r] - model.rowUpper[r]);
    }
  }

  out.objective = objective;
  out.maxRowViolation = maxRow;
  out.maxBoundViolation = maxBound;
  out.maxIntegralityViolation = maxIntegrality;
  out.feasible =
    maxRow <= tol && maxBound <= tol && maxIntegrality <= tol;
  return out;
}

export const MODEL_STATUS = [
  'NotSet', 'LoadError', 'ModelError', 'PresolveError', 'SolveError',
  'PostsolveError', 'Empty', 'Optimal', 'Infeasible', 'UnboundedOrInfeasible',
  'Unbounded', 'ObjectiveBound', 'ObjectiveTarget', 'TimeLimit',
  'IterationLimit', 'Unknown', 'SolutionLimit', 'Interrupt',
];

export function statusName(code) {
  return MODEL_STATUS[code] ?? `Status${code}`;
}
