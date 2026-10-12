//! Feasible pattern search for integer scheduling blocks coupled by load rows.
//! Detects constraints, not variable names; every returned point is checked.
use crate::{
    model::{check_solution, Model, INF},
    solve::Clock,
};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

#[path = "planning_proof.rs"]
mod proof;
pub(crate) use proof::prove;

// These bounded DP tables contain internally generated numeric load profiles.
// Their hash need not perform the keyed byte mixing used for arbitrary input.
// Equality still compares every count/load, so collisions cannot merge states.
#[derive(Default)]
struct ProfileHasher(u64);
impl Hasher for ProfileHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(7) ^ value).wrapping_mul(0x9e3779b185ebca87);
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            self.write_u64(u64::from_le_bytes(chunk.try_into().unwrap()));
        }
        let mut tail = [0u8; 8];
        tail[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
        if !chunks.remainder().is_empty() {
            self.write_u64(u64::from_le_bytes(tail));
        }
    }
}
type Profiles<K, V> = HashMap<K, V, BuildHasherDefault<ProfileHasher>>;

struct Group {
    session: usize,
    feeders: Vec<(usize, f64)>,
    cap: f64,
}
struct Pattern {
    values: Vec<(usize, f64)>,
    loads: Vec<(usize, f64)>,
    cost: f64,
}
struct Block {
    rows: Vec<usize>,
    cols: Vec<usize>,
    patterns: Arc<Vec<Pattern>>,
}
struct Rule {
    target: usize,
    lower: bool,
    constant: f64,
    loads: Vec<(usize, f64)>,
    aux: Vec<(usize, f64)>,
}
struct Aux {
    col: usize,
    upper_only: bool,
    rules: Vec<Rule>,
}
struct Shape {
    groups: Vec<Group>,
    sessions: Vec<Vec<usize>>,
    blocks: Vec<Block>,
    programs: Vec<Vec<Aux>>,
    peak_cols: Vec<usize>,
    column_groups: Vec<Option<usize>>,
    xr_x: Vec<Option<usize>>,
    x_z: Vec<Option<usize>>,
}

fn pattern_key(p: &Pattern) -> Vec<u64> {
    let mut key = vec![p.cost.to_bits()];
    let mut loads = p.loads.clone();
    loads.sort_by_key(|&(g, _)| g);
    for (g, l) in loads {
        key.push(g as u64);
        key.push(l.to_bits());
    }
    key
}

fn root(parent: &mut [usize], j: usize) -> usize {
    if parent[j] != j {
        parent[j] = root(parent, parent[j]);
    }
    parent[j]
}
fn row(model: &Model, r: usize) -> impl Iterator<Item = (usize, f64)> + '_ {
    (model.a_start[r] as usize..model.a_start[r + 1] as usize)
        .map(|k| (model.a_index[k] as usize, model.a_value[k]))
}

fn row_len(model: &Model, r: usize) -> usize {
    (model.a_start[r + 1] - model.a_start[r]) as usize
}

fn only(mut entries: impl Iterator<Item = (usize, f64)>) -> Option<(usize, f64)> {
    let first = entries.next()?;
    entries.next().is_none().then_some(first)
}

