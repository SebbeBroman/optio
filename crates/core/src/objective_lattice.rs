//! A finite superset of integer objective values, for rounding certified bounds.
//! Group-count domains ignore coupling rows, so rounding cannot exclude an
//! attainable original objective. Row-derived caps only drop nonnegative terms.
use crate::model::{Model, INF};
use std::collections::BTreeMap;

const EXACT_MAX: i128 = 1i128 << 53;
const MAX_RANGE: usize = 1_000_000;
const MAX_GROUPS: usize = 32;

pub(crate) struct ObjectiveLattice {
    values: Vec<f64>,
}

struct Group {
    cost: i128,
    columns: usize,
    lower: i128,
    upper: Option<i128>,
}

fn exact_integer(x: f64) -> Option<i128> {
    (x.is_finite() && x.abs() <= EXACT_MAX as f64 && x.fract() == 0.0).then_some(x as i128)
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl ObjectiveLattice {
    pub(crate) fn build(model: &Model) -> Option<Self> {
        let mut groups: Vec<Group> = Vec::new();
        let mut by_cost = BTreeMap::new();
        let mut column_group = vec![None; model.num_col];
        for j in 0..model.num_col {
            let cost = model.col_cost[j];
            if !cost.is_finite() {
                return None;
            }
            if cost == 0.0 {
                continue;
            }
            let cost = exact_integer(cost)?;
            if cost.abs() > 1_000_000_000 || !model.col_type[j].is_integral() {
                return None;
            }
            let lower = model.col_lower[j];
            let upper = model.col_upper[j];
            if !lower.is_finite() || lower < 0.0 || lower >= INF || upper.is_nan() || upper < lower
            {
                return None;
            }
            let lower = exact_integer(lower.ceil())?;
            let upper = if upper.is_finite() && upper < INF {
                Some(exact_integer(upper.floor())?)
            } else {
                None
            };
            if upper.is_some_and(|hi| hi < lower) {
                return None;
            }
            let g = *by_cost.entry(cost).or_insert_with(|| {
                groups.push(Group {
                    cost,
                    columns: 0,
                    lower: 0,
                    upper: Some(0),
                });
                groups.len() - 1
            });
            let group = &mut groups[g];
            group.columns += 1;
            group.lower += lower;
            group.upper = group.upper.zip(upper).map(|(a, b)| a + b);
            column_group[j] = Some(g);
        }
        if groups.is_empty() || groups.len() > MAX_GROUPS {
            return None;
        }

        let mut seen = vec![usize::MAX; model.num_col];
        let mut counts = vec![0usize; groups.len()];
        let mut minimum = vec![i128::MAX; groups.len()];
        for r in 0..model.num_row {
            let rhs = model.row_upper[r];
            if !rhs.is_finite() || rhs < 0.0 || rhs >= INF || rhs > EXACT_MAX as f64 {
                continue;
            }
            counts.fill(0);
            minimum.fill(i128::MAX);
            let mut valid = true;
            for k in model.a_start[r] as usize..model.a_start[r + 1] as usize {
                let j = model.a_index[k] as usize;
                let a = model.a_value[k];
                if seen[j] == r {
                    valid = false;
                }
                seen[j] = r;
                if a == 0.0 {
                    continue;
                }
                // Other groups and continuous auxiliaries may be discarded
                // only when their entire contribution is nonnegative.
                if !a.is_finite() || a < 0.0 || !(model.col_lower[j] >= 0.0) {
                    valid = false;
                    continue;
                }
                if let Some(g) = column_group[j] {
                    if let Some(a) = exact_integer(a).filter(|&a| a > 0) {
                        counts[g] += 1;
                        minimum[g] = minimum[g].min(a);
                    } else {
                        valid = false;
                    }
                }
            }
            if !valid {
                continue;
            }
            // The left-hand group contribution is integral. An outward margin
            // also retains objectives of numerically feasible edge witnesses.
            // Loosen the cap at numerical row edges; this remains a superset
            // of exact feasible counts and preserves tightly checked starts.
            let rhs = (rhs.next_up() + 1e-9).floor() as i128;
            for (g, group) in groups.iter_mut().enumerate() {
                if counts[g] == group.columns {
                    let upper = rhs / minimum[g];
                    group.upper = Some(group.upper.map_or(upper, |old| old.min(upper)));
                }
            }
        }

        let unit = groups.iter().fold(0, |u, g| gcd(u, g.cost.abs()));
        let mut base = 0i128;
        let mut range = 0i128;
        let mut absolute_objective = 0i128;
        for group in &groups {
            let upper = group.upper?;
            if upper < group.lower || upper > EXACT_MAX || group.lower > EXACT_MAX {
                return None;
            }
            absolute_objective += group.cost.abs() * upper;
            base += group.cost * if group.cost < 0 { upper } else { group.lower };
            range += group.cost.abs() / unit * (upper - group.lower);
        }
        // Bound every floating product and partial sum, including cancellation.
        if absolute_objective > EXACT_MAX
            || range > MAX_RANGE as i128
            || base.abs() > EXACT_MAX
            || (base + range * unit).abs() > EXACT_MAX
        {
            return None;
        }
        let range = range as usize;
        let mut reachable = vec![0u8; range + 1];
        let mut next = vec![0u8; range + 1];
        reachable[0] = 1;
        // Bounded-knapsack sliding windows, one per residue modulo group step.
        // This is O(groups * range), independent of domain cardinalities.
        for group in groups {
            let copies = (group.upper? - group.lower) as usize;
            if copies == 0 {
                continue;
            }
            let step = (group.cost.abs() / unit) as usize;
            next.fill(0);
            for residue in 0..step.min(range + 1) {
                let mut window = 0usize;
                for position in (residue..=range).step_by(step) {
                    window += reachable[position] as usize;
                    if position / step > copies {
                        window -= reachable[position - (copies + 1) * step] as usize;
                    }
                    next[position] = u8::from(window > 0);
                }
            }
            std::mem::swap(&mut reachable, &mut next);
        }
        let values = reachable
            .into_iter()
            .enumerate()
            .filter(|&(_, present)| present != 0)
            .map(|(i, _)| (base + i as i128 * unit) as f64)
            .collect();
        Some(Self { values })
    }

    /// Only accepts an already certified lower bound. A one-ULP downward
    /// target avoids jumping past an objective exactly on a floating edge.
    pub(crate) fn round_up(&self, bound: f64) -> f64 {
        if !bound.is_finite() {
            return bound;
        }
        let target = bound.next_down();
        let i = self.values.partition_point(|&value| value < target);
        self.values.get(i).map_or(bound, |&value| bound.max(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{check_solution, VarType};

    #[test]
    fn arbitrary_mixed_cost_lattice_never_excludes_an_integer_objective() {
        let mut m = Model::new();
        for (lo, hi, cost) in [(0.2, 3.8, -7.0), (0.0, 2.0, -7.0), (1.0, 4.0, 11.0)] {
            let j = m.add_col(lo, hi);
            m.set_integrality(j, VarType::Integer);
            m.set_cost(j, cost);
        }
        let lattice = ObjectiveLattice::build(&m).unwrap();
        for a in 1..=3 {
            for b in 0..=2 {
                for c in 1..=4 {
                    let objective = m.objective(&[a as f64, b as f64, c as f64]);
                    for fraction in [0.0, 0.1, 0.5, 1.0, 20.0] {
                        let bound = objective - fraction;
                        let rounded = lattice.round_up(bound);
                        assert!(rounded >= bound);
                        assert!(rounded <= objective);
                    }
                }
            }
        }
    }

    #[test]
    fn complete_cover_rows_bound_unbounded_domains_and_allow_continuous_terms() {
        let mut m = Model::new();
        for cost in [-19.0, -19.0, -7.0, -7.0] {
            let j = m.add_col(0.0, INF);
            m.set_integrality(j, VarType::Integer);
            m.set_cost(j, cost);
        }
        m.add_col(0.0, INF);
        m.add_row(-INF, 6.5, &[(0, 2.0), (1, 3.0), (4, 0.25)]);
        m.add_row(-INF, 2.0, &[(2, 1.0), (3, 1.0)]);
        let lattice = ObjectiveLattice::build(&m).unwrap();
        assert_eq!(lattice.round_up(-45.5), -45.0);
        for a in 0..=3 {
            for b in 0..=2 {
                for c in 0..=2 {
                    for d in 0..=2 {
                        let point = [a as f64, b as f64, c as f64, d as f64, 0.0];
                        if check_solution(&m, &point, 1e-9).feasible {
                            let objective = m.objective(&point);
                            assert!(lattice.round_up(objective - 0.5) <= objective);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn fractional_costs_continuous_objectives_and_large_ranges_are_rejected() {
        let mut m = Model::new();
        let j = m.add_col(0.0, 1.0);
        m.set_cost(j, 1.0);
        assert!(ObjectiveLattice::build(&m).is_none());
        m.set_integrality(j, VarType::Integer);
        m.set_cost(j, 1.5);
        assert!(ObjectiveLattice::build(&m).is_none());
        m.set_cost(j, 1.0);
        m.col_upper[0] = 2_000_000.0;
        assert!(ObjectiveLattice::build(&m).is_none());
        m.col_upper[0] = 1e20;
        assert!(ObjectiveLattice::build(&m).is_none());
    }

    #[test]
    fn incomplete_duplicate_and_negative_cover_rows_cannot_establish_caps() {
        for row in [
            vec![(0, 1.0)],
            vec![(0, 1.0), (0, 1.0), (1, 1.0)],
            vec![(0, 1.0), (1, -1.0)],
        ] {
            let mut m = Model::new();
            for _ in 0..2 {
                let j = m.add_col(0.0, INF);
                m.set_integrality(j, VarType::Integer);
                m.set_cost(j, -3.0);
            }
            m.add_row(-INF, 3.0, &row);
            assert!(ObjectiveLattice::build(&m).is_none());
        }
    }

    #[test]
    fn large_cancelling_terms_are_rejected() {
        let mut m = Model::new();
        for (value, cost) in [
            (1_000_000_000.0, 999_999_937.0),
            (1_000_000_001.0, -999_999_936.0),
        ] {
            let j = m.add_col(value, value);
            m.set_integrality(j, VarType::Integer);
            m.set_cost(j, cost);
        }
        assert!(ObjectiveLattice::build(&m).is_none());
    }

    #[test]
    fn rounding_handles_exact_edges_and_outside_range_conservatively() {
        let mut m = Model::new();
        let j = m.add_col(0.0, 3.0);
        m.set_integrality(j, VarType::Integer);
        m.set_cost(j, -7.0);
        let lattice = ObjectiveLattice::build(&m).unwrap();
        assert_eq!(lattice.round_up(-14.0), -14.0);
        assert_eq!(lattice.round_up((-14.0f64).next_up()), (-14.0f64).next_up());
        assert_eq!(lattice.round_up(-13.9), -7.0);
        assert_eq!(lattice.round_up(1.0), 1.0);
        assert_eq!(lattice.round_up(f64::NEG_INFINITY), f64::NEG_INFINITY);
    }
}
