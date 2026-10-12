//! Directional pseudocosts and a bounded shortlist for reliability branching.
//!
//! Only solved child relaxations contribute observations. Estimates influence
//! branching order; they must never be used as certified subtree bounds.

const INT_TOL: f64 = 1e-6;
const RELIABLE_SAMPLES: u64 = 2;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Candidate {
    pub column: usize,
    pub down_distance: f64,
    pub up_distance: f64,
    pub score: f64,
    /// Probe only while either direction lacks enough solved observations.
    pub needs_probe: bool,
}
/// Per-worker objective degradation per unit of branching distance.
pub(crate) struct Pseudocosts {
    means: Vec<[f64; 2]>,
    counts: Vec<[u64; 2]>,
    global_means: [f64; 2],
    global_counts: [u64; 2],
}

/// Prefer branches that raise both child bounds. Inputs are objective gains,
/// rather than pseudocosts per unit distance.
pub(crate) fn gain_score(down: f64, up: f64) -> f64 {
    let down = down.max(0.0);
    let up = up.max(0.0);
    down.min(up) + 0.1 * down.max(up)
}

impl Pseudocosts {
    pub(crate) fn new(n: usize) -> Self {
        Self {
            means: vec![[0.0; 2]; n],
            counts: vec![[0; 2]; n],
            global_means: [0.0; 2],
            global_counts: [0; 2],
        }
    }

    pub(crate) fn update(&mut self, branch: Option<(usize, bool, f64, f64)>, bound: f64) {
        let Some((j, up, distance, parent)) = branch else {
            return;
        };
        if j >= self.means.len()
            || !parent.is_finite()
            || !bound.is_finite()
            || !distance.is_finite()
            || distance <= INT_TOL
        {
            return;
        }
        let value = (bound - parent).max(0.0) / distance;
        if !value.is_finite() {
            return;
        }
        let side = usize::from(up);
        self.counts[j][side] += 1;
        self.means[j][side] += (value - self.means[j][side]) / self.counts[j][side] as f64;
        self.global_counts[side] += 1;
        self.global_means[side] +=
            (value - self.global_means[side]) / self.global_counts[side] as f64;
    }

    fn estimate(&self, j: usize, side: usize) -> f64 {
        if self.counts[j][side] > 0 {
            self.means[j][side]
        } else if self.global_counts[side] > 0 {
            self.global_means[side]
        } else if self.global_counts[1 - side] > 0 {
            self.global_means[1 - side]
        } else {
            1.0
        }
    }

    fn candidate(&self, j: usize, value: f64, learn: bool) -> Option<Candidate> {
        if j >= self.means.len() || !value.is_finite() {
            return None;
        }
        let down_distance = value - value.floor();
        let up_distance = 1.0 - down_distance;
        if down_distance.min(up_distance) <= INT_TOL {
            return None;
        }
        Some(Candidate {
            column: j,
            down_distance,
            up_distance,
            score: if learn {
                gain_score(
                    down_distance * self.estimate(j, 0),
                    up_distance * self.estimate(j, 1),
                )
            } else {
                down_distance.min(up_distance)
            },
            needs_probe: learn && self.counts[j].iter().any(|&n| n < RELIABLE_SAMPLES),
        })
    }

    /// Keep at most `limit` candidates, sorted best first with column-index ties.
    /// The caller owns LP probe budgets and keeps solved child LPs for reuse.
    pub(crate) fn shortlist(
        &self,
        cols: &[usize],
        x: &[f64],
        learn: bool,
        limit: usize,
    ) -> Vec<Candidate> {
        let mut best: Vec<Candidate> = Vec::with_capacity(limit.min(cols.len()));
        if limit == 0 {
            return best;
        }
        for &j in cols {
            let Some(candidate) = x.get(j).and_then(|&v| self.candidate(j, v, learn)) else {
                continue;
            };
            let position = best.partition_point(|c| {
                c.score > candidate.score
                    || (c.score == candidate.score && c.column < candidate.column)
            });
            if position < limit {
                best.insert(position, candidate);
                best.truncate(limit);
            }
        }
        best
    }

    #[cfg(test)]
    pub(crate) fn choose(&self, cols: &[usize], x: &[f64], learn: bool) -> Option<usize> {
        self.shortlist(cols, x, learn, 1)
            .first()
            .map(|candidate| candidate.column)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reliability_requires_both_directions_and_multiple_observations() {
        let mut costs = Pseudocosts::new(1);
        for _ in 0..2 {
            costs.update(Some((0, false, 0.25, 10.0)), 11.0);
        }
        assert!(costs.shortlist(&[0], &[0.25], true, 1)[0].needs_probe);
        costs.update(Some((0, true, 0.75, 10.0)), 13.0);
        assert!(costs.shortlist(&[0], &[0.25], true, 1)[0].needs_probe);
        costs.update(Some((0, true, 0.75, 10.0)), 13.0);
        let candidate = costs.shortlist(&[0], &[0.25], true, 1)[0];
        assert!(!candidate.needs_probe);
        assert_eq!(candidate.down_distance, 0.25);
        assert_eq!(candidate.up_distance, 0.75);
        assert!((candidate.score - 1.3).abs() < 1e-12);
    }

    #[test]
    fn learned_gains_can_prefer_less_fractional_variable() {
        let mut costs = Pseudocosts::new(2);
        for side in [false, true] {
            costs.update(Some((0, side, 0.5, 0.0)), 0.5);
            costs.update(Some((1, side, if side { 0.8 } else { 0.2 }, 0.0)), 5.0);
        }
        assert_eq!(costs.choose(&[0, 1], &[0.5, 0.2], false), Some(0));
        assert_eq!(costs.choose(&[0, 1], &[0.5, 0.2], true), Some(1));
        assert!(gain_score(3.0, 3.0) > gain_score(0.0, 20.0));
    }

    #[test]
    fn unobserved_directions_use_the_observed_objective_scale() {
        let mut costs = Pseudocosts::new(2);
        costs.update(Some((0, false, 0.5, 0.0)), 500.0);
        let shortlist = costs.shortlist(&[0, 1], &[0.5, 0.5], true, 2);
        assert_eq!(shortlist[0].column, 0);
        assert_eq!(shortlist[1].column, 1);
        assert_eq!(shortlist[0].score, shortlist[1].score);
        assert_eq!(shortlist[1].score, 550.0);
    }

    #[test]
    fn shortlist_is_bounded_deterministic_and_ignores_invalid_observations() {
        let mut costs = Pseudocosts::new(5);
        costs.update(Some((0, false, 0.5, 0.0)), f64::NAN);
        costs.update(Some((0, false, 0.0, 0.0)), 100.0);
        costs.update(Some((0, true, f64::INFINITY, 0.0)), 100.0);
        let x = [0.5, 0.5, 1.0, f64::NAN, -0.5];
        let shortlist = costs.shortlist(&[4, 3, 2, 1, 0], &x, true, 2);
        assert_eq!(
            shortlist.iter().map(|c| c.column).collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(shortlist.iter().all(|c| c.needs_probe));
        assert!(costs.shortlist(&[0, 1], &x, true, 0).is_empty());
        assert_eq!(costs.choose(&[2, 3], &x, true), None);
    }
}
