//! Exact propagation of binary cardinality rows. No objective or row is removed,
//! so the original solution coordinates and independent checker remain valid.
use crate::model::Model;
use crate::solve::Clock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PropagationResult {
    Complete { changed: bool },
    Infeasible,
    TimedOut { changed: bool },
}

/// Prepared row adjacency for incremental propagation after a branch. The caller
/// must pass bounds already propagated at the parent when using `Some(column)`;
/// `None` establishes propagation from arbitrary bounds by visiting every row.
pub(crate) struct PreparedPropagation<'a> {
    model: &'a Model,
    adjacency: Vec<Vec<usize>>,
    eligible: Vec<bool>,
    queued: Vec<bool>,
    queue: std::collections::VecDeque<usize>,
    activities: Vec<(usize, f64, f64, f64, bool, bool)>,
    #[cfg(test)]
    row_visits: usize,
}

impl<'a> PreparedPropagation<'a> {
    pub(crate) fn new(model: &'a Model) -> Self {
        let mut adjacency = vec![Vec::new(); model.num_col];
        let mut eligible = vec![true; model.num_row];
        let mut seen = vec![usize::MAX; model.num_col];
        for r in 0..model.num_row {
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                let j = model.a_index[k] as usize;
                if seen[j] == r || !model.a_value[k].is_finite() {
                    eligible[r] = false;
                }
                seen[j] = r;
            }
            if eligible[r] {
                for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                    if model.a_value[k] != 0.0 {
                        adjacency[model.a_index[k] as usize].push(r);
                    }
                }
            }
        }
        Self {
            model,
            adjacency,
            eligible,
            queued: vec![false; model.num_row],
            queue: std::collections::VecDeque::new(),
            activities: Vec::new(),
            #[cfg(test)]
            row_visits: 0,
        }
    }

    fn enqueue_column(&mut self, j: usize, except: Option<usize>) {
        for &r in &self.adjacency[j] {
            if Some(r) != except && !self.queued[r] {
                self.queued[r] = true;
                self.queue.push_back(r);
            }
        }
    }

    pub(crate) fn run(
        &mut self,
        lower: &mut [f64],
        upper: &mut [f64],
        seed: Option<usize>,
        clock: &dyn Clock,
        deadline: f64,
    ) -> PropagationResult {
        use crate::model::INF;
        assert_eq!(lower.len(), self.model.num_col);
        assert_eq!(upper.len(), self.model.num_col);
        self.queue.clear();
        self.queued.fill(false);
        #[cfg(test)]
        {
            self.row_visits = 0;
        }
        let mut changed = false;
        if clock.now_secs() >= deadline {
            return PropagationResult::TimedOut { changed };
        }
        // A seeded call inherits a fully propagated parent, so only the
        // branched domain can need normalization/validation. Full propagation
        // also supports arbitrary input bounds and re-entry after a timeout.
        let domains = if let Some(j) = seed {
            assert!(j < lower.len());
            j..j + 1
        } else {
            0..lower.len()
        };
        for j in domains {
            if seed.is_none() && j % 256 == 0 && clock.now_secs() >= deadline {
                return PropagationResult::TimedOut { changed };
            }
            if self.model.col_type[j].is_integral() {
                let lo = lower[j].ceil();
                let hi = upper[j].floor();
                if lo != lower[j] || hi != upper[j] {
                    lower[j] = lo;
                    upper[j] = hi;
                    changed = true;
                    self.enqueue_column(j, None);
                }
            }
            if lower[j] > upper[j] {
                return PropagationResult::Infeasible;
            }
        }
        if let Some(j) = seed {
            self.enqueue_column(j, None);
        } else {
            for r in 0..self.model.num_row {
                if self.eligible[r] && !self.queued[r] {
                    self.queued[r] = true;
                    self.queue.push_back(r);
                }
            }
        }
        while let Some(r) = self.queue.pop_front() {
            self.queued[r] = false;
            if clock.now_secs() >= deadline {
                return PropagationResult::TimedOut { changed };
            }
            #[cfg(test)]
            {
                self.row_visits += 1;
            }
            self.activities.clear();
            let (mut min, mut max, mut magnitude) = (0.0, 0.0, 1.0);
            let (mut min_inf, mut max_inf) = (0usize, 0usize);
            for k in self.model.a_start[r] as usize..self.model.a_start[r + 1] as usize {
                if k % 256 == 0 && clock.now_secs() >= deadline {
                    return PropagationResult::TimedOut { changed };
                }
                let j = self.model.a_index[k] as usize;
                let a = self.model.a_value[k];
                if a == 0.0 {
                    continue;
                }
                let (lo, hi) = if a > 0.0 {
                    (lower[j], upper[j])
                } else {
                    (upper[j], lower[j])
                };
                let (l, u) = (a * lo, a * hi);
                // Test the bound before multiplication: a small coefficient
                // must not turn the infinity sentinel into a finite activity.
                let lf = lo.is_finite() && lo.abs() < INF && l.is_finite();
                let uf = hi.is_finite() && hi.abs() < INF && u.is_finite();
                if lf {
                    min += l;
                    magnitude += l.abs();
                } else {
                    min_inf += 1;
                }
                if uf {
                    max += u;
                    magnitude += u.abs();
                } else {
                    max_inf += 1;
                }
                self.activities.push((j, a, l, u, lf, uf));
            }
            let row_lo = self.model.row_lower[r];
            let row_hi = self.model.row_upper[r];
            let lo_finite = row_lo.is_finite() && row_lo > -INF;
            let hi_finite = row_hi.is_finite() && row_hi < INF;
            // Upper-only deductions tighten maximum contributions but leave
            // every minimum contribution unchanged; lower-only deductions do
            // the converse. Their own activity/implications therefore remain
            // valid without a rescan. Other adjacent rows still need visiting.
            // Two-sided rows can change activities used by their opposite side.
            let except = if lo_finite != hi_finite {
                Some(r)
            } else {
                None
            };
            if lo_finite {
                magnitude += row_lo.abs();
            }
            if hi_finite {
                magnitude += row_hi.abs();
            }
            let error = 1e-7 + 64.0 * f64::EPSILON * (self.activities.len() + 1) as f64 * magnitude;
            if !min.is_finite() || !max.is_finite() || !error.is_finite() {
                continue;
            }
            if (min_inf == 0 && hi_finite && min - error > row_hi)
                || (max_inf == 0 && lo_finite && max + error < row_lo)
            {
                return PropagationResult::Infeasible;
            }
            for t in 0..self.activities.len() {
                let (j, a, l, u, lf, uf) = self.activities[t];
                let (mut lo, mut hi) = (lower[j], upper[j]);
                if min_inf == usize::from(!lf) && hi_finite {
                    let bound = (row_hi - (min - if lf { l } else { 0.0 }) + error) / a;
                    if bound.is_finite() {
                        if a > 0.0 {
                            hi = hi.min(bound.next_up());
                        } else {
                            lo = lo.max(bound.next_down());
                        }
                    }
                }
                if max_inf == usize::from(!uf) && lo_finite {
                    let bound = (row_lo - (max - if uf { u } else { 0.0 }) - error) / a;
                    if bound.is_finite() {
                        if a > 0.0 {
                            lo = lo.max(bound.next_down());
                        } else {
                            hi = hi.min(bound.next_up());
                        }
                    }
                }
                let integral = self.model.col_type[j].is_integral();
                if integral {
                    lo = lo.ceil();
                    hi = hi.floor();
                }
                if lo > hi {
                    return PropagationResult::Infeasible;
                }
                let tol = if integral {
                    0.0
                } else {
                    1e-9 * (1.0 + lo.abs().min(hi.abs()))
                };
                let tightened = lo > lower[j] + tol || hi < upper[j] - tol;
                if lo > lower[j] + tol {
                    lower[j] = lo;
                }
                if hi < upper[j] - tol {
                    upper[j] = hi;
                }
                if tightened {
                    changed = true;
                    self.enqueue_column(j, except);
                }
            }
        }
        PropagationResult::Complete { changed }
    }
}