fn detect(model: &Model, clock: &dyn Clock, deadline: f64, sample_limit: usize) -> Option<Shape> {
    let primal = sample_limit != 0;
    let n = model.num_col;
    let mut x_z = vec![None; n];
    for r in 0..model.num_row {
        if row_len(model, r) != 2 || model.row_lower[r] > -INF / 2. || model.row_upper[r] != 0. {
            continue;
        }
        for (x, a) in row(model, r) {
            let (z, b) = row(model, r).find(|&(c, _)| c != x)?;
            if a > 0.
                && b < 0.
                && model.col_type[x].is_integral()
                && model.col_type[z].is_integral()
                && model.col_lower[z] >= 0.
                && model.col_upper[z] <= 1.
            {
                x_z[x] = Some(z)
            }
        }
    }
    let mut groups = Vec::<Group>::new();
    let mut sessions = Vec::<Vec<usize>>::new();
    let mut peak_session = HashMap::new();
    let mut x_group = vec![None; n];
    for r in 0..model.num_row {
        if model.row_upper[r] != 0. || model.row_lower[r] > -INF / 2. {
            continue;
        }
        let Some((peak, coefficient)) =
            only(row(model, r).filter(|&(j, _)| !model.col_type[j].is_integral()))
        else {
            continue;
        };
        if coefficient != -1. || model.col_cost[peak] <= 0. {
            continue;
        }
        let feeders: Vec<_> = row(model, r)
            .filter(|&(j, _)| model.col_type[j].is_integral())
            .collect();
        if feeders.is_empty()
            || feeders
                .iter()
                .any(|&(j, a)| x_z[j].is_none() || a <= 0. || x_group[j].is_some())
        {
            continue;
        }
        let s = *peak_session.entry(peak).or_insert_with(|| {
            sessions.push(Vec::new());
            sessions.len() - 1
        });
        let g = groups.len();
        for &(j, _) in &feeders {
            x_group[j] = Some(g)
        }
        sessions[s].push(g);
        groups.push(Group {
            session: s,
            feeders,
            cap: INF,
        });
    }
    if groups.is_empty() {
        return None;
    }
    let mut duration = vec![0.; n];
    for g in &groups {
        for &(j, d) in &g.feeders {
            duration[j] = d
        }
    }
    for r in 0..model.num_row {
        if model.row_upper[r] >= INF / 2.
            || model.row_upper[r] <= 0.
            || model.row_lower[r] == model.row_upper[r]
        {
            continue;
        }
        if let Some(g) = row(model, r).next().and_then(|(j, _)| x_group[j]) {
            if row_len(model, r) == groups[g].feeders.len()
                && row(model, r)
                    .all(|(j, a)| x_group[j] == Some(g) && (a - duration[j]).abs() < 1e-9)
            {
                groups[g].cap = groups[g].cap.min(model.row_upper[r])
            }
        }
    }
    // Integer equalities define independent scheduling blocks. Capacity rows
    // are inequalities and therefore do not merge otherwise independent units.
    let mut parent: Vec<_> = (0..n).collect();
    for r in 0..model.num_row {
        if model.row_lower[r] != model.row_upper[r] {
            continue;
        }
        if row_len(model, r) == 0 || row(model, r).any(|(j, _)| !model.col_type[j].is_integral()) {
            continue;
        }
        let first = row(model, r).next()?.0;
        for (j, _) in row(model, r) {
            let a = root(&mut parent, first);
            let b = root(&mut parent, j);
            parent[b] = a
        }
    }
    for (x, z) in x_z.iter().enumerate() {
        if let Some(z) = z {
            let a = root(&mut parent, x);
            let b = root(&mut parent, *z);
            parent[b] = a
        }
    }
    let mut components = HashMap::<usize, Vec<usize>>::new();
    for j in 0..n {
        if model.col_type[j].is_integral() {
            let p = root(&mut parent, j);
            components.entry(p).or_default().push(j)
        }
    }
    // A single allowed ring can produce x - y = 0, where y is a day
    // choice rather than a per-round allocation. Keep the one-hot choices
    // out of the round-to-load map so their defining row remains detectable.
    let mut choice_column = vec![false; n];
    for r in 0..model.num_row {
        if model.row_lower[r] == 1.
            && model.row_upper[r] == 1.
            && row(model, r).all(|(j, a)| {
                a == 1.
                    && model.col_type[j].is_integral()
                    && x_group[j].is_none()
                    && model.col_lower[j] >= 0.
                    && model.col_upper[j] <= 1.
            })
        {
            for (j, _) in row(model, r) {
                choice_column[j] = true;
            }
        }
    }
    let mut xr_x = vec![None; n];
    for r in 0..model.num_row {
        if model.row_lower[r] != 0. || model.row_upper[r] != 0. {
            continue;
        }
        let Some((x, _)) = only(row(model, r).filter(|&(j, a)| x_group[j].is_some() && a == 1.))
        else {
            continue;
        };
        if row_len(model, r) > 1
            && row(model, r).all(|(j, a)| {
                j == x || (model.col_type[j].is_integral() && x_group[j].is_none() && a == -1.)
            })
        {
            for (j, _) in row(model, r) {
                if j != x && !choice_column[j] {
                    xr_x[j] = Some(x)
                }
            }
        }
    }
    // Index integer-only rows once. Scanning every row for every component
    // repeats the same work for identical blocks and allocates temporary terms.
    let component_of: Vec<_> = (0..n).map(|j| root(&mut parent, j)).collect();
    let mut component_rows = HashMap::<usize, Vec<usize>>::new();
    for r in 0..model.num_row {
        let mut terms = row(model, r);
        if let Some((first, _)) = terms.next() {
            let p = component_of[first];
            if model.col_type[first].is_integral()
                && terms.all(|(j, _)| model.col_type[j].is_integral() && component_of[j] == p)
            {
                component_rows.entry(p).or_default().push(r);
            }
        }
    }
    let mut blocks = Vec::new();
    let mut templates = HashMap::<Vec<u64>, Arc<Vec<Pattern>>>::new();
    let mut point = vec![0.; n];
    let mut ordered: Vec<_> = components.into_values().collect();
    ordered.sort_by_key(|c| c[0]);
    for cols in ordered {
        if !cols.iter().any(|&j| x_group[j].is_some()) {
            return None;
        }
        let local_rows = component_rows
            .remove(&component_of[cols[0]])
            .unwrap_or_default();
        // The certificate derives all alternatives from original demand rows;
        // primal ring permutations and auxiliary projection are unnecessary.
        if !primal {
            blocks.push(Block {
                rows: local_rows,
                cols,
                patterns: Arc::new(Vec::new()),
            });
            continue;
        }
        let mut template = vec![cols.len() as u64];
        for &j in &cols {
            template.extend([
                model.col_cost[j].to_bits(),
                model.col_lower[j].to_bits(),
                model.col_upper[j].to_bits(),
                duration[j].to_bits(),
                x_group[j].map_or(0, |g| g as u64 + 1),
                xr_x[j].map_or(0, |x| cols.binary_search(&x).unwrap() as u64 + 1),
            ]);
        }
        for &r in &local_rows {
            template.extend([
                model.row_lower[r].to_bits(),
                model.row_upper[r].to_bits(),
                (model.a_start[r + 1] - model.a_start[r]) as u64,
            ]);
            for (j, a) in row(model, r) {
                template.extend([cols.binary_search(&j).unwrap() as u64, a.to_bits()]);
            }
        }
        if let Some(patterns) = templates.get(&template) {
            blocks.push(Block {
                rows: local_rows,
                cols,
                patterns: Arc::clone(patterns),
            });
            continue;
        }
        let choices = local_rows.iter().find_map(|&r| {
            (model.row_lower[r] == 1.
                && model.row_upper[r] == 1.
                && row(model, r).all(|(j, a)| {
                    a == 1.
                        && x_group[j].is_none()
                        && xr_x[j].is_none()
                        && model.col_lower[j] >= 0.
                        && model.col_upper[j] <= 1.
                }))
            .then(|| row(model, r).map(|(j, _)| j).collect::<Vec<_>>())
        })?;
        let mut patterns = Vec::new();
        for &choice in &choices {
            if model.col_upper[choice] < 1. {
                continue;
            }
            let mut demands = Vec::<(Vec<usize>, f64, usize)>::new();
            let mut allowed = HashMap::<usize, Vec<usize>>::new();
            for &r in &local_rows {
                if model.row_lower[r] != 0. || model.row_upper[r] != 0. {
                    continue;
                }
                let positives: Vec<_> = row(model, r).filter(|&(_, a)| a > 0.).collect();
                if positives.is_empty()
                    || positives.iter().any(|&(j, a)| xr_x[j].is_none() || a != 1.)
                    || row(model, r).any(|(j, a)| a < 0. && !choices.contains(&j))
                {
                    continue;
                }
                let amount = -row(model, r)
                    .filter(|&(j, _)| j == choice)
                    .map(|(_, a)| a)
                    .sum::<f64>();
                if amount <= 0. {
                    continue;
                }
                let sid = demands.len();
                let gs: Vec<_> = positives
                    .iter()
                    .map(|&(j, _)| x_group[xr_x[j].unwrap()].unwrap())
                    .collect();
                allowed
                    .entry(sid)
                    .and_modify(|a| a.retain(|g| gs.contains(g)))
                    .or_insert(gs);
                demands.push((positives.iter().map(|&(j, _)| j).collect(), amount, sid));
            }
            if demands.is_empty() {
                continue;
            }
            let mut active: Vec<_> = allowed.into_iter().collect();
            active.sort_by_key(|&(s, _)| s);
            if active.iter().any(|(_, gs)| gs.is_empty()) {
                continue;
            }
            let count = active
                .iter()
                .try_fold(1usize, |v, (_, gs)| v.checked_mul(gs.len()))?;
            if count > 100_000 {
                return None;
            }
            let samples = count.min(sample_limit);
            let mut step = (count / samples).max(1);
            while gcd(step, count) != 1 {
                step += 1;
            }
            for k in 0..samples {
                let index = (k * step) % count;
                if patterns.len() % 256 == 0 && clock.now_secs() > deadline {
                    return None;
                }
                for balanced in [false, true] {
                    let mut index = index;
                    for &j in &cols {
                        point[j] = 0.
                    }
                    point[choice] = 1.;
                    let mut selected = HashMap::new();
                    for (s, gs) in &active {
                        selected.insert(*s, gs[index % gs.len()]);
                        index /= gs.len()
                    }
                    for (vars, amount, s) in &demands {
                        let g = selected[s];
                        let mut candidates: Vec<_> = vars
                            .iter()
                            .copied()
                            .filter(|&j| {
                                let h = x_group[xr_x[j].unwrap()].unwrap();
                                if balanced {
                                    let ring =
                                        sessions[groups[g].session].iter().position(|&v| v == g);
                                    sessions[groups[h].session].iter().position(|&v| v == h) == ring
                                } else {
                                    h == g
                                }
                            })
                            .collect();
                        candidates.sort_by_key(|&j| x_group[xr_x[j].unwrap()].unwrap());
                        let offset = candidates
                            .iter()
                            .position(|&j| x_group[xr_x[j].unwrap()] == Some(g))?;
                        for k in 0..(*amount as usize) {
                            let j = candidates[(offset + k) % candidates.len()];
                            point[j] += 1.;
                            point[xr_x[j].unwrap()] += 1.;
                        }
                    }
                    for &j in &cols {
                        if let Some(z) = x_z[j] {
                            point[z] = if point[j] > 0. { 1. } else { 0. }
                        }
                    }
                    if cols.iter().any(|&j| {
                        point[j] < model.col_lower[j] - 1e-7 || point[j] > model.col_upper[j] + 1e-7
                    }) {
                        continue;
                    }
                    if local_rows.iter().any(|&r| {
                        let v = row(model, r).map(|(j, a)| point[j] * a).sum::<f64>();
                        v < model.row_lower[r] - 1e-7 || v > model.row_upper[r] + 1e-7
                    }) {
                        continue;
                    }
                    let mut loads = Vec::new();
                    let mut occupied: Vec<_> = cols
                        .iter()
                        .filter_map(|&j| (point[j] > 0.).then_some(x_group[j]).flatten())
                        .collect();
                    occupied.sort_unstable();
                    occupied.dedup();
                    for g in occupied {
                        let l = cols
                            .iter()
                            .filter(|&&j| x_group[j] == Some(g))
                            .map(|&j| point[j] * duration[j])
                            .sum::<f64>();
                        if l > 0. {
                            loads.push((g, l))
                        }
                    }
                    if loads.iter().any(|&(g, l)| l > groups[g].cap + 1e-7) {
                        continue;
                    }
                    patterns.push(Pattern {
                        cost: cols.iter().map(|&j| model.col_cost[j] * point[j]).sum(),
                        values: cols
                            .iter()
                            .filter(|&&j| point[j] != 0.)
                            .map(|&j| (cols.binary_search(&j).unwrap(), point[j]))
                            .collect(),
                        loads,
                    });
                }
            }
        }
        if patterns.is_empty() {
            return None;
        }
        patterns.sort_by_cached_key(pattern_key);
        patterns.dedup_by(|a, b| pattern_key(a) == pattern_key(b));
        let patterns = Arc::new(patterns);
        templates.insert(template, Arc::clone(&patterns));
        blocks.push(Block {
            rows: local_rows,
            cols,
            patterns,
        });
    }
    let mut programs: Vec<Vec<Aux>> = (0..sessions.len()).map(|_| Vec::new()).collect();
    if primal {
        // Compile continuous auxiliary projection into per-session affine rules.
        // Each integer expression must be a complete scaled group load.
        let mut rules = Vec::<Rule>::new();
        let mut aux_session = vec![None; n];
        let mut cont = Vec::new();
        let mut coefficients = HashMap::<usize, (f64, usize)>::new();
        for r in 0..model.num_row {
            cont.clear();
            cont.extend(row(model, r).filter(|&(j, _)| !model.col_type[j].is_integral()));
            if cont.is_empty() {
                continue;
            }
            coefficients.clear();
            let mut sid = None;
            for (j, a) in row(model, r) {
                if model.col_type[j].is_integral() {
                    let g = x_group[j]?;
                    let v = a / duration[j];
                    let e = coefficients.entry(g).or_insert((v, 0));
                    if (e.0 - v).abs() > 1e-8 {
                        return None;
                    }
                    e.1 += 1;
                    if sid.is_some_and(|s| s != groups[g].session) {
                        return None;
                    }
                    sid = Some(groups[g].session)
                }
            }
            if coefficients
                .iter()
                .any(|(&g, &(_, count))| count != groups[g].feeders.len())
            {
                return None;
            }
            if let Some(s) = sid {
                for &(j, _) in &cont {
                    if aux_session[j].is_some_and(|v| v != s) {
                        return None;
                    }
                    aux_session[j] = Some(s)
                }
            }
            for &(target, a) in &cont {
                for (bound, lower) in [(model.row_lower[r], true), (model.row_upper[r], false)] {
                    if bound.abs() >= INF / 2. {
                        continue;
                    }
                    rules.push(Rule {
                        target,
                        lower: if a > 0. { lower } else { !lower },
                        constant: bound / a,
                        loads: coefficients
                            .iter()
                            .map(|(&g, &(v, _))| (g, -v / a))
                            .collect(),
                        aux: cont
                            .iter()
                            .filter(|&&(j, _)| j != target)
                            .map(|&(j, b)| (j, -b / a))
                            .collect(),
                    });
                }
            }
        }
        // Propagate session membership through continuous-only definition rows.
        for _ in 0..8 {
            for r in &rules {
                let s = aux_session[r.target]
                    .or_else(|| r.aux.iter().find_map(|&(j, _)| aux_session[j]));
                if let Some(s) = s {
                    aux_session[r.target] = Some(s);
                    for &(j, _) in &r.aux {
                        aux_session[j] = Some(s)
                    }
                }
            }
        }

        let continuous: Vec<_> = (0..n)
            .filter(|&j| !model.col_type[j].is_integral())
            .collect();
        let mut rules_by_target = vec![Vec::new(); n];
        for (i, rule) in rules.iter().enumerate() {
            rules_by_target[rule.target].push(i);
        }
        let mut ready = vec![false; n];
        for _ in 0..continuous.len() {
            let mut progress = false;
            for &j in &continuous {
                if ready[j] {
                    continue;
                }
                let usable: Vec<_> = rules_by_target[j]
                    .iter()
                    .copied()
                    .filter(|&i| rules[i].aux.iter().all(|&(c, _)| ready[c]))
                    .collect();
                if usable.is_empty() {
                    continue;
                }
                let s = aux_session[j]?;
                let upper_only = usable.iter().all(|&i| !rules[i].lower);
                let own = usable
                    .into_iter()
                    .map(|i| {
                        let r = &mut rules[i];
                        Rule {
                            target: j,
                            lower: r.lower,
                            constant: r.constant,
                            loads: std::mem::take(&mut r.loads),
                            aux: std::mem::take(&mut r.aux),
                        }
                    })
                    .collect();
                programs[s].push(Aux {
                    col: j,
                    upper_only,
                    rules: own,
                });
                ready[j] = true;
                progress = true;
            }
            if !progress {
                break;
            }
        }
        if (0..n).any(|j| !model.col_type[j].is_integral() && !ready[j]) {
            return None;
        }
    }
    let mut column_groups = x_group.clone();
    for j in 0..n {
        if let Some(g) = x_group[j] {
            if let Some(z) = x_z[j] {
                column_groups[z] = Some(g);
            }
        }
        if let Some(x) = xr_x[j] {
            column_groups[j] = x_group[x];
        }
    }
    let mut peak_cols = vec![0; sessions.len()];
    for (col, session) in peak_session {
        peak_cols[session] = col;
    }
    Some(Shape {
        peak_cols,
        xr_x,
        x_z,
        column_groups,
        groups,
        sessions,
        blocks,
        programs,
    })
}

