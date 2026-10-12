//! Exact sum-variable aggregation of columns with identical coefficients/costs.
//! No row or objective term is dropped. Original witnesses are always checked
//! after disaggregation; the compressed domains are conservative intervals.
use crate::model::{check_solution, Model, VarType, INF};
use std::collections::HashMap;

pub(crate) struct Compression<'a> {
    original: &'a Model,
    pub(crate) reduced_model: Model,
    groups: Vec<Vec<usize>>,
}

impl<'a> Compression<'a> {
    pub(crate) fn build(original: &'a Model) -> Option<Self> {
        let mut columns = vec![Vec::<(u32, u64)>::new(); original.num_col];
        let mut seen = vec![usize::MAX; original.num_col];
        for r in 0..original.num_row {
            for k in original.a_start[r] as usize..original.a_start[r + 1] as usize {
                let j = original.a_index[k] as usize;
                let a = original.a_value[k];
                // Duplicate CSR entries cannot safely be represented by taking
                // one entry per compressed group.
                if seen[j] == r || !a.is_finite() {
                    return None;
                }
                seen[j] = r;
                if a != 0.0 {
                    columns[j].push((r as u32, a.to_bits()));
                }
            }
        }
        let mut keys = HashMap::new();
        let mut groups: Vec<Vec<usize>> = Vec::new();
        let mut group_of = vec![0; original.num_col];
        for j in 0..original.num_col {
            let lo = original.col_lower[j];
            let hi = original.col_upper[j];
            if lo.is_nan() || hi.is_nan() || lo > hi || !original.col_cost[j].is_finite() {
                return None;
            }
            if original.col_type[j].is_integral() && lo.ceil() > hi.floor() {
                return None;
            }
            let finite = lo.is_finite() && hi.is_finite() && lo.abs() < INF && hi.abs() < INF;
            let integral_safe = !original.col_type[j].is_integral()
                || lo.abs().max(hi.abs()) <= (1u64 << 53) as f64;
            let g = if finite && integral_safe {
                let key = (
                    original.col_type[j] as u8,
                    original.col_cost[j].to_bits(),
                    std::mem::take(&mut columns[j]),
                );
                *keys.entry(key).or_insert_with(|| {
                    groups.push(Vec::new());
                    groups.len() - 1
                })
            } else {
                groups.push(Vec::new());
                groups.len() - 1
            };
            group_of[j] = g;
            groups[g].push(j);
        }
        // Avoid imposing a full solve/lift path for insignificant compression.
        let removed = original.num_col.saturating_sub(groups.len());
        if removed == 0 || (original.num_col >= 32 && removed < 8) {
            return None;
        }
        let mut reduced = Model::new();
        for group in &groups {
            let first = group[0];
            let integral = original.col_type[first].is_integral();
            let (lo, hi) = if group.len() == 1 {
                (original.col_lower[first], original.col_upper[first])
            } else if integral {
                let lo: i128 = group
                    .iter()
                    .map(|&j| original.col_lower[j].ceil() as i128)
                    .sum();
                let hi: i128 = group
                    .iter()
                    .map(|&j| original.col_upper[j].floor() as i128)
                    .sum();
                if lo.abs().max(hi.abs()) > (1i128 << 53) {
                    return None;
                }
                (lo as f64, hi as f64)
            } else {
                let mut lo = 0.0f64;
                let mut hi = 0.0f64;
                for &j in group {
                    lo = (lo + original.col_lower[j]).next_down();
                    hi = (hi + original.col_upper[j]).next_up();
                }
                if !lo.is_finite() || !hi.is_finite() || lo.abs().max(hi.abs()) >= INF {
                    return None;
                }
                (lo, hi)
            };
            let c = reduced.add_col(lo, hi);
            reduced.col_cost[c.0 as usize] = original.col_cost[first];
            reduced.set_integrality(
                c,
                if group.len() > 1 && integral {
                    VarType::Integer
                } else {
                    original.col_type[first]
                },
            );
        }
        let mut group_seen = vec![usize::MAX; groups.len()];
        let mut row = Vec::new();
        for r in 0..original.num_row {
            row.clear();
            for k in original.a_start[r] as usize..original.a_start[r + 1] as usize {
                let g = group_of[original.a_index[k] as usize];
                let a = original.a_value[k];
                if a != 0.0 && group_seen[g] != r {
                    row.push((g as u32, a));
                    group_seen[g] = r;
                }
            }
            reduced.add_row(original.row_lower[r], original.row_upper[r], &row);
        }
        Some(Self {
            original,
            reduced_model: reduced,
            groups,
        })
    }

    pub(crate) fn project_start(&self, point: &[f64]) -> Option<Vec<f64>> {
        if point.len() != self.original.num_col
            || !check_solution(self.original, point, 1e-9).feasible
        {
            return None;
        }
        let projected: Vec<f64> = self
            .groups
            .iter()
            .map(|g| g.iter().map(|&j| point[j]).sum())
            .collect();
        if !check_solution(&self.reduced_model, &projected, 1e-9).feasible {
            return None;
        }
        self.same_objective(point, &projected).then_some(projected)
    }