pub fn propagate(model: &Model, clock: &dyn Clock, deadline: f64) -> Result<Model, ()> {
    let mut reduced = model.clone();
    // Only integer unit-coefficient rows with binary domains are eligible.
    // This deliberately avoids floating-point implied-bound rounding.
    let mut seen = vec![usize::MAX; model.num_col];
    let rows: Vec<usize> = (0..model.num_row)
        .filter(|&r| {
            let range = model.a_start[r] as usize..model.a_start[r + 1] as usize;
            range.clone().all(|k| {
                let j = model.a_index[k] as usize;
                model.a_value[k] == 1.0
                    && model.col_type[j].is_integral()
                    && model.col_lower[j] >= 0.0
                    && model.col_upper[j] <= 1.0
                    && {
                        let fresh = seen[j] != r;
                        seen[j] = r;
                        fresh
                    }
            })
        })
        .collect();
    for j in 0..model.num_col {
        if reduced.col_type[j].is_integral() {
            reduced.col_lower[j] = reduced.col_lower[j].ceil();
            reduced.col_upper[j] = reduced.col_upper[j].floor();
            if reduced.col_lower[j] > reduced.col_upper[j] {
                return Err(());
            }
        }
    }
    for _ in 0..32 {
        if clock.now_secs() >= deadline {
            break;
        }
        let mut changed = false;
        for &r in &rows {
            let range = model.a_start[r] as usize..model.a_start[r + 1] as usize;
            let min: f64 = range
                .clone()
                .map(|k| reduced.col_lower[model.a_index[k] as usize])
                .sum();
            let max: f64 = range
                .clone()
                .map(|k| reduced.col_upper[model.a_index[k] as usize])
                .sum();
            let lo = model.row_lower[r].ceil();
            let hi = model.row_upper[r].floor();
            if min > hi || max < lo {
                return Err(());
            }
            for k in range {
                let j = model.a_index[k] as usize;
                if reduced.col_lower[j] == reduced.col_upper[j] {
                    continue;
                }
                if min == hi {
                    reduced.col_upper[j] = 0.0;
                    changed = true;
                } else if max == lo {
                    reduced.col_lower[j] = 1.0;
                    changed = true;
                }
            }
        }
        if !changed && reduced.col_lower != reduced.col_upper {
            changed |= propagate_weighted(&mut reduced)?;
        }
        if !changed {
            break;
        }
    }
    Ok(reduced)
}