fn session_cost(
    model: &Model,
    sh: &Shape,
    s: usize,
    loads: &[f64],
    aux: &mut [f64],
    respect_box: bool,
) -> f64 {
    let mut cost = 0.;
    for a in &sh.programs[s] {
        let mut lo = if respect_box {
            model.col_lower[a.col]
        } else {
            0.
        };
        let mut hi = if respect_box {
            model.col_upper[a.col]
        } else {
            INF
        };
        for r in &a.rules {
            let v = r.constant
                + r.loads.iter().map(|&(g, c)| c * loads[g]).sum::<f64>()
                + r.aux.iter().map(|&(j, c)| c * aux[j]).sum::<f64>();
            if r.lower {
                lo = lo.max(v)
            } else {
                hi = hi.min(v)
            }
        }
        if lo > hi + 1e-6 {
            return f64::INFINITY;
        }
        aux[a.col] = if a.upper_only { hi } else { lo };
        cost += aux[a.col] * model.col_cost[a.col];
    }
    cost
}

// Repack independent round/day demands into balanced session capacities.
// Each demand uses at most one ring per session. The original checker remains
// authoritative for constraints outside this detected scheduling shape.
fn rebalance(model: &Model, sh: &Shape, point: &mut [f64], loads: &mut [f64]) -> Option<()> {
    let mut days = HashMap::<Vec<usize>, Vec<(f64, Vec<usize>)>>::new();
    for block in &sh.blocks {
        for &r in &block.rows {
            if model.row_lower[r] != 0. || model.row_upper[r] != 0. {
                continue;
            }
            let vars: Vec<_> = row(model, r)
                .filter_map(|(j, a)| (a == 1. && sh.xr_x[j].is_some()).then_some(j))
                .collect();
            if vars.is_empty() || row(model, r).any(|(j, a)| a > 0. && sh.xr_x[j].is_none()) {
                continue;
            }
            let amount: f64 = vars.iter().map(|&j| point[j]).sum();
            if amount <= 0. {
                continue;
            }
            let mut sessions: Vec<_> = vars
                .iter()
                .map(|&j| sh.groups[sh.column_groups[j].unwrap()].session)
                .collect();
            sessions.sort_unstable();
            sessions.dedup();
            days.entry(sessions).or_default().push((amount, vars));
        }
    }
    if days.is_empty() {
        return None;
    }
    for (sessions, mut demands) in days {
        let groups: Vec<_> = sessions
            .iter()
            .flat_map(|&s| sh.sessions[s].iter().copied())
            .collect();
        let total: f64 = demands.iter().map(|v| v.0).sum();
        let base = (total / groups.len() as f64).floor();
        let extra = (total - base * groups.len() as f64).round() as usize;
        let mut room = vec![0.; sh.groups.len()];
        for (k, &g) in groups.iter().enumerate() {
            room[g] = base + if k < extra { 1. } else { 0. };
        }
        demands.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (amount, vars) in demands {
            for &j in &vars {
                point[j] = 0.;
            }
            // Consume bin residues before whole chunks leave small gaps in
            // every ring. A round may split across the two sessions.
            let residual = vars.iter().copied().find_map(|a| {
                let ga = sh.column_groups[a].unwrap();
                let first = room[ga] % amount;
                if first <= 0. {
                    return None;
                }
                vars.iter()
                    .copied()
                    .find(|&b| {
                        let gb = sh.column_groups[b].unwrap();
                        sh.groups[ga].session != sh.groups[gb].session && room[gb] >= amount - first
                    })
                    .map(|b| (a, b, first))
            });
            if let Some((a, b, first)) = residual {
                point[a] = first;
                point[b] = amount - first;
                room[sh.column_groups[a].unwrap()] -= first;
                room[sh.column_groups[b].unwrap()] -= amount - first;
                continue;
            }
            let best = vars
                .iter()
                .copied()
                .filter(|&j| room[sh.column_groups[j].unwrap()] >= amount)
                .min_by(|&a, &b| {
                    room[sh.column_groups[a].unwrap()]
                        .total_cmp(&room[sh.column_groups[b].unwrap()])
                });
            if let Some(j) = best {
                point[j] = amount;
                room[sh.column_groups[j].unwrap()] -= amount;
                continue;
            }
            let pair = vars.iter().copied().find_map(|a| {
                vars.iter()
                    .copied()
                    .find(|&b| {
                        let ga = sh.column_groups[a].unwrap();
                        let gb = sh.column_groups[b].unwrap();
                        sh.groups[ga].session != sh.groups[gb].session
                            && room[ga] > 0.
                            && room[gb] > 0.
                            && room[ga] + room[gb] >= amount
                    })
                    .map(|b| (a, b))
            });
            let (a, b) = pair?;
            let ga = sh.column_groups[a].unwrap();
            let gb = sh.column_groups[b].unwrap();
            let first = amount.min(room[ga]);
            point[a] = first;
            point[b] = amount - first;
            room[ga] -= first;
            room[gb] -= amount - first;
        }
    }
    loads.fill(0.);
    for g in &sh.groups {
        for &(j, _) in &g.feeders {
            point[j] = 0.;
        }
    }
    for j in 0..point.len() {
        if let Some(x) = sh.xr_x[j] {
            point[x] += point[j];
        }
    }
    for (g, group) in sh.groups.iter().enumerate() {
        for &(j, d) in &group.feeders {
            point[sh.x_z[j]?] = if point[j] > 0. { 1. } else { 0. };
            loads[g] += point[j] * d;
        }
    }
    Some(())
}

