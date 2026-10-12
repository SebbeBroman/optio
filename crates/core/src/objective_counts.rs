//! Equivalent integer objective-count columns, exposing category decisions to
//! branching while retaining every original variable, row and domain.
use crate::model::{check_solution, Model, VarType, INF};
use std::collections::BTreeMap;

const EXACT_MAX: i128 = 1i128 << 53;

pub(crate) struct ObjectiveCounts<'a> {
    original: &'a Model,
    pub(crate) augmented_model: Model,
    groups: Vec<Vec<usize>>,
}

struct Group {
    cost: i128,
    columns: Vec<usize>,
    lower: i128,
    domain_upper: Option<i128>,
    upper: Option<i128>,
}

fn integer(x: f64) -> Option<i128> {
    (x.is_finite() && x.abs() <= EXACT_MAX as f64 && x.fract() == 0.0).then_some(x as i128)
}

impl<'a> ObjectiveCounts<'a> {
    pub(crate) fn build(original: &'a Model) -> Option<Self> {
        let mut groups: Vec<Group> = Vec::new();
        let mut by_cost = BTreeMap::new();
        let mut column_group = vec![None; original.num_col];
        let mut objective_columns = 0;
        for j in 0..original.num_col {
            let cost = original.col_cost[j];
            if !cost.is_finite() {
                return None;
            }
            if cost == 0.0 {
                continue;
            }
            let cost = integer(cost)?;
            let lo = original.col_lower[j];
            let hi = original.col_upper[j];
            if cost.abs() > 1_000_000_000
                || !original.col_type[j].is_integral()
                || !lo.is_finite()
                || lo < 0.0
                || lo >= INF
                || hi.is_nan()
                || hi < lo
            {
                return None;
            }
            let lo = integer(lo.ceil())?;
            let hi = if hi.is_finite() && hi < INF {
                Some(integer(hi.floor())?)
            } else {
                None
            };
            if hi.is_some_and(|hi| hi < lo) {
                return None;
            }
            let g = *by_cost.entry(cost).or_insert_with(|| {
                groups.push(Group {
                    cost,
                    columns: Vec::new(),
                    lower: 0,
                    domain_upper: Some(0),
                    upper: Some(0),
                });
                groups.len() - 1
            });
            if groups.len() > 8 {
                return None;
            }
            groups[g].columns.push(j);
            groups[g].lower += lo;
            groups[g].domain_upper = groups[g].domain_upper.zip(hi).map(|(a, b)| a + b);
            groups[g].upper = groups[g].domain_upper;
            column_group[j] = Some(g);
            objective_columns += 1;
        }
        if objective_columns < 32 || groups.is_empty() {
            return None;
        }

        let mut seen = vec![usize::MAX; original.num_col];
        let mut covered = vec![0usize; groups.len()];
        let mut minimum = vec![i128::MAX; groups.len()];
        for r in 0..original.num_row {
            let rhs = original.row_upper[r];
            if !rhs.is_finite() || rhs < 0.0 || rhs >= INF || rhs > EXACT_MAX as f64 {
                continue;
            }
            covered.fill(0);
            minimum.fill(i128::MAX);
            let mut valid = true;
            for k in original.a_start[r] as usize..original.a_start[r + 1] as usize {
                let j = original.a_index[k] as usize;
                let a = original.a_value[k];
                if seen[j] == r {
                    valid = false;
                }
                seen[j] = r;
                if a == 0.0 {
                    continue;
                }
                if !a.is_finite() || a < 0.0 || !(original.col_lower[j] >= 0.0) {
                    valid = false;
                    continue;
                }
                if let Some(g) = column_group[j] {
                    if let Some(a) = integer(a).filter(|&a| a > 0) {
                        covered[g] += 1;
                        minimum[g] = minimum[g].min(a);
                    } else {
                        valid = false;
                    }
                }
            }
            if valid {
                // Loosen the cap at numerical row edges; this remains a superset
                // of exact feasible counts and preserves tightly checked starts.
                let rhs = (rhs.next_up() + 1e-9).floor() as i128;
                for (g, group) in groups.iter_mut().enumerate() {
                    if covered[g] == group.columns.len() {
                        let upper = rhs / minimum[g];
                        group.upper = Some(group.upper.map_or(upper, |old| old.min(upper)));
                    }
                }
            }
        }
        // Keep this reformulation focused on scarce objective categories; a
        // complete cover must improve at least one summed original domain.
        if !groups.iter().any(|g| {
            g.upper
                .is_some_and(|hi| g.domain_upper.is_none_or(|old| hi < old))
        }) {
            return None;
        }
        let mut magnitude = 0i128;
        for group in &groups {
            let upper = group.upper?;
            if upper < group.lower || upper > EXACT_MAX || group.lower > EXACT_MAX {
                return None;
            }
            // This stronger condition also prevents large cancellation in the
            // original floating objective from disagreeing with count costs.
            magnitude += group.cost.abs() * upper;
        }
        if magnitude > EXACT_MAX {
            return None;
        }

        let mut augmented_model = original.clone();
        for group in &groups {
            let count = augmented_model.add_col(group.lower as f64, group.upper? as f64);
            augmented_model.set_integrality(count, VarType::Integer);
            augmented_model.set_cost(count, group.cost as f64);
            let mut equality: Vec<_> = group.columns.iter().map(|&j| (j as u32, 1.0)).collect();
            equality.push((count.0, -1.0));
            augmented_model.add_row(0.0, 0.0, &equality);
            for &j in &group.columns {
                augmented_model.col_cost[j] = 0.0;
            }
        }
        Some(Self {
            original,
            augmented_model,
            groups: groups.into_iter().map(|g| g.columns).collect(),
        })
    }