/// Conservative interval propagation. Rows with duplicate entries are skipped before
/// deriving bounds, and outward margins protect floating-point cancellation.
fn propagate_weighted(model: &mut Model) -> Result<bool, ()> {
    let mut changed = false;
    let mut terms = Vec::new();
    let mut unique: Vec<(usize, f64)> = Vec::new();
    let mut activities = Vec::new();
    for r in 0..model.num_row {
        terms.clear();
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            terms.push((model.a_index[k] as usize, model.a_value[k]));
        }
        terms.sort_unstable_by_key(|&(j, _)| j);
        if terms.windows(2).any(|w| w[0].0 == w[1].0) {
            continue;
        }
        unique.clear();
        for &(j, a) in &terms {
            if let Some(last) = unique.last_mut().filter(|last| last.0 == j) {
                last.1 += a;
            } else {
                unique.push((j, a));
            }
        }
        unique.retain(|&(_, a)| a != 0.0);
        activities.clear();
        let mut min = 0.0;
        let mut max = 0.0;
        let mut magnitude = 1.0;
        let mut min_inf = 0;
        let mut max_inf = 0;
        for &(j, a) in &unique {
            let (lo, hi) = if a > 0.0 {
                (model.col_lower[j], model.col_upper[j])
            } else {
                (model.col_upper[j], model.col_lower[j])
            };
            let l = a * lo;
            let u = a * hi;
            activities.push((l, u));
            if l.is_finite() && l.abs() < crate::model::INF {
                min += l;
                magnitude += l.abs();
            } else {
                min_inf += 1;
            }
            if u.is_finite() && u.abs() < crate::model::INF {
                max += u;
                magnitude += u.abs();
            } else {
                max_inf += 1;
            }
        }
        // LP files can contain rounded coefficients in redundant equalities.
        // Preserve points accepted at the solver's primal feasibility tolerance;
        // rounding error alone cannot cover their accumulated residuals.
        let error = 1e-7 + 64.0 * f64::EPSILON * (unique.len() + 1) as f64 * magnitude;
        if (min_inf == 0 && min - error > model.row_upper[r])
            || (max_inf == 0 && max + error < model.row_lower[r])
        {
            return Err(());
        }
        for (&(j, a), &(l, u)) in unique.iter().zip(&activities) {
            let lfinite = l.is_finite() && l.abs() < crate::model::INF;
            let ufinite = u.is_finite() && u.abs() < crate::model::INF;
            let mut lower = model.col_lower[j];
            let mut upper = model.col_upper[j];
            if min_inf == usize::from(!lfinite) && model.row_upper[r] < crate::model::INF {
                let rest = min - if lfinite { l } else { 0.0 };
                let bound = (model.row_upper[r] - rest + error) / a;
                if a > 0.0 {
                    upper = upper.min(bound.next_up());
                } else {
                    lower = lower.max(bound.next_down());
                }
            }
            if max_inf == usize::from(!ufinite) && model.row_lower[r] > -crate::model::INF {
                let rest = max - if ufinite { u } else { 0.0 };
                let bound = (model.row_lower[r] - rest - error) / a;
                if a > 0.0 {
                    lower = lower.max(bound.next_down());
                } else {
                    upper = upper.min(bound.next_up());
                }
            }
            if model.col_type[j].is_integral() {
                lower = lower.ceil();
                upper = upper.floor();
            }
            if lower > upper {
                return Err(());
            }
            // Ignore sub-tolerance continuous improvements to avoid long
            // asymptotic fixed-point passes; integer improvements remain exact.
            let tol = if model.col_type[j].is_integral() {
                0.0
            } else {
                1e-9 * (1.0 + lower.abs().min(upper.abs()))
            };
            if lower > model.col_lower[j] + tol {
                model.col_lower[j] = lower;
                changed = true;
            }
            if upper < model.col_upper[j] - tol {
                model.col_upper[j] = upper;
                changed = true;
            }
        }
    }
    Ok(changed)
}