    pub(crate) fn lift(&self, point: &[f64]) -> Option<Vec<f64>> {
        if point.len() != self.groups.len()
            || !check_solution(&self.reduced_model, point, 1e-9).feasible
        {
            return None;
        }
        let mut lifted = vec![0.0; self.original.num_col];
        for (g, group) in self.groups.iter().enumerate() {
            if group.len() == 1 {
                lifted[group[0]] = point[g];
                continue;
            }
            let integral = self.original.col_type[group[0]].is_integral();
            if integral {
                // Work in exact integer arithmetic throughout disaggregation.
                let mut remaining = point[g].round() as i128;
                for &j in group {
                    let lo = self.original.col_lower[j].ceil() as i128;
                    lifted[j] = lo as f64;
                    remaining -= lo;
                }
                if remaining < 0 {
                    return None;
                }
                for &j in group {
                    let room = self.original.col_upper[j].floor() as i128 - lifted[j] as i128;
                    let delta = remaining.min(room);
                    lifted[j] = (lifted[j] as i128 + delta) as f64;
                    remaining -= delta;
                }
                if remaining != 0 {
                    return None;
                }
            } else {
                let mut remaining = point[g];
                for &j in group {
                    lifted[j] = self.original.col_lower[j];
                    remaining -= lifted[j];
                }
                // Continuous outward bounds can admit a tiny unattainable edge;
                // validation below rejects any resulting original violation.
                for &j in group {
                    let room = self.original.col_upper[j] - lifted[j];
                    let delta = remaining.max(0.0).min(room);
                    lifted[j] += delta;
                    remaining -= delta;
                }
            }
        }
        if !check_solution(self.original, &lifted, 1e-9).feasible {
            return None;
        }
        self.same_objective(&lifted, point).then_some(lifted)
    }

    fn same_objective(&self, original: &[f64], reduced: &[f64]) -> bool {
        (self.original.objective(original) - self.reduced_model.objective(reduced)).abs() <= 1e-9
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixed() -> Model {
        let mut m = Model::new();
        for (lo, hi, ty, cost) in [
            (-1.2, 2.8, VarType::Integer, 3.0),
            (0.1, 2.9, VarType::Integer, 3.0),
            (-0.5, 1.0, VarType::Continuous, -2.0),
            (0.0, 1.5, VarType::Continuous, -2.0),
        ] {
            let c = m.add_col(lo, hi);
            m.set_integrality(c, ty);
            m.set_cost(c, cost);
        }
        m.add_row(-2.0, 4.0, &[(0, 2.0), (1, 2.0), (2, -1.0), (3, -1.0)]);
        m.add_row(-INF, 6.0, &[(0, -1.0), (1, -1.0), (2, 2.0), (3, 2.0)]);
        m
    }

    #[test]
    fn exhaustive_mixed_projection_and_lifting_preserve_feasibility_and_objective() {
        let m = mixed();
        let c = Compression::build(&m).unwrap();
        assert_eq!(c.reduced_model.num_col, 2);
        for x in -1..=2 {
            for y in 1..=2 {
                for zi in -2..=4 {
                    for wi in 0..=6 {
                        let original = [x as f64, y as f64, zi as f64 / 4.0, wi as f64 / 4.0];
                        if check_solution(&m, &original, 1e-9).feasible {
                            let projected = c.project_start(&original).unwrap();
                            let lifted = c.lift(&projected).unwrap();
                            assert!(check_solution(&m, &lifted, 1e-9).feasible);
                            assert_eq!(m.objective(&original), m.objective(&lifted));
                        }
                    }
                }
            }
        }
        for sum in 0..=4 {
            for zi in -2..=10 {
                let reduced = [sum as f64, zi as f64 / 4.0];
                if check_solution(&c.reduced_model, &reduced, 1e-9).feasible {
                    let lifted = c.lift(&reduced).unwrap();
                    assert_eq!(m.objective(&lifted), c.reduced_model.objective(&reduced));
                }
            }
        }
    }

    #[test]
    fn unequal_cost_coefficients_and_types_do_not_merge() {
        let mut m = Model::new();
        for _ in 0..4 {
            m.add_col(0.0, 1.0);
        }
        m.col_cost[1] = 1.0;
        m.col_type[3] = VarType::Integer;
        m.add_row(-INF, 5.0, &[(0, 1.0), (1, 1.0), (2, 2.0), (3, 1.0)]);
        assert!(Compression::build(&m).is_none());
        m.col_cost[1] = 0.0;
        let c = Compression::build(&m).unwrap();
        assert_eq!(c.reduced_model.num_col, 3);
    }

    #[test]
    fn duplicates_invalid_domains_and_unbounded_groups_are_rejected() {
        let mut m = mixed();
        m.add_row(-INF, INF, &[(0, 1.0), (0, -1.0)]);
        assert!(Compression::build(&m).is_none());
        let mut m = mixed();
        m.col_lower[0] = 0.2;
        m.col_upper[0] = 0.8;
        assert!(Compression::build(&m).is_none());
        let mut m = Model::new();
        m.add_col(0.0, INF);
        m.add_col(0.0, INF);
        assert!(Compression::build(&m).is_none());
    }
}