    pub(crate) fn project_start(&self, point: &[f64]) -> Option<Vec<f64>> {
        if point.len() != self.original.num_col
            || !check_solution(self.original, point, 1e-9).feasible
        {
            return None;
        }
        let mut projected = point.to_vec();
        for group in &self.groups {
            projected.push(group.iter().map(|&j| point[j]).sum());
        }
        (check_solution(&self.augmented_model, &projected, 1e-9).feasible
            && self.same_objective(point, &projected))
        .then_some(projected)
    }

    pub(crate) fn lift(&self, point: &[f64]) -> Option<Vec<f64>> {
        if point.len() != self.augmented_model.num_col
            || !check_solution(&self.augmented_model, point, 1e-9).feasible
        {
            return None;
        }
        let original = point[..self.original.num_col].to_vec();
        (check_solution(self.original, &original, 1e-9).feasible
            && self.same_objective(&original, point))
        .then_some(original)
    }

    fn same_objective(&self, original: &[f64], augmented: &[f64]) -> bool {
        (self.original.objective(original) - self.augmented_model.objective(augmented)).abs()
            <= 1e-9
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixed() -> Model {
        let mut m = Model::new();
        for j in 0..32 {
            let c = m.add_col(0.0, if j < 3 { 3.0 } else { 0.0 });
            m.set_integrality(c, VarType::Integer);
            m.set_cost(c, if j == 2 { 11.0 } else { -7.0 });
        }
        m.add_col(0.0, 2.0); // Zero-cost continuous auxiliary.
        let row: Vec<_> = (0..32)
            .filter(|&j| j != 2)
            .map(|j| (j, if j == 1 { 2.0 } else { 1.0 }))
            .chain([(32, 0.25)])
            .collect();
        m.add_row(-INF, 4.5, &row);
        m
    }

    #[test]
    fn exhaustive_witness_mapping_preserves_objective_and_original_coordinates() {
        let m = mixed();
        let counts = ObjectiveCounts::build(&m).unwrap();
        assert_eq!(counts.augmented_model.num_col, m.num_col + 2);
        assert_eq!(&counts.augmented_model.col_type[..m.num_col], m.col_type);
        for a in 0..=3 {
            for b in 0..=3 {
                for c in 0..=3 {
                    for z in [0.0, 0.5, 2.0] {
                        let mut point = vec![0.0; m.num_col];
                        point[..3].copy_from_slice(&[a as f64, b as f64, c as f64]);
                        point[32] = z;
                        if check_solution(&m, &point, 1e-9).feasible {
                            let projected = counts.project_start(&point).unwrap();
                            assert_eq!(counts.lift(&projected).unwrap(), point);
                            assert_eq!(
                                m.objective(&point),
                                counts.augmented_model.objective(&projected)
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn cover_caps_bound_group_counts_without_changing_original_domains() {
        let m = mixed();
        let counts = ObjectiveCounts::build(&m).unwrap();
        assert_eq!(counts.augmented_model.col_upper[m.num_col], 4.0);
        assert_eq!(counts.augmented_model.col_upper[m.num_col + 1], 3.0);
        assert_eq!(&counts.augmented_model.col_upper[..m.num_col], m.col_upper);
        let mut point = vec![0.0; m.num_col];
        point[0] = 2.0;
        point[1] = 1.0;
        let mut projected = counts.project_start(&point).unwrap();
        projected[m.num_col] += 1.0;
        assert!(counts.lift(&projected).is_none());
        point[0] = 3.0;
        point[1] = 3.0;
        assert!(counts.project_start(&point).is_none());
    }

    #[test]
    fn unsafe_costs_domains_and_large_cancellation_are_rejected() {
        let mut m = mixed();
        m.col_cost[0] = 1.5;
        assert!(ObjectiveCounts::build(&m).is_none());
        let mut m = mixed();
        m.col_type[0] = VarType::Continuous;
        assert!(ObjectiveCounts::build(&m).is_none());
        let mut m = mixed();
        m.col_lower[0] = -1.0;
        assert!(ObjectiveCounts::build(&m).is_none());
        let mut m = mixed();
        m.col_lower[2] = 1_000_000_001.0;
        m.col_upper[2] = m.col_lower[2];
        m.col_cost[2] = -999_999_936.0;
        m.col_lower[0] = 1_000_000_000.0;
        m.col_upper[0] = m.col_lower[0] + 1.0;
        for j in 0..32 {
            if j != 2 {
                m.col_cost[j] = 999_999_937.0;
            }
        }
        m.row_upper[0] = 1_000_000_000.0;
        assert!(ObjectiveCounts::build(&m).is_none());
    }

    #[test]
    fn duplicate_incomplete_and_negative_rows_cannot_establish_caps() {
        for variant in 0..3 {
            let mut m = mixed();
            match variant {
                0 => {
                    m.a_index[0] = m.a_index[1];
                }
                1 => {
                    m.a_value[0] = 0.0;
                }
                _ => {
                    m.a_value[0] = -1.0;
                }
            }
            assert!(ObjectiveCounts::build(&m).is_none());
        }
    }

    #[test]
    fn too_few_objective_columns_or_no_domain_reduction_are_skipped() {
        let mut m = mixed();
        m.col_cost[31] = 0.0;
        assert!(ObjectiveCounts::build(&m).is_none());
        let mut m = mixed();
        m.row_upper[0] = 100.0;
        assert!(ObjectiveCounts::build(&m).is_none());
    }
}