/// A checked incumbent permits objective-domain bounds: every improving point
/// satisfies c*x <= incumbent. These finite bounds also prevent harmless dual
/// residuals on unbounded objective auxiliaries from destroying a lower bound.
fn objective_bounds(model: &mut Model, incumbent: f64) {
    let mut terms = vec![0.0; model.num_col];
    let mut min = 0.0;
    let mut magnitude = 1.0;
    for j in 0..model.num_col {
        let c = model.col_cost[j];
        if c == 0.0 {
            continue;
        }
        let b = if c > 0.0 {
            model.col_lower[j]
        } else {
            model.col_upper[j]
        };
        if b.abs() >= crate::model::INF || !b.is_finite() {
            return;
        }
        terms[j] = c * b;
        min += terms[j];
        magnitude += terms[j].abs();
    }
    let error = 64.0 * f64::EPSILON * (model.num_col + 1) as f64 * (magnitude + incumbent.abs());
    for j in 0..model.num_col {
        let c = model.col_cost[j];
        if c == 0.0 {
            continue;
        }
        let b = (incumbent - min + terms[j] + error) / c;
        if c > 0.0 {
            model.col_upper[j] = model.col_upper[j].min(b.next_up());
        } else {
            model.col_lower[j] = model.col_lower[j].max(b.next_down());
        }
    }
}