/// A dual row-combination certificate. Multipliers are heuristic; the final
/// bound uses outward-rounded interval arithmetic over every original column.
/// It remains valid even when pattern detection or symmetry assumptions fail.
pub(crate) fn row_bound(model: &Model) -> Option<f64> {
    let mut reduced = model.col_cost.clone();
    let mut weights = vec![0.; model.num_row];
    let mut peaks = HashMap::<usize, Vec<usize>>::new();
    for r in 0..model.num_row {
        if model.row_upper[r].abs() >= INF / 2. {
            continue;
        }
        let Some((peak, coefficient)) =
            only(row(model, r).filter(|&(j, _)| !model.col_type[j].is_integral()))
        else {
            continue;
        };
        if coefficient == -1.
            && model.col_cost[peak] > 0.
            && row(model, r).all(|(j, a)| j == peak || (model.col_type[j].is_integral() && a > 0.))
        {
            peaks.entry(peak).or_default().push(r);
        }
    }
    for (j, rows) in peaks {
        let w = model.col_cost[j] / rows.len() as f64;
        for r in rows {
            weights[r] += w;
            for (j, a) in row(model, r) {
                reduced[j] += w * a;
            }
        }
    }
    for r in 0..model.num_row {
        if model.row_lower[r] != model.row_upper[r] || model.row_lower[r].abs() >= INF / 2. {
            continue;
        }
        if row_len(model, r) == 0
            || row(model, r).any(|(j, a)| !model.col_type[j].is_integral() || a <= 0.)
        {
            continue;
        }
        let shift = row(model, r)
            .map(|(j, a)| reduced[j] / a)
            .min_by(f64::total_cmp)?;
        weights[r] -= shift;
        for (j, a) in row(model, r) {
            reduced[j] -= shift * a;
        }
    }
    let mut lo = model.col_cost.clone();
    let mut hi = lo.clone();
    let mut bound = 0.;
    for (r, &w) in weights.iter().enumerate() {
        if w == 0. {
            continue;
        }
        let rhs = if w > 0. {
            model.row_upper[r]
        } else {
            model.row_lower[r]
        };
        if rhs.abs() >= INF / 2. || !w.is_finite() {
            return None;
        }
        if rhs != 0. {
            bound = (bound + (-w * rhs).next_down()).next_down();
        }
        for (j, a) in row(model, r) {
            if a == 0. {
                continue;
            }
            let v = w * a;
            lo[j] = (lo[j] + v.next_down()).next_down();
            hi[j] = (hi[j] + v.next_up()).next_up();
        }
    }
    for j in 0..model.num_col {
        if lo[j] == 0. && hi[j] == 0. {
            continue;
        }
        let l = model.col_lower[j];
        let u = model.col_upper[j];
        if (l <= -INF / 2. && hi[j] > 0.) || (u >= INF / 2. && lo[j] < 0.) {
            return None;
        }
        let term = [lo[j] * l, lo[j] * u, hi[j] * l, hi[j] * u]
            .into_iter()
            .min_by(f64::total_cmp)?
            .next_down();
        bound = (bound + term).next_down();
    }
    bound.is_finite().then_some(bound)
}

/// Integer peak certificate for models whose one-hot choices fix each
/// session's total match count. All options come from original equalities;
/// other constraints are relaxed, never used to exclude certificate states.
pub(crate) fn scheduling_bound(model: &Model, clock: &dyn Clock, deadline: f64) -> Option<f64> {
    if model.num_col > 2000 {
        return None;
    }
    let mut seen = vec![0usize; model.num_col];
    for r in 0..model.num_row {
        for (j, _) in row(model, r) {
            if seen[j] == r + 1 {
                return None;
            }
            seen[j] = r + 1;
        }
    }
    let sh = detect(model, clock, deadline, 0)?;
    if sh.blocks.len() > 40 || sh.sessions.len() > 8 {
        return None;
    }
    let mut unit = [0.; 8];
    for (session, groups) in sh.sessions.iter().enumerate() {
        unit[session] = sh.groups[groups[0]].feeders[0].1;
        if groups
            .iter()
            .flat_map(|&g| &sh.groups[g].feeders)
            .any(|&(_, d)| d != unit[session])
        {
            return None;
        }
    }
    let mut choice_cols = std::collections::HashSet::new();
    let mut options = Vec::new();
    for block in &sh.blocks {
        let choices = block.rows.iter().find_map(|&r| {
            (model.row_lower[r] == 1.
                && model.row_upper[r] == 1.
                && row_len(model, r) != 0
                && row(model, r).all(|(j, a)| {
                    a == 1.
                        && sh.column_groups[j].is_none()
                        && model.col_type[j].is_integral()
                        && model.col_lower[j] >= 0.
                        && model.col_upper[j] <= 1.
                }))
            .then(|| row(model, r).map(|(j, _)| j).collect::<Vec<_>>())
        })?;
        choice_cols.extend(choices.iter().copied());
        let mut expected = std::collections::HashSet::new();
        for group in &sh.groups {
            for &(x, _) in &group.feeders {
                if !block.cols.contains(&x) {
                    continue;
                }
                if !model.col_type[x].is_integral() || model.col_lower[x] < 0. {
                    return None;
                }
                let xr: Vec<_> = block
                    .cols
                    .iter()
                    .copied()
                    .filter(|&j| sh.xr_x[j] == Some(x))
                    .collect();
                if xr.is_empty()
                    || !block.rows.iter().any(|&r| {
                        model.row_lower[r] == 0.
                            && model.row_upper[r] == 0.
                            && row_len(model, r) == xr.len() + 1
                            && row(model, r)
                                .all(|(j, a)| (j == x && a == 1.) || (xr.contains(&j) && a == -1.))
                    })
                {
                    return None;
                }
                expected.extend(xr);
            }
        }
        let mut demand_rows = Vec::new();
        let mut covered = std::collections::HashSet::new();
        for &r in &block.rows {
            if model.row_lower[r] != 0. || model.row_upper[r] != 0. {
                continue;
            }
            let vars: Vec<_> = row(model, r)
                .filter_map(|(j, a)| (a > 0.).then_some(j))
                .collect();
            if vars.is_empty()
                || row(model, r).any(|(j, a)| {
                    if a > 0. {
                        a != 1. || !expected.contains(&j) || model.col_lower[j] < 0.
                    } else {
                        a != 0. && (!choices.contains(&j) || a.fract() != 0.)
                    }
                })
            {
                continue;
            }
            let session = sh.groups[sh.column_groups[vars[0]]?].session;
            if vars
                .iter()
                .any(|&j| sh.groups[sh.column_groups[j].unwrap()].session != session)
            {
                return None;
            }
            for j in vars {
                if !covered.insert(j) {
                    return None;
                }
            }
            demand_rows.push((r, session));
        }
        if covered != expected {
            return None;
        }
        let mut alternatives = Profiles::<[u32; 8], f64>::default();
        for &choice in &choices {
            if model.col_upper[choice] < 1.
                || choices
                    .iter()
                    .any(|&j| j != choice && model.col_lower[j] > 0.)
            {
                continue;
            }
            let mut profile = [0u32; 8];
            for &(r, session) in &demand_rows {
                let amount = -row(model, r)
                    .filter(|&(j, _)| j == choice)
                    .map(|(_, a)| a)
                    .sum::<f64>();
                if amount < 0. || amount.fract() != 0. || amount > u32::MAX as f64 {
                    return None;
                }
                profile[session] = profile[session].checked_add(amount as u32)?;
            }
            let cost = model.col_cost[choice];
            if alternatives.get(&profile).is_none_or(|&c| cost < c) {
                alternatives.insert(profile, cost);
            }
        }
        if alternatives.is_empty() {
            return None;
        }
        options.push(alternatives);
    }
    let mut states = Profiles::default();
    states.insert([0u32; 8], 0f64);
    for alternatives in options {
        let mut next = Profiles::<[u32; 8], f64>::default();
        for (key, cost) in states {
            for (profile, &extra) in &alternatives {
                let mut total = [0u32; 8];
                for k in 0..8 {
                    total[k] = key[k].checked_add(profile[k])?;
                }
                let value = (cost + extra).next_down();
                next.entry(total)
                    .and_modify(|cost| *cost = cost.min(value))
                    .or_insert(value);
            }
        }
        if next.len() > 50_000 || clock.now_secs() >= deadline {
            return None;
        }
        states = next;
    }
    let mut constant = 0.;
    for j in 0..model.num_col {
        if choice_cols.contains(&j) || sh.peak_cols.contains(&j) || model.col_cost[j] == 0. {
            continue;
        }
        let cost = model.col_cost[j];
        let x = if cost >= 0. {
            model.col_lower[j]
        } else {
            model.col_upper[j]
        };
        if x.abs() >= INF / 2. {
            return None;
        }
        constant = (constant + (cost * x).next_down()).next_down();
    }
    states
        .into_iter()
        .map(|(profile, cost)| {
            let mut bound = (constant + cost).next_down();
            for (session, groups) in sh.sessions.iter().enumerate() {
                let count = profile[session] as usize;
                let peak = (count / groups.len() + usize::from(count % groups.len() != 0)) as f64;
                let term = ((peak * unit[session]).next_down()
                    * model.col_cost[sh.peak_cols[session]])
                    .next_down();
                bound = (bound + term).next_down();
            }
            bound
        })
        .min_by(f64::total_cmp)
}

fn packed_candidate(model: &Model, sh: &Shape, raw: &[f64]) -> Option<(Vec<f64>, Vec<f64>)> {
    let mut point = raw.to_vec();
    let mut loads = vec![0.; sh.groups.len()];
    rebalance(model, sh, &mut point, &mut loads)?;
    for session in 0..sh.sessions.len() {
        session_cost(model, sh, session, &loads, &mut point, true);
    }
    check_solution(model, &point, 1e-9)
        .feasible
        .then_some((point, loads))
}

// The active table needs only a cost and a trace index. Earlier layers live in
// this contiguous arena, rather than copying all 40 choices on each transition.
struct ChoiceTrace {
    parent: u32,
    pattern: u16,
}

fn trace_choices(traces: &[ChoiceTrace], mut trace: u32, blocks: usize) -> [u16; 40] {
    let mut choices = [0; 40];
    for block in (0..blocks).rev() {
        let node = &traces[trace as usize];
        choices[block] = node.pattern;
        trace = node.parent;
    }
    debug_assert_eq!(trace, u32::MAX);
    choices
}

fn session_program_is_local(sh: &Shape, session: usize, columns: usize) -> bool {
    let mut ready = vec![false; columns];
    for aux in &sh.programs[session] {
        for rule in &aux.rules {
            if rule
                .loads
                .iter()
                .any(|&(g, _)| sh.groups[g].session != session)
                || rule.aux.iter().any(|&(j, _)| !ready[j])
            {
                return false;
            }
        }
        ready[aux.col] = true;
    }
    true
}

fn best_scores(scored: &mut Vec<(f64, u32, usize)>, limit: usize) {
    // Include the original iteration position to retain stable tie ordering
    // even though the partition itself is unstable.
    let order = |a: &(f64, u32, usize), b: &(f64, u32, usize)| {
        a.0.total_cmp(&b.0).then_with(|| a.2.cmp(&b.2))
    };
    if scored.len() > limit {
        scored.select_nth_unstable_by(limit, order);
        scored.truncate(limit);
    }
    scored.sort_by(order);
}

fn day_dp(model: &Model, sh: &Shape, clock: &dyn Clock, deadline: f64) -> Option<Vec<f64>> {
    if sh.blocks.len() > 40 || sh.sessions.len() > 8 {
        return None;
    }
    let mut traces = Vec::<ChoiceTrace>::new();
    let mut states = Profiles::default();
    states.insert([0u64; 8], (0., u32::MAX));
    for block in &sh.blocks {
        let mut alternatives = Profiles::<[u64; 8], (f64, usize)>::default();
        for (i, p) in block.patterns.iter().enumerate() {
            let mut totals = [0.; 8];
            for &(g, l) in &p.loads {
                totals[sh.groups[g].session] += l;
            }
            let key = totals.map(f64::to_bits);
            if alternatives.get(&key).is_none_or(|v| p.cost < v.0) {
                alternatives.insert(key, (p.cost, i));
            }
        }
        let mut next = Profiles::<[u64; 8], (f64, u32)>::default();
        for (key, (cost, parent)) in states {
            for (profile, &(extra, i)) in &alternatives {
                let mut total = [0u64; 8];
                for k in 0..8 {
                    total[k] = (f64::from_bits(key[k]) + f64::from_bits(profile[k])).to_bits();
                }
                let value = cost + extra;
                use std::collections::hash_map::Entry;
                match next.entry(total) {
                    Entry::Occupied(mut entry) => {
                        if value < entry.get().0 {
                            let trace = entry.get().1;
                            traces[trace as usize] = ChoiceTrace {
                                parent,
                                pattern: u16::try_from(i).ok()?,
                            };
                            entry.insert((value, trace));
                        }
                    }
                    Entry::Vacant(entry) => {
                        let trace = u32::try_from(traces.len()).ok()?;
                        traces.push(ChoiceTrace {
                            parent,
                            pattern: u16::try_from(i).ok()?,
                        });
                        entry.insert((value, trace));
                    }
                }
            }
        }
        if next.len() > 50_000 || clock.now_secs() >= deadline {
            return None;
        }
        states = next;
    }
    let unit = sh
        .groups
        .iter()
        .flat_map(|g| g.feeders.iter().map(|&(_, d)| d))
        .min_by(f64::total_cmp)?;
    if sh
        .groups
        .iter()
        .flat_map(|g| &g.feeders)
        .any(|&(_, d)| (d / unit - (d / unit).round()).abs() > 1e-9)
    {
        return None;
    }
    let mut scored = Vec::new();
    let mut aux = vec![0.; model.num_col];
    let mut loads = vec![0.; sh.groups.len()];
    // Balanced loads are determined entirely by a session's integer count.
    // Cache only programs whose inputs are local and topologically ordered;
    // otherwise preserve the original scratch-dependent evaluation.
    let cache_sessions =
        (0..sh.sessions.len()).all(|s| session_program_is_local(sh, s, model.num_col));
    let mut session_scores: Vec<Profiles<usize, f64>> = (0..sh.sessions.len())
        .map(|_| Profiles::default())
        .collect();
    for (ordinal, (key, (cost, trace))) in states.into_iter().enumerate() {
        for (session, groups) in sh.sessions.iter().enumerate() {
            let count = (f64::from_bits(key[session]) / unit).round() as usize;
            for (k, &g) in groups.iter().enumerate() {
                loads[g] =
                    (count / groups.len() + usize::from(k < count % groups.len())) as f64 * unit;
            }
        }
        let mut value = cost;
        for session in 0..sh.sessions.len() {
            let count = (f64::from_bits(key[session]) / unit).round() as usize;
            value += if cache_sessions {
                *session_scores[session]
                    .entry(count)
                    .or_insert_with(|| session_cost(model, sh, session, &loads, &mut aux, true))
            } else {
                session_cost(model, sh, session, &loads, &mut aux, true)
            };
        }
        if value.is_finite() {
            scored.push((value, trace, ordinal));
        }
    }
    best_scores(&mut scored, 64);
    let mut best = None;
    for (_, trace, _) in scored {
        let path = trace_choices(&traces, trace, sh.blocks.len());
        let mut raw = vec![0.; model.num_col];
        for (block, i) in sh.blocks.iter().zip(path) {
            for &(j, v) in &block.patterns[i as usize].values {
                raw[block.cols[j]] = v;
            }
        }
        if let Some((point, _)) = packed_candidate(model, sh, &raw) {
            if best
                .as_ref()
                .is_none_or(|x: &Vec<f64>| model.objective(&point) < model.objective(x))
            {
                best = Some(point);
            }
        }
        if clock.now_secs() >= deadline {
            break;
        }
    }
    best
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

struct LoadRow {
    terms: Vec<(usize, f64)>,
    lower: f64,
    upper: f64,
}

// Compile global integer rows that depend only on complete group loads.
// This includes capacities and ring ordering, without assuming equal match
// durations, interchangeable rings, or a particular number of restrictions.
fn load_rows(model: &Model, sh: &Shape) -> Vec<LoadRow> {
    let mut rows = Vec::new();
    let mut duration = vec![0.; model.num_col];
    let mut feeder_group = vec![None; model.num_col];
    for (g, group) in sh.groups.iter().enumerate() {
        for &(j, d) in &group.feeders {
            duration[j] = d;
            feeder_group[j] = Some(g);
        }
    }
    for r in 0..model.num_row {
        let mut coefficients = HashMap::<usize, (f64, usize)>::new();
        let mut valid = true;
        for (j, a) in row(model, r) {
            let Some(g) = feeder_group[j] else {
                valid = false;
                break;
            };
            let scale = a / duration[j];
            let entry = coefficients.entry(g).or_insert((scale, 0));
            if (entry.0 - scale).abs() > 1e-9 {
                valid = false;
                break;
            }
            entry.1 += 1;
        }
        if valid
            && !coefficients.is_empty()
            && coefficients
                .iter()
                .all(|(&g, &(_, count))| count == sh.groups[g].feeders.len())
        {
            let mut terms: Vec<_> = coefficients.into_iter().map(|(g, (a, _))| (g, a)).collect();
            terms.sort_by_key(|&(g, _)| g);
            rows.push(LoadRow {
                terms,
                lower: model.row_lower[r],
                upper: model.row_upper[r],
            });
        }
    }
    rows
}

fn profile_score(
    model: &Model,
    sh: &Shape,
    rows: &[LoadRow],
    loads: &[f64],
    cost: f64,
    aux: &mut [f64],
) -> f64 {
    let mut value = cost;
    for s in 0..sh.sessions.len() {
        value += session_cost(model, sh, s, loads, aux, false);
    }
    // Penalties guide construction only; they never certify feasibility or
    // optimality. The original row checker decides whether to keep a point.
    let mut violation = 0.;
    for row in rows {
        let activity: f64 = row.terms.iter().map(|&(g, a)| a * loads[g]).sum();
        violation += (row.lower - activity).max(0.) + (activity - row.upper).max(0.);
    }
    for program in &sh.programs {
        for a in program {
            violation += (model.col_lower[a.col] - aux[a.col]).max(0.)
                + (aux[a.col] - model.col_upper[a.col]).max(0.);
        }
    }
    value + violation * 1e4
}

fn pattern_point(model: &Model, sh: &Shape, choices: &[usize], loads: &[f64]) -> Option<Vec<f64>> {
    let mut point = vec![0.; model.num_col];
    for (block, &i) in sh.blocks.iter().zip(choices) {
        for &(j, v) in &block.patterns[i].values {
            point[block.cols[j]] = v;
        }
    }
    for s in 0..sh.sessions.len() {
        session_cost(model, sh, s, loads, &mut point, true);
    }
    let check = check_solution(model, &point, 1e-9);
    #[cfg(not(target_arch = "wasm32"))]
    if !check.feasible && std::env::var("OPTIO_BLOCKDEBUG").is_ok() {
        let worst = (0..model.num_row)
            .map(|r| {
                let activity: f64 = row(model, r).map(|(j, a)| a * point[j]).sum();
                (
                    r,
                    (model.row_lower[r] - activity).max(activity - model.row_upper[r]),
                )
            })
            .max_by(|a, b| a.1.total_cmp(&b.1));
        eprintln!(
            "pattern check row {:?}, bounds {} integer {}",
            worst, check.max_bound_violation, check.max_integrality_violation
        );
    }
    check.feasible.then_some(point)
}

// Coordinate descent over checked local patterns. Scores use actual weighted
// ring loads, so mixed durations and asymmetric ring eligibility need no
// match-count approximation or ring-label symmetry.
fn pattern_descent(
    model: &Model,
    sh: &Shape,
    start: Option<&[f64]>,
    clock: &dyn Clock,
    deadline: f64,
    exchanges: bool,
    report: &mut Option<&mut dyn FnMut(&[f64])>,
) -> Option<Vec<f64>> {
    let rows = load_rows(model, sh);
    let mut aux = vec![0.; model.num_col];
    let mut loads = vec![0.; sh.groups.len()];
    let mut choices = Vec::new();
    let mut cost = 0.;
    for block in &sh.blocks {
        let i = start
            .and_then(|x| {
                block.patterns.iter().position(|p| {
                    block.cols.iter().enumerate().all(|(k, &j)| {
                        let v = p
                            .values
                            .iter()
                            .find(|&&(c, _)| c == k)
                            .map_or(0., |&(_, v)| v);
                        (x[j] - v).abs() < 1e-9
                    })
                })
            })
            .unwrap_or_else(|| {
                block
                    .patterns
                    .iter()
                    .enumerate()
                    .min_by(|a, b| a.1.cost.total_cmp(&b.1.cost))
                    .unwrap()
                    .0
            });
        choices.push(i);
        let p = &block.patterns[i];
        cost += p.cost;
        for &(g, l) in &p.loads {
            loads[g] += l;
        }
    }
    let mut best = start
        .filter(|x| check_solution(model, x, 1e-9).feasible)
        .map(<[f64]>::to_vec);
    if let Some(point) = pattern_point(model, sh, &choices, &loads) {
        if best
            .as_ref()
            .is_none_or(|x| model.objective(&point) < model.objective(x))
        {
            best = Some(point);
        }
    }
    let mut score = profile_score(model, sh, &rows, &loads, cost, &mut aux);
    for pass in 0..256 {
        let mut changed = false;
        for k in 0..sh.blocks.len() {
            if clock.now_secs() >= deadline {
                return best;
            }
            // Alternate traversal to avoid a persistent bias towards the first
            // blocks while retaining deterministic tie handling.
            let b = if pass % 2 == 0 {
                k
            } else {
                sh.blocks.len() - 1 - k
            };
            let block = &sh.blocks[b];
            let old = choices[b];
            let previous = &block.patterns[old];
            for &(g, l) in &previous.loads {
                loads[g] -= l;
            }
            cost -= previous.cost;
            let mut chosen = old;
            let mut chosen_score = score;
            for (i, p) in block.patterns.iter().enumerate() {
                for &(g, l) in &p.loads {
                    loads[g] += l;
                }
                let trial = profile_score(model, sh, &rows, &loads, cost + p.cost, &mut aux);
                for &(g, l) in &p.loads {
                    loads[g] -= l;
                }
                if trial < chosen_score - 1e-8 {
                    chosen = i;
                    chosen_score = trial;
                }
            }
            let p = &block.patterns[chosen];
            for &(g, l) in &p.loads {
                loads[g] += l;
            }
            cost += p.cost;
            choices[b] = chosen;
            score = chosen_score;
            changed |= chosen != old;
        }
        if let Some(point) = pattern_point(model, sh, &choices, &loads) {
            if best
                .as_ref()
                .is_none_or(|x| model.objective(&point) < model.objective(x))
            {
                if let Some(f) = report.as_mut() {
                    f(&point);
                }
                best = Some(point);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if std::env::var("OPTIO_BLOCKDEBUG").is_ok() {
            eprintln!(
                "pattern pass {pass}: score {score} feasible {} loads {loads:?}",
                best.is_some()
            );
        }
        if !changed {
            if !exchanges {
                break;
            }
            // A transfer can temporarily violate ordering/capacity while a
            // compensating transfer restores it. Rank alternatives without
            // global load-row penalties, then score each pair together with
            // all hard constraints. These are heuristic shortlists, not an
            // exhaustive search or a lower-bound calculation.
            let mut alternatives = Vec::new();
            for (b, block) in sh.blocks.iter().enumerate() {
                let previous = &block.patterns[choices[b]];
                for &(g, l) in &previous.loads {
                    loads[g] -= l;
                }
                let mut ranked = Vec::new();
                for (i, p) in block.patterns.iter().enumerate() {
                    if i == choices[b] {
                        continue;
                    }
                    for &(g, l) in &p.loads {
                        loads[g] += l;
                    }
                    ranked.push((
                        profile_score(
                            model,
                            sh,
                            &[],
                            &loads,
                            cost - previous.cost + p.cost,
                            &mut aux,
                        ),
                        i,
                    ));
                    for &(g, l) in &p.loads {
                        loads[g] -= l;
                    }
                }
                for &(g, l) in &previous.loads {
                    loads[g] += l;
                }
                ranked.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
                ranked.truncate(16);
                alternatives.push(ranked.into_iter().map(|(_, i)| i).collect::<Vec<_>>());
                if clock.now_secs() >= deadline {
                    return best;
                }
            }
            let mut exchange = None;
            'pairs: for a in 0..sh.blocks.len() {
                let pa = &sh.blocks[a].patterns[choices[a]];
                for b in a + 1..sh.blocks.len() {
                    if clock.now_secs() >= deadline {
                        return best;
                    }
                    let pb = &sh.blocks[b].patterns[choices[b]];
                    for &(g, l) in pa.loads.iter().chain(&pb.loads) {
                        loads[g] -= l;
                    }
                    let base_cost = cost - pa.cost - pb.cost;
                    for &i in &alternatives[a] {
                        let qa = &sh.blocks[a].patterns[i];
                        for &(g, l) in &qa.loads {
                            loads[g] += l;
                        }
                        for &j in &alternatives[b] {
                            let qb = &sh.blocks[b].patterns[j];
                            for &(g, l) in &qb.loads {
                                loads[g] += l;
                            }
                            let trial = profile_score(
                                model,
                                sh,
                                &rows,
                                &loads,
                                base_cost + qa.cost + qb.cost,
                                &mut aux,
                            );
                            for &(g, l) in &qb.loads {
                                loads[g] -= l;
                            }
                            if trial < score - 1e-8 {
                                exchange = Some((a, b, i, j, trial));
                                break;
                            }
                        }
                        for &(g, l) in &qa.loads {
                            loads[g] -= l;
                        }
                        if exchange.is_some() {
                            break;
                        }
                    }
                    for &(g, l) in pa.loads.iter().chain(&pb.loads) {
                        loads[g] += l;
                    }
                    if exchange.is_some() {
                        break 'pairs;
                    }
                }
            }
            let Some((a, b, i, j, trial)) = exchange else {
                break;
            };
            for (b, i) in [(a, i), (b, j)] {
                let old = &sh.blocks[b].patterns[choices[b]];
                for &(g, l) in &old.loads {
                    loads[g] -= l;
                }
                cost -= old.cost;
                let new = &sh.blocks[b].patterns[i];
                for &(g, l) in &new.loads {
                    loads[g] += l;
                }
                cost += new.cost;
                choices[b] = i;
            }
            score = trial;
        }
    }
    best
}

pub(crate) fn improve(
    model: &Model,
    start: Option<&[f64]>,
    clock: &dyn Clock,
    seconds: f64,
) -> Option<Vec<f64>> {
    improve_with_report(model, start, clock, seconds, None)
}

pub(crate) fn improve_with_report(
    model: &Model,
    start: Option<&[f64]>,
    clock: &dyn Clock,
    seconds: f64,
    mut report: Option<&mut dyn FnMut(&[f64])>,
) -> Option<Vec<f64>> {
    let deadline = clock.now_secs() + seconds;
    let mut certificate = RowCertificate { model, bound: None };
    // Try one representative ring assignment per day/round choice first on
    // larger models. Packing removes ring-label permutations. Accept this
    // shortcut only when the original-row certificate proves its optimum;
    // otherwise retain the full sampled-pattern search.
    let mut representative = None;
    if model.num_col > 2000 {
        if let Some(point) = improve_patterns(
            model,
            start,
            clock,
            deadline,
            1,
            &mut certificate,
            false,
            &mut report,
        ) {
            if certificate
                .get()
                .is_some_and(|bound| model.objective(&point) - bound <= 1e-6)
            {
                return Some(point);
            }
            representative = Some(point);
        }
    }
    let seed = match (representative.as_deref(), start) {
        (Some(a), Some(b)) if model.objective(b) < model.objective(a) => Some(b),
        (a, b) => a.or(b),
    };
    let expanded = improve_patterns(
        model,
        seed,
        clock,
        deadline,
        512,
        &mut certificate,
        start.is_some(),
        &mut report,
    );
    expanded.or(representative)
}

// Scoped to one immutable model and planner invocation. The solve pipeline
// still certifies the original model independently of its presolved copy.
struct RowCertificate<'a> {
    model: &'a Model,
    bound: Option<Option<f64>>,
}

impl RowCertificate<'_> {
    fn get(&mut self) -> Option<f64> {
        *self.bound.get_or_insert_with(|| row_bound(self.model))
    }
}