/// Promote integer definitions and objective auxiliaries whose favorable
/// rounding direction preserves feasibility. This preserves an optimal solution,
/// though an arbitrary feasible point may have fractional auxiliary values.
/// Applying it only to the B&B model preserves the planning heuristic's shape.
fn infer_integrality(model: &mut Model, original: &Model) {
    let mut rows = vec![Vec::new(); model.num_col];
    for r in 0..model.num_row {
        for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
            rows[model.a_index[k] as usize].push((r, model.a_value[k]));
        }
    }
    for _ in 0..8 {
        let mut changed = false;
        for j in 0..model.num_col {
            if model.col_type[j].is_integral()
                || rows[j].is_empty()
                || rows[j].windows(2).any(|w| w[0].0 == w[1].0)
            {
                continue;
            }
            let integer_rest = |r: usize| {
                (model.a_start[r] as usize..model.a_start[r + 1] as usize).all(|k| {
                    let c = model.a_index[k] as usize;
                    c == j
                        || (model.a_value[k].fract() == 0.0
                            && model.a_value[k].abs() <= 1e9
                            && (model.col_type[c].is_integral()
                                || (model.col_lower[c] == model.col_upper[c]
                                    && model.col_lower[c].fract() == 0.0)))
                })
            };
            // An equality with unit coefficient defines an integer value in
            // every feasible point, regardless of this column's other rows.
            let defined = rows[j].iter().any(|&(r, a)| {
                a.abs() == 1.0
                    && model.row_lower[r] == model.row_upper[r]
                    && model.row_lower[r].fract() == 0.0
                    && integer_rest(r)
            });
            let rounding_safe = |down: bool| {
                (if down {
                    original.col_lower[j].fract() == 0.0
                } else {
                    original.col_upper[j].fract() == 0.0
                }) && rows[j].iter().all(|&(r, a)| {
                    if a == 0.0 {
                        return true;
                    }
                    let delta = if down { -a } else { a };
                    let hurts_upper = delta > 0.0 && model.row_upper[r] < crate::model::INF;
                    let hurts_lower = delta < 0.0 && model.row_lower[r] > -crate::model::INF;
                    if !hurts_upper && !hurts_lower {
                        return true;
                    }
                    // Rounding a unit-coefficient variable toward an integer
                    // boundary remains feasible when the boundary is integral.
                    a.abs() == 1.0
                        && integer_rest(r)
                        && (!hurts_upper || model.row_upper[r].fract() == 0.0)
                        && (!hurts_lower || model.row_lower[r].fract() == 0.0)
                })
            };
            if defined
                || (model.col_cost[j] >= 0.0 && rounding_safe(true))
                || (model.col_cost[j] <= 0.0 && rounding_safe(false))
            {
                model.col_type[j] = crate::model::VarType::Integer;
                model.col_lower[j] = model.col_lower[j].ceil();
                model.col_upper[j] = model.col_upper[j].floor();
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// Small root-only integer rounding/cover cut pool. All coefficients used for
/// rounding are exact small integers; nonnegative domains make floor rounding
/// valid. Original rows and columns remain in place for witness checking.
pub fn strengthen(model: &Model, original: &Model, incumbent: Option<f64>) -> (Model, usize) {
    let mut out = model.clone();
    if let Some(inc) = incumbent {
        objective_bounds(&mut out, inc);
    }
    infer_integrality(&mut out, original);
    let mut count = 0;
    for r in 0..model.num_row {
        if count >= 32 {
            break;
        }
        let entries: Vec<(u32, f64)> = (model.a_start[r] as usize..model.a_start[r + 1] as usize)
            .map(|k| (model.a_index[k], model.a_value[k]))
            .collect();
        if entries.is_empty()
            || !entries.iter().all(|&(j, a)| {
                out.col_type[j as usize].is_integral()
                    && a.fract() == 0.0
                    && a.abs() <= 1e9
                    && a != 0.0
            })
        {
            continue;
        }
        let gcd = |mut a: u64, mut b: u64| {
            while b != 0 {
                let t = a % b;
                a = b;
                b = t;
            }
            a
        };
        let g = entries
            .iter()
            .fold(0u64, |g, &(_, a)| gcd(g, a.abs() as u64)) as f64;
        if out.row_lower[r] > -crate::model::INF {
            out.row_lower[r] = (out.row_lower[r] / g).next_down().ceil() * g;
        }
        if out.row_upper[r] < crate::model::INF {
            out.row_upper[r] = (out.row_upper[r] / g).next_up().floor() * g;
        }
        let rhs = model.row_upper[r];
        if rhs < 0.0
            || rhs >= crate::model::INF
            || !entries
                .iter()
                .all(|&(j, a)| a > 0.0 && model.col_lower[j as usize] >= 0.0)
        {
            continue;
        }
        let min_a = entries.iter().map(|&(_, a)| a as u64).min().unwrap();
        for divisor in [2u64, min_a.max(2)] {
            let row: Vec<_> = entries
                .iter()
                .filter_map(|&(j, a)| {
                    let v = (a as u64 / divisor) as f64;
                    if v > 0.0 {
                        Some((j, v))
                    } else {
                        None
                    }
                })
                .collect();
            let bound = (rhs / divisor as f64).next_up().floor();
            if row.is_empty() || entries.iter().all(|&(_, a)| a as u64 % divisor == 0) {
                continue;
            }
            out.add_row(-crate::model::INF, bound, &row);
            count += 1;
        }
        if entries
            .iter()
            .all(|&(j, _)| model.col_upper[j as usize] <= 1.0)
        {
            let mut cover = entries.clone();
            cover.sort_by(|a, b| b.1.total_cmp(&a.1));
            let mut weight = 0.0;
            let mut row = Vec::new();
            for (j, a) in cover {
                weight += a;
                row.push((j, 1.0));
                if weight > rhs {
                    break;
                }
            }
            let mut ids: Vec<_> = row.iter().map(|&(j, _)| j).collect();
            ids.sort_unstable();
            ids.dedup();
            if weight > rhs && ids.len() == row.len() {
                out.add_row(-crate::model::INF, row.len() as f64 - 1.0, &row);
                count += 1;
            }
        }
    }
    (out, count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StdClock, VarType, INF};
    #[test]
    fn integer_load_auxiliaries_preserve_the_original_optimum() {
        let mut m = Model::new();
        let x = m.add_col(0.0, 3.0);
        let y = m.add_col(0.0, 3.0);
        m.set_integrality(x, VarType::Integer);
        m.set_integrality(y, VarType::Integer);
        let peak = m.add_col(0.0, INF);
        let min = m.add_col(0.0, INF);
        let range = m.add_col(0.0, INF);
        let total = m.add_col(0.0, INF);
        let dev = m.add_col(0.0, INF);
        m.set_cost(peak, 0.05);
        m.set_cost(range, 1.0);
        m.set_cost(dev, 0.01);
        m.add_row(3.0, 3.0, &[(x.0, 1.0), (y.0, 1.0)]);
        m.add_row(-INF, 0.0, &[(x.0, 2.0), (peak.0, -1.0)]);
        m.add_row(-INF, 0.0, &[(y.0, 3.0), (peak.0, -1.0)]);
        m.add_row(0.0, INF, &[(x.0, 2.0), (min.0, -1.0)]);
        m.add_row(0.0, INF, &[(y.0, 3.0), (min.0, -1.0)]);
        m.add_row(0.0, INF, &[(range.0, 1.0), (peak.0, -1.0), (min.0, 1.0)]);
        m.add_row(0.0, 0.0, &[(total.0, 1.0), (x.0, -2.0), (y.0, -3.0)]);
        m.add_row(-INF, 0.0, &[(x.0, 4.0), (total.0, -1.0), (dev.0, -1.0)]);
        m.add_row(-INF, 0.0, &[(x.0, -4.0), (total.0, 1.0), (dev.0, -1.0)]);
        let (cut, _) = strengthen(&m, &m, None);
        assert!(cut.col_type.iter().all(|t| t.is_integral()));
        let s = crate::solve(
            &m,
            &crate::Options {
                time_limit_secs: 3.0,
                mip_rel_gap: 0.0,
                ..Default::default()
            },
            &StdClock::new(),
        );
        assert_eq!(s.status, crate::ModelStatus::Optimal, "{s:?}");
        assert!((s.objective - 1.21).abs() < 1e-6);
        assert!(crate::model::check_solution(&m, &s.col_value, 1e-6).feasible);
    }

    #[test]
    fn weighted_propagation_preserves_feasible_mixed_domain_witnesses() {
        let mut seed = 41u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 32) as u32
        };
        for _ in 0..100 {
            let mut m = Model::new();
            let mut x = Vec::new();
            for j in 0..5 {
                let v = m.add_col(-10.0, 10.0);
                if j < 3 {
                    m.set_integrality(v, VarType::Integer);
                    x.push((rnd() % 7) as f64 - 3.0);
                } else {
                    x.push((rnd() % 70) as f64 / 10.0 - 3.0);
                }
            }
            for _ in 0..5 {
                let row: Vec<_> = (0..5)
                    .map(|j| (j, (rnd() % 31) as f64 / 10.0 - 1.5))
                    .collect();
                let activity: f64 = row.iter().map(|&(j, a)| a * x[j as usize]).sum();
                m.add_row(activity - 0.01, activity + 0.01, &row);
            }
            let reduced = propagate(&m, &StdClock::new(), 2.0)
                .expect("feasible witness cannot become infeasible");
            assert!(crate::model::check_solution(&reduced, &x, 1e-9).feasible);
        }
    }

    #[test]
    fn integer_cuts_preserve_every_feasible_binary_assignment() {
        let mut m = Model::new();
        for _ in 0..5 {
            let v = m.add_col(0.0, 1.0);
            m.set_integrality(v, VarType::Integer);
        }
        m.add_row(
            -INF,
            13.5,
            &[(0, 7.0), (1, 6.0), (2, 5.0), (3, 4.0), (4, 3.0)],
        );
        let (cut, n) = strengthen(&m, &m, None);
        assert!(n > 0);
        for mask in 0..32 {
            let x: Vec<_> = (0..5).map(|j| ((mask >> j) & 1) as f64).collect();
            if crate::model::check_solution(&m, &x, 1e-9).feasible {
                assert!(crate::model::check_solution(&cut, &x, 1e-9).feasible);
            }
        }
    }

    #[test]
    fn cascades_and_detects_contradiction() {
        let mut m = Model::new();
        let a = m.add_col(1.0, 1.0);
        let b = m.add_col(0.0, 1.0);
        let c = m.add_col(0.0, 1.0);
        for j in [a, b, c] {
            m.set_integrality(j, VarType::Integer);
        }
        m.add_row(1.0, 1.0, &[(b.0, 1.0), (c.0, 1.0)]);
        m.add_row(1.0, 1.0, &[(a.0, 1.0), (b.0, 1.0)]);
        let p = propagate(&m, &StdClock::new(), 10.0).unwrap();
        assert_eq!(p.col_lower, vec![1.0, 0.0, 1.0]);
        assert_eq!(p.col_upper, p.col_lower);
        m.add_row(-INF, 0.0, &[(c.0, 1.0)]);
        assert!(propagate(&m, &StdClock::new(), 10.0).is_err());
    }
    #[test]
    fn duplicate_and_nonunit_entries_are_not_cardinality_rows() {
        let mut m = Model::new();
        let a = m.add_col(0.0, 1.0);
        m.set_integrality(a, VarType::Integer);
        m.add_row(1.0, 1.0, &[(a.0, 1.0), (a.0, 1.0)]);
        let p = propagate(&m, &StdClock::new(), 10.0).unwrap();
        assert_eq!(p.col_lower, vec![0.0]);
        assert_eq!(p.col_upper, vec![1.0]);
    }
}

#[cfg(test)]
mod weighted_tolerance_regression {
    use super::*;
    use crate::solve::StdClock;
    #[test]
    fn rounded_redundant_equalities_do_not_claim_infeasible() {
        let mut m = Model::new();
        let x = m.add_col(0.0, 1000.0);
        let y = m.add_col(0.0, 1000.0);
        m.add_row(222.3403333300, 222.3403333300, &[(x.0, 1.0)]);
        m.add_row(222.3403333310, 222.3403333310, &[(y.0, 1.0)]);
        m.add_row(0.0, 0.0, &[(x.0, 1.0), (y.0, -1.0)]);
        assert!(propagate(&m, &StdClock::new(), 10.0).is_ok());
    }
}

#[cfg(test)]
mod node_propagation_tests {
    use super::*;
    use crate::{VarType, INF};

    struct FixedClock(f64);
    impl Clock for FixedClock {
        fn now_secs(&self) -> f64 {
            self.0
        }
    }

    #[test]
    fn seeded_normalization_uses_propagated_parent_and_full_run_checks_all_domains() {
        let mut m = Model::new();
        for (lo, hi) in [(0.0, 5.0), (0.25, 2.75)] {
            let j = m.add_col(lo, hi);
            m.set_integrality(j, VarType::Integer);
        }
        let mut p = PreparedPropagation::new(&m);
        let mut lo = m.col_lower.clone();
        let mut hi = m.col_upper.clone();
        assert_eq!(
            p.run(&mut lo, &mut hi, None, &FixedClock(0.0), 1.0),
            PropagationResult::Complete { changed: true }
        );
        assert_eq!(lo, vec![0.0, 1.0]);
        assert_eq!(hi, vec![5.0, 2.0]);
        // Only the branch domain changed after complete parent propagation.
        lo[0] = 1.2;
        hi[0] = 2.8;
        assert_eq!(
            p.run(&mut lo, &mut hi, Some(0), &FixedClock(0.0), 1.0),
            PropagationResult::Complete { changed: true }
        );
        assert_eq!(lo, vec![2.0, 1.0]);
        assert_eq!(hi, vec![2.0, 2.0]);
        lo[0] = 2.1;
        assert_eq!(
            p.run(&mut lo, &mut hi, Some(0), &FixedClock(0.0), 1.0),
            PropagationResult::Infeasible
        );
        // Arbitrary/re-entry domains must use None, detecting contradictions
        // even on columns disconnected from the branch/constraint matrix.
        lo = vec![0.0, 3.0];
        hi = vec![5.0, 2.0];
        assert_eq!(
            p.run(&mut lo, &mut hi, None, &FixedClock(0.0), 1.0),
            PropagationResult::Infeasible
        );
    }

    #[test]
    fn single_sided_rows_avoid_self_rescans_but_revisit_changed_neighbors() {
        let mut m = Model::new();
        for hi in [5.0, 5.0, 1.0] {
            let j = m.add_col(0.0, hi);
            m.set_integrality(j, VarType::Integer);
        }
        m.add_row(-crate::model::INF, 1.0, &[(0, 1.0), (1, 1.0)]);
        m.add_row(2.0, crate::model::INF, &[(1, 1.0), (2, 1.0)]);
        let mut p = PreparedPropagation::new(&m);
        let mut lo = m.col_lower.clone();
        let mut hi = m.col_upper.clone();
        assert_eq!(
            p.run(&mut lo, &mut hi, None, &FixedClock(0.0), 1.0),
            PropagationResult::Complete { changed: true }
        );
        assert_eq!(lo, vec![0.0, 1.0, 1.0]);
        assert_eq!(hi, vec![0.0, 1.0, 1.0]);
        assert_eq!(p.row_visits, 3);
    }

    #[test]
    fn signed_single_sided_rows_skip_self_but_two_sided_rows_keep_self() {
        for (row_lo, row_hi, coefficient, expected_lo, expected_hi, visits) in [
            (-INF, 1.0, 1.0, 0.0, 1.0, 1),
            (-INF, -1.0, -1.0, 1.0, 5.0, 1),
            (1.0, INF, 1.0, 1.0, 5.0, 1),
            (-1.0, INF, -1.0, 0.0, 1.0, 1),
            (1.0, 1.0, 1.0, 1.0, 1.0, 2),
        ] {
            let mut m = Model::new();
            let j = m.add_col(0.0, 5.0);
            m.set_integrality(j, VarType::Integer);
            m.add_row(row_lo, row_hi, &[(0, coefficient)]);
            let mut p = PreparedPropagation::new(&m);
            let mut lo = m.col_lower.clone();
            let mut hi = m.col_upper.clone();
            assert_eq!(
                p.run(&mut lo, &mut hi, None, &FixedClock(0.0), 1.0),
                PropagationResult::Complete { changed: true }
            );
            assert_eq!(lo, vec![expected_lo]);
            assert_eq!(hi, vec![expected_hi]);
            assert_eq!(p.row_visits, visits);
        }
    }

    #[test]
    fn exhaustive_signed_mixed_domains_preserve_all_feasible_witnesses() {
        // Every combination of positive, negative and fractional coefficients,
        // with integral x,y and continuous z. Enumerate all grid witnesses and
        // all x branches independently, including infeasible branch domains.
        for a in [-3.0, -0.5, 0.5, 2.0] {
            for b in [-2.0, -0.25, 0.25, 3.0] {
                for rhs in [-2.0, 0.0, 2.0] {
                    let mut m = Model::new();
                    let x = m.add_col(-2.0, 2.0);
                    let y = m.add_col(-2.0, 2.0);
                    let z = m.add_col(-1.0, 1.0);
                    m.set_integrality(x, VarType::Integer);
                    m.set_integrality(y, VarType::Integer);
                    m.add_row(rhs - 0.75, rhs + 0.75, &[(x.0, a), (y.0, b), (z.0, -1.25)]);
                    m.add_row(-INF, 1.5, &[(y.0, -0.75), (z.0, 2.0)]);
                    let mut prepared = PreparedPropagation::new(&m);
                    for branch in -2..=2 {
                        let mut lo = m.col_lower.clone();
                        let mut hi = m.col_upper.clone();
                        lo[0] = branch as f64;
                        hi[0] = branch as f64;
                        let result = prepared.run(&mut lo, &mut hi, None, &FixedClock(0.0), 1.0);
                        for yv in -2..=2 {
                            for zi in -4..=4 {
                                let p = [branch as f64, yv as f64, zi as f64 / 4.0];
                                if crate::model::check_solution(&m, &p, 1e-9).feasible {
                                    assert_ne!(result, PropagationResult::Infeasible);
                                    for j in 0..3 {
                                        assert!(
                                            p[j] >= lo[j] && p[j] <= hi[j],
                                            "removed {p:?}, {lo:?}..{hi:?}"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn incremental_chain_detects_branch_contradiction_and_reuses_scratch() {
        let mut m = Model::new();
        for _ in 0..3 {
            let c = m.add_col(0.0, 1.0);
            m.set_integrality(c, VarType::Integer);
        }
        m.add_row(0.0, 0.0, &[(0, 1.0), (1, -1.0)]);
        m.add_row(0.0, 0.0, &[(1, 1.0), (2, -1.0)]);
        let mut prepared = PreparedPropagation::new(&m);
        let mut lo = m.col_lower.clone();
        let mut hi = m.col_upper.clone();
        lo[0] = 1.0;
        assert_eq!(
            prepared.run(&mut lo, &mut hi, Some(0), &FixedClock(0.0), 1.0),
            PropagationResult::Complete { changed: true }
        );
        assert_eq!(lo, vec![1.0; 3]);
        lo = m.col_lower.clone();
        hi = m.col_upper.clone();
        lo[0] = 1.0;
        hi[2] = 0.0;
        assert_eq!(
            prepared.run(&mut lo, &mut hi, Some(0), &FixedClock(0.0), 1.0),
            PropagationResult::Infeasible
        );
    }

    #[test]
    fn unbounded_small_coefficients_and_duplicates_are_conservative() {
        let mut m = Model::new();
        m.add_col(-INF, INF);
        m.add_col(0.0, 1.0);
        m.add_row(0.0, 0.0, &[(0, 1e-35), (1, 1.0)]);
        // A duplicate row may be feasible despite separately treating each
        // occurrence as the same changing variable during propagation.
        m.add_row(-INF, 0.0, &[(1, 1.0), (1, -1.0)]);
        let mut p = PreparedPropagation::new(&m);
        let mut lo = m.col_lower.clone();
        let mut hi = m.col_upper.clone();
        assert_ne!(
            p.run(&mut lo, &mut hi, None, &FixedClock(0.0), 1.0),
            PropagationResult::Infeasible
        );
        assert_eq!(hi[1], 1.0);
    }

    #[test]
    fn deadline_is_distinct_from_infeasibility_and_reentry_is_safe() {
        let mut m = Model::new();
        let x = m.add_col(0.0, 1.0);
        m.set_integrality(x, VarType::Integer);
        m.add_row(2.0, INF, &[(0, 1.0)]);
        let mut p = PreparedPropagation::new(&m);
        let mut lo = m.col_lower.clone();
        let mut hi = m.col_upper.clone();
        assert_eq!(
            p.run(&mut lo, &mut hi, None, &FixedClock(2.0), 1.0),
            PropagationResult::TimedOut { changed: false }
        );
        assert_eq!(
            p.run(&mut lo, &mut hi, None, &FixedClock(0.0), 1.0),
            PropagationResult::Infeasible
        );
    }

    #[test]
    fn partial_timeout_retains_safe_tightenings() {
        struct SteppingClock(std::sync::atomic::AtomicUsize);
        impl Clock for SteppingClock {
            fn now_secs(&self) -> f64 {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as f64
            }
        }
        let mut m = Model::new();
        let x = m.add_col(0.1, 2.9);
        m.set_integrality(x, VarType::Integer);
        m.add_row(0.0, INF, &[(0, 1.0)]);
        let mut p = PreparedPropagation::new(&m);
        let mut lo = m.col_lower.clone();
        let mut hi = m.col_upper.clone();
        let clock = SteppingClock(std::sync::atomic::AtomicUsize::new(0));
        assert_eq!(
            p.run(&mut lo, &mut hi, None, &clock, 2.0),
            PropagationResult::TimedOut { changed: true }
        );
        assert_eq!(lo, vec![1.0]);
        assert_eq!(hi, vec![2.0]);
        assert_eq!(
            p.run(&mut lo, &mut hi, None, &FixedClock(0.0), 1.0),
            PropagationResult::Complete { changed: false }
        );
    }
}