fn improve_patterns(
    model: &Model,
    start: Option<&[f64]>,
    clock: &dyn Clock,
    deadline: f64,
    sample_limit: usize,
    certificate: &mut RowCertificate<'_>,
    exchanges: bool,
    report: &mut Option<&mut dyn FnMut(&[f64])>,
) -> Option<Vec<f64>> {
    let sh = detect(model, clock, deadline, sample_limit)?;
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var("OPTIO_BLOCKDEBUG").is_ok() {
        let classes = sh
            .blocks
            .iter()
            .map(|b| Arc::as_ptr(&b.patterns) as usize)
            .collect::<std::collections::HashSet<_>>();
        eprintln!(
            "blocks {} pattern classes {}",
            sh.blocks.len(),
            classes.len()
        );
    }
    let mut raw = vec![0.; model.num_col];
    for block in &sh.blocks {
        let pattern = block
            .patterns
            .iter()
            .min_by(|a, b| a.cost.total_cmp(&b.cost))?;
        for &(j, v) in &pattern.values {
            raw[block.cols[j]] = v;
        }
    }
    let mut best = packed_candidate(model, &sh, &raw).map(|v| v.0);
    if let Some(point) = best.as_ref() {
        if certificate
            .get()
            .is_some_and(|bound| model.objective(point) - bound <= 1e-6)
        {
            return start
                .is_none_or(|x| model.objective(point) <= model.objective(x) + 1e-9)
                .then(|| point.clone());
        }
    }
    let searched = if sh.blocks.len() > 40 {
        pattern_descent(model, &sh, start, clock, deadline, exchanges, report)
    } else {
        day_dp(model, &sh, clock, deadline)
            .or_else(|| pattern_descent(model, &sh, start, clock, deadline, exchanges, report))
    };
    if let Some(point) = searched {
        if best
            .as_ref()
            .is_none_or(|x| model.objective(&point) < model.objective(x))
        {
            best = Some(point);
        }
    }
    let point = best?;
    start
        .is_none_or(|x| model.objective(&point) <= model.objective(x) + 1e-9)
        .then_some(point)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StdClock, VarType};

    pub(super) fn mixed_model(blocks: usize) -> Model {
        let mut m = Model::new();
        let mut feeders = vec![Vec::new(); 9];
        for b in 0..blocks {
            let duration = [420., 720., 900., 1110.][b % 4];
            let choices: Vec<_> = (0..2)
                .map(|_| {
                    let j = m.add_col(0., 1.);
                    m.set_integrality(j, VarType::Binary);
                    j
                })
                .collect();
            m.set_cost(choices[0], 0.02);
            m.add_row(
                1.,
                1.,
                &choices.iter().map(|j| (j.0, 1.)).collect::<Vec<_>>(),
            );
            for s in 0..3 {
                // Multiple, overlapping eligibility sets, including a single
                // allowed ring. Eligibility differs between blocks/sessions.
                let allowed: &[usize] = match (b + s) % 3 {
                    0 => &[0],
                    1 => &[0, 2],
                    _ => &[1, 2],
                };
                let mut rounds = vec![Vec::new(), Vec::new()];
                let mut day = Vec::new();
                let mut occupied = Vec::new();
                for &ring in allowed {
                    let x = m.add_col(0., 2.);
                    m.set_integrality(x, VarType::Integer);
                    let z = m.add_col(0., 1.);
                    m.set_integrality(z, VarType::Binary);
                    m.add_row(-INF, 0., &[(x.0, 1.), (z.0, -2.)]);
                    let mut link = vec![(x.0, 1.)];
                    for round in &mut rounds {
                        let xr = m.add_col(0., 1.);
                        m.set_integrality(xr, VarType::Integer);
                        link.push((xr.0, -1.));
                        round.push((xr.0, 1.));
                    }
                    m.add_row(0., 0., &link);
                    feeders[s * 3 + ring].push((x.0, duration));
                    day.push((x.0, 1.));
                    occupied.push((z.0, 1.));
                }
                for (r, round) in rounds.iter_mut().enumerate() {
                    // First round starts on day 0 or day 1; the second ends
                    // on day 1 or day 2. This preserves precedence.
                    if s == r {
                        round.push((choices[0].0, -1.));
                    }
                    if s == r + 1 {
                        round.push((choices[1].0, -1.));
                    }
                    m.add_row(0., 0., round);
                }
                for (choice, sessions) in [(choices[0], [0, 1]), (choices[1], [1, 2])] {
                    if sessions.contains(&s) {
                        day.push((choice.0, -1.));
                    }
                }
                m.add_row(0., 0., &day);
                m.add_row(-INF, 1., &occupied);
            }
        }
        for s in 0..3 {
            let peak = m.add_col(0., 1e9);
            m.set_cost(peak, 0.05);
            for ring in 0..3 {
                let terms = &feeders[s * 3 + ring];
                let mut peak_row = terms.clone();
                peak_row.push((peak.0, -1.));
                m.add_row(-INF, 0., &peak_row);
                m.add_row(-INF, blocks as f64 * 1500., terms);
                let over = m.add_col(0., 1e9);
                m.set_cost(over, 3.);
                let mut soft = terms.clone();
                soft.push((over.0, -1.));
                m.add_row(-INF, blocks as f64 * 200., &soft);
            }
        }
        m
    }

    #[test]
    fn mixed_duration_cache_and_single_ring_choices_preserve_actual_loads() {
        let m = mixed_model(12);
        let clock = StdClock::new();
        let sh = detect(&m, &clock, clock.now_secs() + 5., 512).unwrap();
        assert_eq!(sh.blocks.len(), 12);
        for block in &sh.blocks {
            for p in block.patterns.iter() {
                for (g, group) in sh.groups.iter().enumerate() {
                    let actual: f64 = group
                        .feeders
                        .iter()
                        .map(|&(j, d)| {
                            block.cols.binary_search(&j).ok().map_or(0., |k| {
                                d * p
                                    .values
                                    .iter()
                                    .find(|&&(c, _)| c == k)
                                    .map_or(0., |&(_, v)| v)
                            })
                        })
                        .sum();
                    let cached = p
                        .loads
                        .iter()
                        .find(|&&(h, _)| h == g)
                        .map_or(0., |&(_, l)| l);
                    assert_eq!(actual, cached);
                }
            }
        }
    }

    #[test]
    fn large_mixed_plan_checks_multiple_restrictions_and_hard_and_soft_limits() {
        let m = mixed_model(48);
        let clock = StdClock::new();
        let point = improve(&m, None, &clock, 5.).expect("mixed plan");
        assert!(check_solution(&m, &point, 1e-9).feasible);
        let improved = improve(&m, Some(&point), &clock, 0.1).expect("retain or improve plan");
        assert!(check_solution(&m, &improved, 1e-9).feasible);
        assert!(m.objective(&improved) <= m.objective(&point) + 1e-9);
    }

    #[test]
    fn soft_penalties_never_allow_a_candidate_through_impossible_hard_limits() {
        let mut m = mixed_model(48);
        for r in 0..m.num_row {
            if row(&m, r).all(|(j, a)| m.col_type[j].is_integral() && a > 1.) {
                m.row_upper[r] = 0.;
            }
        }
        let clock = StdClock::new();
        assert!(improve(&m, None, &clock, 0.1).is_none());
    }

    #[test]
    fn exchange_search_reports_only_checked_improving_candidates() {
        let m = mixed_model(12);
        let clock = StdClock::new();
        let sh = detect(&m, &clock, clock.now_secs() + 5., 512).unwrap();
        let choices = vec![0; sh.blocks.len()];
        let mut loads = vec![0.; sh.groups.len()];
        for block in &sh.blocks {
            for &(g, l) in &block.patterns[0].loads {
                loads[g] += l;
            }
        }
        let start = pattern_point(&m, &sh, &choices, &loads).unwrap();
        let mut objectives = vec![m.objective(&start)];
        let mut report = |point: &[f64]| {
            assert!(check_solution(&m, point, 1e-9).feasible);
            let objective = m.objective(point);
            assert!(objective < *objectives.last().unwrap());
            objectives.push(objective);
        };
        let best = pattern_descent(
            &m,
            &sh,
            Some(&start),
            &clock,
            clock.now_secs() + 1.,
            true,
            &mut Some(&mut report),
        )
        .unwrap();
        assert!(objectives.len() > 1);
        assert!(m.objective(&best) <= *objectives.last().unwrap() + 1e-9);
    }

    #[test]
    fn candidate_selection_matches_stable_full_sort_with_ties() {
        for size in [0, 1, 63, 64, 65, 500] {
            let mut scores: Vec<_> = (0..size)
                .map(|i| (((i * 37) % 11) as f64, i as u32, i))
                .collect();
            let mut expected = scores.clone();
            expected.sort_by(|a, b| a.0.total_cmp(&b.0));
            expected.truncate(64);
            best_scores(&mut scores, 64);
            assert_eq!(scores, expected);
        }
    }

    #[test]
    fn session_cache_rejects_cross_session_and_forward_dependencies() {
        let rule = |target, inputs| Rule {
            target,
            lower: true,
            constant: 0.,
            loads: Vec::new(),
            aux: inputs,
        };
        let mut sh = Shape {
            groups: Vec::new(),
            sessions: vec![Vec::new(), Vec::new()],
            blocks: Vec::new(),
            programs: vec![
                vec![Aux {
                    col: 0,
                    upper_only: false,
                    rules: vec![rule(0, Vec::new())],
                }],
                vec![Aux {
                    col: 1,
                    upper_only: false,
                    rules: vec![rule(1, vec![(0, 1.)])],
                }],
            ],
            peak_cols: Vec::new(),
            column_groups: Vec::new(),
            xr_x: Vec::new(),
            x_z: Vec::new(),
        };
        assert!(session_program_is_local(&sh, 0, 2));
        assert!(!session_program_is_local(&sh, 1, 2));
        sh.programs[1][0].rules[0].aux = vec![(1, 1.)];
        assert!(!session_program_is_local(&sh, 1, 2));
        sh.programs[1][0].rules[0].aux.clear();
        assert!(session_program_is_local(&sh, 1, 2));
    }

    fn fixture(name: &str) -> Model {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../harness/fixtures")
            .join(format!("planning-app-{name}.model.json"));
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let f = |key: &str| {
            v[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_f64().unwrap())
                .collect::<Vec<_>>()
        };
        let costs = f("colCost");
        let lo = f("colLower");
        let hi = f("colUpper");
        let rl = f("rowLower");
        let ru = f("rowUpper");
        let mut m = Model::new();
        for j in 0..costs.len() {
            let id = m.add_col(lo[j], hi[j]);
            m.set_cost(id, costs[j]);
            m.set_integrality(
                id,
                match v["integrality"][j].as_u64().unwrap() {
                    0 => VarType::Continuous,
                    2 => VarType::Binary,
                    _ => VarType::Integer,
                },
            );
        }
        let a = &v["matrix"];
        for r in 0..rl.len() {
            let start = a["starts"][r].as_u64().unwrap() as usize;
            let end = a["starts"][r + 1].as_u64().unwrap() as usize;
            let terms = (start..end)
                .map(|k| {
                    (
                        a["indices"][k].as_u64().unwrap() as u32,
                        a["values"][k].as_f64().unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            m.add_row(rl[r], ru[r], &terms);
        }
        m
    }
    #[test]
    fn identical_templates_share_patterns_and_medium_plan_is_checked() {
        let m = fixture("medium");
        let clock = StdClock::new();
        let shape = detect(&m, &clock, clock.now_secs() + 30., 512).unwrap();
        let classes = shape
            .blocks
            .iter()
            .map(|b| Arc::as_ptr(&b.patterns) as usize)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(shape.blocks.len(), 28);
        assert_eq!(classes.len(), 6);
        let x = improve(&m, None, &clock, 10.).expect("unseeded medium plan");
        assert!(check_solution(&m, &x, 1e-6).feasible);
        assert!((m.objective(&x) - 3096.8).abs() < 1e-9);
    }
    #[test]
    fn large_plan_respects_capacity_and_ring_order_without_seed() {
        let m = fixture("large");
        let clock = StdClock::new();
        let x = improve(&m, None, &clock, 30.).expect("unseeded large plan");
        assert!(check_solution(&m, &x, 1e-6).feasible);
        assert!(m.objective(&x) < 10000.);
    }
    #[test]
    fn row_certificate_never_exceeds_exhaustive_optimum() {
        for seed in 0..64 {
            let mut m = Model::new();
            let mut vars = Vec::new();
            for j in 0..4 {
                let v = m.add_col(0., 1.);
                m.set_integrality(v, VarType::Binary);
                m.set_cost(v, ((seed * 17 + j * 7) % 11) as f64 - 5.);
                vars.push(v);
            }
            let p = m.add_col(0., 1e9);
            m.set_cost(p, 0.05);
            let mut durations = Vec::new();
            for (j, &v) in vars.iter().enumerate() {
                let d = ((seed * 3 + j * 5) % 13 + 1) as f64;
                durations.push(d);
                m.add_row(-INF, 0., &[(v.0, d), (p.0, -1.)]);
            }
            m.add_row(2., 2., &vars.iter().map(|v| (v.0, 1.)).collect::<Vec<_>>());
            let mut optimum = f64::INFINITY;
            for mask in 0u32..16 {
                if mask.count_ones() != 2 {
                    continue;
                }
                let mut x = vec![0.; 5];
                for j in 0..4 {
                    x[j] = ((mask >> j) & 1) as f64;
                }
                x[4] = (0..4).map(|j| x[j] * durations[j]).fold(0., f64::max);
                assert!(check_solution(&m, &x, 1e-9).feasible);
                optimum = optimum.min(m.objective(&x));
            }
            assert!(row_bound(&m).unwrap() <= optimum, "seed {seed}");
        }
    }
    #[test]
    fn large_optimum_is_certified_on_original_rows_with_zero_requested_gap() {
        let m = fixture("large");
        let clock = StdClock::new();
        let options = crate::Options {
            time_limit_secs: 2.,
            mip_rel_gap: 0.,
            mip_abs_gap: 0.,
            threads: 1,
            ..Default::default()
        };
        let sol = crate::solve(&m, &options, &clock);
        assert_eq!(sol.status, crate::ModelStatus::Optimal);
        assert!(check_solution(&m, &sol.col_value, 1e-9).feasible);
        assert!((sol.objective - 4028.4).abs() < 1e-9);
        let bound = sol.dual_bound.unwrap();
        assert!(bound <= sol.objective);
        assert!(sol.objective - bound < 1e-6);
        assert_eq!(sol.nodes, 0);
    }
    #[test]
    fn medium_integer_peak_certificate_closes_the_real_gap() {
        let m = fixture("medium");
        let clock = StdClock::new();
        let bound = scheduling_bound(&m, &clock, clock.now_secs() + 5.).unwrap();
        assert!(bound <= 3096.8);
        assert!(3096.8 - bound < 1e-8);
        let options = crate::Options {
            time_limit_secs: 2.,
            mip_rel_gap: 0.,
            mip_abs_gap: 0.,
            threads: 1,
            ..Default::default()
        };
        let sol = crate::solve(&m, &options, &clock);
        assert_eq!(sol.status, crate::ModelStatus::Optimal);
        assert_eq!(sol.nodes, 0);
        assert!(check_solution(&m, &sol.col_value, 1e-9).feasible);
        assert!((sol.objective - 3096.8).abs() < 1e-9);
    }
    #[test]
    fn integer_certificate_rejects_incomplete_round_definitions() {
        let mut m = fixture("medium");
        let clock = StdClock::new();
        let sh = detect(&m, &clock, clock.now_secs() + 5., 512).unwrap();
        let r = (0..m.num_row)
            .find(|&r| {
                m.row_lower[r] == 0.
                    && m.row_upper[r] == 0.
                    && row(&m, r).any(|(j, a)| a == -1. && sh.xr_x[j].is_some())
            })
            .unwrap();
        let k = (m.a_start[r] as usize..m.a_start[r + 1] as usize)
            .find(|&k| m.a_value[k] == -1.)
            .unwrap();
        m.a_value[k] = -2.;
        assert!(scheduling_bound(&m, &clock, clock.now_secs() + 5.).is_none());
    }
    #[test]
    fn seeded_real_app_plan_is_improved_and_proved() {
        let m = fixture("medium");
        let clock = StdClock::new();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../harness/fixtures/planning-app-medium.start.json");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let mut start = vec![0.; m.num_col];
        for (j, value) in v["indices"]
            .as_array()
            .unwrap()
            .iter()
            .zip(v["values"].as_array().unwrap())
        {
            start[j.as_u64().unwrap() as usize] = value.as_f64().unwrap();
        }
        assert!(check_solution(&m, &start, 1e-9).feasible);
        let options = crate::Options {
            time_limit_secs: 2.,
            mip_rel_gap: 0.,
            mip_abs_gap: 0.,
            threads: 1,
            ..Default::default()
        };
        let mut incumbents = Vec::new();
        let mut on_incumbent = |inc: &crate::Incumbent| incumbents.push(inc.objective);
        let mut callbacks = crate::Callbacks {
            on_incumbent: Some(&mut on_incumbent),
            ..Default::default()
        };
        let sol = crate::solve_with_start(&m, &options, &clock, &mut callbacks, Some(&start));
        assert_eq!(sol.status, crate::ModelStatus::Optimal);
        assert_eq!(sol.nodes, 0);
        assert!((sol.objective - 3096.8).abs() < 1e-9);
        assert!(sol.local_search_secs > 0.);
        assert_eq!(incumbents.len(), 2);
        assert!((incumbents[0] - 4979.4).abs() < 1e-8);
        assert!((incumbents[1] - 3096.8).abs() < 1e-8);
    }
    #[test]
    fn a_checked_start_is_never_replaced_by_a_worse_plan() {
        let m = fixture("medium");
        let clock = StdClock::new();
        let start = improve(&m, None, &clock, 10.).unwrap();
        if let Some(x) = improve(&m, Some(&start), &clock, 10.) {
            assert!(check_solution(&m, &x, 1e-6).feasible);
            assert!(m.objective(&x) <= m.objective(&start) + 1e-7);
        }
    }
}
