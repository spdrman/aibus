//! The definition graph of a workflow (#283, spec section 4.2): route conditions, and the static
//! checks `Workflow::parse` makes before anything can run. A chain needs little of this; routes,
//! parents, native steps and loops need all of it.
//!
//! The graph is the steps aibus schedules (every step but a native subagent) joined by the routes.
//! A route with `max_iterations` goes back to an earlier step and is a loop; every other route goes
//! forward, and the forward routes must make a DAG that starts at the first step and reaches every
//! scheduled step.

use std::fmt;

use super::{jev, reads, var, Mode, Output, StepIsolation, Var, Workflow, WorkflowError};

/// The most steps a workflow that uses any field added in #283 may have.
pub const MAX_STEPS: usize = 256;

/// What a route condition can say about a step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pred {
    Completed,
    Failed,
    /// The step's JSON artifact parsed; only a step with `output = "json"` has one.
    ArtifactValid,
}

/// A route's `when`: `<step>.<completed|failed|artifact_valid>` joined with `!`, `&&` and `||`
/// (`&&` binds tighter; there are no parentheses).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Cond {
    Is { step: String, what: Pred },
    Not(Box<Cond>),
    All(Vec<Cond>),
    Any(Vec<Cond>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tok<'a> {
    Not,
    And,
    Or,
    Word(&'a str),
}

impl Cond {
    pub fn parse(text: &str) -> Result<Self, String> {
        // The longest `when` a route may have.
        const MAX_WHEN_BYTES: usize = 256;
        if text.len() > MAX_WHEN_BYTES {
            return Err(format!(
                "{} bytes, the most is {MAX_WHEN_BYTES}",
                text.len()
            ));
        }
        let bytes = text.as_bytes();
        let mut toks = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            let word = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-.".contains(&c);
            match c {
                b' ' | b'\t' => i += 1,
                b'!' => {
                    toks.push(Tok::Not);
                    i += 1;
                }
                b'&' if bytes.get(i + 1) == Some(&b'&') => {
                    toks.push(Tok::And);
                    i += 2;
                }
                b'|' if bytes.get(i + 1) == Some(&b'|') => {
                    toks.push(Tok::Or);
                    i += 2;
                }
                c if word(c) => {
                    let start = i;
                    while i < bytes.len() && word(bytes[i]) {
                        i += 1;
                    }
                    toks.push(Tok::Word(&text[start..i]));
                }
                _ => {
                    let ch = text[i..].chars().next().unwrap_or('?');
                    return Err(format!("unexpected `{}` at byte {i}", ch.escape_default()));
                }
            }
        }
        any(&toks)
    }

    /// Every `<step>.<pred>` in the condition, in order.
    pub fn atoms(&self) -> Vec<(&str, Pred)> {
        match self {
            Cond::Is { step, what } => vec![(step.as_str(), *what)],
            Cond::Not(c) => c.atoms(),
            Cond::All(cs) | Cond::Any(cs) => cs.iter().flat_map(Cond::atoms).collect(),
        }
    }
}

fn any(toks: &[Tok<'_>]) -> Result<Cond, String> {
    let parts = toks
        .split(|t| *t == Tok::Or)
        .map(all)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(one_or(parts, Cond::Any))
}

fn all(toks: &[Tok<'_>]) -> Result<Cond, String> {
    let parts = toks
        .split(|t| *t == Tok::And)
        .map(unary)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(one_or(parts, Cond::All))
}

fn one_or(mut parts: Vec<Cond>, many: fn(Vec<Cond>) -> Cond) -> Cond {
    if parts.len() == 1 {
        parts.remove(0)
    } else {
        many(parts)
    }
}

fn unary(toks: &[Tok<'_>]) -> Result<Cond, String> {
    const WANT: &str =
        "want <step>.completed, <step>.failed or <step>.artifact_valid, joined with !, && and ||";
    match toks {
        [Tok::Not, rest @ ..] => Ok(Cond::Not(Box::new(unary(rest)?))),
        [Tok::Word(w)] => {
            let (step, what) = w.rsplit_once('.').ok_or_else(|| format!("`{w}`: {WANT}"))?;
            let what = match what {
                "completed" => Pred::Completed,
                "failed" => Pred::Failed,
                "artifact_valid" => Pred::ArtifactValid,
                _ => return Err(format!("`{w}`: {WANT}")),
            };
            crate::ids::AgentName::new(step).map_err(|_| format!("`{w}`: {WANT}"))?;
            Ok(Cond::Is {
                step: step.to_owned(),
                what,
            })
        }
        _ => Err(WANT.to_owned()),
    }
}

/// The canonical spelling, which is what a plan records.
impl fmt::Display for Cond {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cond::Is { step, what } => {
                let what = match what {
                    Pred::Completed => "completed",
                    Pred::Failed => "failed",
                    Pred::ArtifactValid => "artifact_valid",
                };
                write!(f, "{step}.{what}")
            }
            Cond::Not(c) => write!(f, "!{c}"),
            Cond::All(cs) | Cond::Any(cs) => {
                let join = if matches!(self, Cond::All(_)) {
                    " && "
                } else {
                    " || "
                };
                let parts: Vec<String> = cs.iter().map(ToString::to_string).collect();
                f.write_str(&parts.join(join))
            }
        }
    }
}

/// The forward routes as adjacency, and for every step the steps that run before it
/// (`before[i][j]`: `j` is an ancestor of `i` through forward routes). Only meaningful for a
/// workflow with routes.
pub struct Dag {
    pub next: Vec<Vec<usize>>,
    pub before: Vec<Vec<bool>>,
    /// The steps in an order where every forward route goes forward. Shorter than the steps when
    /// the routes have a cycle: what is missing is on or after it, and `before` is then empty.
    pub order: Vec<usize>,
}

impl Dag {
    pub fn of(wf: &Workflow) -> Self {
        let n = wf.steps.len();
        let mut next = vec![Vec::new(); n];
        let mut into = vec![0usize; n];
        for r in wf.routes.iter().filter(|r| r.max_iterations.is_none()) {
            if let (Some(f), Some(t)) = (wf.index(&r.from), wf.index(&r.to)) {
                if !next[f].contains(&t) {
                    next[f].push(t);
                    into[t] += 1;
                }
            }
        }
        // Kahn's algorithm, smallest index first so the order is the file's where it can be.
        let mut ready: std::collections::BTreeSet<usize> =
            (0..n).filter(|&i| into[i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(i) = ready.pop_first() {
            order.push(i);
            for &t in &next[i] {
                into[t] -= 1;
                if into[t] == 0 {
                    ready.insert(t);
                }
            }
        }
        let mut before = Vec::new();
        if order.len() == n {
            before = vec![vec![false; n]; n];
            for &i in &order {
                let row = before[i].clone();
                for &t in &next[i] {
                    for (b, r) in before[t].iter_mut().zip(&row) {
                        *b |= *r;
                    }
                    before[t][i] = true;
                }
            }
        }
        Dag {
            next,
            before,
            order,
        }
    }
}

/// Everything about the file as a whole that `parse` can't see one step at a time.
pub(super) fn check(wf: &Workflow) -> Result<(), WorkflowError> {
    let n = wf.steps.len();
    let step = |i: usize, why: String| WorkflowError::Step { step: i + 1, why };
    let file = WorkflowError::Invalid;
    let routed = !wf.routes.is_empty();

    let new_fields = routed
        || wf.revision.is_some()
        || wf.limits.is_some()
        || !wf.approvals.is_empty()
        || wf.steps.iter().any(|s| {
            s.id.is_some()
                || s.parent.is_some()
                || s.mode != Mode::AibusManaged
                || s.runtime != super::Runtime::Aibus
                || !s.requires.is_empty()
                || s.isolation != StepIsolation::None
                || s.output != Output::Summary
        });
    if new_fields && n > MAX_STEPS {
        return Err(file(format!(
            "{n} steps: a workflow that uses routes, ids or the other fields added in #283 has at most {MAX_STEPS}"
        )));
    }
    if let Some(r) = &wf.revision {
        let ok = !r.is_empty()
            && r.len() <= 64
            && r.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c));
        if !ok {
            return Err(file(format!(
                "revision {:?}: want up to 64 letters, digits, `.`, `_`, `+` or `-`",
                r.chars().take(80).collect::<String>()
            )));
        }
    }
    for (i, s) in wf.steps.iter().enumerate() {
        if let Some(id) = &s.id {
            if let Some(j) = (0..i).find(|&j| wf.steps[j].id.as_ref() == Some(id)) {
                return Err(step(
                    i,
                    format!("id `{id}` is already the id of step {}", j + 1),
                ));
            }
            if let Some(j) = (0..n).find(|&j| j != i && wf.steps[j].agent == *id) {
                return Err(step(
                    i,
                    format!(
                        "id `{id}` is the agent of step {}, so `steps.{id}.summary` could mean either",
                        j + 1
                    ),
                ));
            }
        }
        for (k, c) in s.requires.iter().enumerate() {
            if s.requires[..k].contains(c) {
                return Err(step(i, format!("requires lists {} twice", c.as_str())));
            }
        }
    }
    let native: Vec<bool> = (0..n).map(|i| wf.native(i)).collect();
    for (i, s) in wf.steps.iter().enumerate() {
        let parent = match &s.parent {
            None => None,
            Some(p) => match (0..i).find(|&j| wf.key(j) == p) {
                None => {
                    return Err(step(
                        i,
                        format!(
                            "parent `{}` is not a step that comes before this one",
                            p.chars().take(80).collect::<String>()
                        ),
                    ))
                }
                Some(j) if native[j] => {
                    return Err(step(
                        i,
                        format!(
                            "parent `{p}` is a native subagent; aibus does not nest under those"
                        ),
                    ))
                }
                Some(j) => Some(j),
            },
        };
        if s.mode == Mode::Native {
            let info = jev::runtime_info(s.runtime);
            if !info.capabilities.contains(&"native_child_lineage") {
                return Err(step(
                    i,
                    format!(
                        "mode native needs a runtime with native_child_lineage, and {} has none",
                        info.id
                    ),
                ));
            }
            let Some(p) = parent else {
                return Err(step(
                    i,
                    "a native subagent needs a parent step that launches it".into(),
                ));
            };
            if wf.steps[p].runtime != s.runtime {
                return Err(step(
                    i,
                    format!(
                        "a native subagent runs in its parent's process, so it needs its parent's runtime ({})",
                        wf.steps[p].runtime.as_str()
                    ),
                ));
            }
        }
        if native[i] {
            if wf.key(i).contains('#') {
                return Err(step(
                    i,
                    format!(
                        "two steps run agent {} as native subagents; give them ids, which become their names",
                        s.agent
                    ),
                ));
            }
            if s.gate.is_some() {
                return Err(step(
                    i,
                    "a gate runs after a step aibus schedules; a native subagent is not one".into(),
                ));
            }
            if s.retries > 0 {
                return Err(step(
                    i,
                    "on_fail retries a step aibus schedules; a native subagent is not one".into(),
                ));
            }
            if !reads(&s.prompt).is_empty() {
                return Err(step(
                    i,
                    "a native subagent takes its task from its parent, so its prompt can't use {{...}}".into(),
                ));
            }
            if s.isolation != StepIsolation::None {
                return Err(step(
                    i,
                    "isolation = \"git_worktree\" needs an aibus-managed step; a native subagent shares its parent's process".into(),
                ));
            }
            if s.output != Output::Summary {
                return Err(step(
                    i,
                    "output = \"json\" is for a step aibus schedules; a native subagent reports to its parent".into(),
                ));
            }
        }
        if routed && s.retries > 0 {
            return Err(step(
                i,
                "on_fail retries the step before in a chain; with routes, write a route back with max_iterations".into(),
            ));
        }
        // Nobody reads a native subagent's summary: aibus never gets one of its own.
        for key in reads(&s.prompt) {
            let Some(Var::Step(name)) = var(&key) else {
                continue;
            };
            let j = if routed {
                wf.index(&name)
            } else {
                (0..i).rev().find(|&j| wf.steps[j].answers_to(&name))
            };
            if j.is_some_and(|j| native[j]) {
                return Err(step(
                    i,
                    format!(
                        "`{key}` is a native subagent's; it reports to its parent, not to aibus"
                    ),
                ));
            }
        }
    }
    /// The checks a workflow with routes needs: route ends, cycles, loops, conditions,
    /// reachability and what each step reads.
    fn routes(wf: &Workflow, native: &[bool]) -> Result<(), WorkflowError> {
        /// The most routes a workflow may have.
        const MAX_ROUTES: usize = 1024;
        /// The most times one route back may be taken in a run.
        const MAX_ITERATIONS: u32 = 10;
        let n = wf.steps.len();
        let at = |r: usize, why: String| WorkflowError::Invalid(format!("route {}: {why}", r + 1));
        let short = |s: &str| s.chars().take(80).collect::<String>();
        if wf.routes.len() > MAX_ROUTES {
            return Err(WorkflowError::Invalid(format!(
                "{} routes, the most is {MAX_ROUTES}",
                wf.routes.len()
            )));
        }
        let mut ends = Vec::new();
        for (ri, r) in wf.routes.iter().enumerate() {
            let f = wf
                .index(&r.from)
                .ok_or_else(|| at(ri, format!("`from` names no step: {}", short(&r.from))))?;
            let t = wf
                .index(&r.to)
                .ok_or_else(|| at(ri, format!("`to` names no step: {}", short(&r.to))))?;
            for (end, i) in [("from", f), ("to", t)] {
                if native[i] {
                    return Err(at(
                        ri,
                        format!(
                            "`{end}` is a native subagent ({}); aibus does not schedule those, its parent does",
                            wf.key(i)
                        ),
                    ));
                }
            }
            if let Some(k) = r
                .max_iterations
                .filter(|k| !(1..=MAX_ITERATIONS).contains(k))
            {
                return Err(at(
                    ri,
                    format!("max_iterations {k}: want 1 to {MAX_ITERATIONS}"),
                ));
            }
            if let Some(other) = ends.iter().position(|e| *e == (f, t)) {
                return Err(at(
                    ri,
                    format!("goes from {} to {} like route {}", r.from, r.to, other + 1),
                ));
            }
            ends.push((f, t));
        }
        // The first step is where a run starts; nothing but a loop goes back into it.
        if let Some(ri) = wf
            .routes
            .iter()
            .position(|r| r.max_iterations.is_none() && wf.index(&r.to) == Some(0))
        {
            return Err(at(
                ri,
                "the first step is where the workflow starts; only a loop (max_iterations) may go back to it".into(),
            ));
        }
        let dag = Dag::of(wf);
        if dag.order.len() < n {
            // What Kahn's algorithm could not order: the steps on or after a cycle.
            let names: Vec<&str> = (0..n)
                .filter(|i| !dag.order.contains(i))
                .take(8)
                .map(|i| wf.key(i))
                .collect();
            return Err(WorkflowError::Invalid(format!(
                "the routes make a cycle through {}; a loop needs max_iterations on the route that goes back",
                names.join(", ")
            )));
        }
        for (ri, r) in wf.routes.iter().enumerate() {
            let (f, t) = ends[ri];
            if r.max_iterations.is_some() && !(f == t || dag.before[f][t]) {
                return Err(at(
                    ri,
                    "max_iterations is for a route that goes back to an earlier step".into(),
                ));
            }
            for (name, what) in r.when.atoms() {
                let Some(j) = wf.index(name) else {
                    return Err(at(ri, format!("when: `{name}` names no step")));
                };
                if native[j] {
                    return Err(at(
                        ri,
                        format!("when: `{name}` is a native subagent; aibus sees no outcome of its own for it"),
                    ));
                }
                if j != f && !dag.before[f][j] {
                    return Err(at(
                        ri,
                        format!("when: `{name}` does not run before this route"),
                    ));
                }
                if what == Pred::ArtifactValid && wf.steps[j].output != Output::Json {
                    return Err(at(
                        ri,
                        format!(
                            "when: `{name}.artifact_valid` needs output = \"json\" on step {name}"
                        ),
                    ));
                }
            }
        }
        // Every scheduled step is reached from the first one.
        let mut seen = vec![false; n];
        let mut stack = vec![0];
        while let Some(i) = stack.pop() {
            if !std::mem::replace(&mut seen[i], true) {
                stack.extend(dag.next[i].iter().copied());
            }
        }
        if let Some(i) = (0..n).find(|&i| !seen[i] && !native[i]) {
            return Err(WorkflowError::Invalid(format!(
                "no route reaches step {} from the first step",
                wf.key(i)
            )));
        }
        for (i, s) in wf.steps.iter().enumerate().filter(|(i, _)| !native[*i]) {
            let bad = |why: String| WorkflowError::Step { step: i + 1, why };
            for key in reads(&s.prompt) {
                match var(&key) {
                    Some(Var::Prev) => {
                        let into = dag.next.iter().filter(|next| next.contains(&i)).count();
                        if into != 1 {
                            return Err(bad(
                                "`{{prev.summary}}` needs exactly one route into this step; name the step with {{steps.<key>.summary}}".into(),
                            ));
                        }
                    }
                    Some(Var::Step(name)) => match wf.index(&name) {
                        Some(j) if dag.before[i][j] => {}
                        _ => {
                            return Err(bad(format!(
                                "`{key}` names a step that does not run before this one"
                            )))
                        }
                    },
                    _ => {}
                }
            }
            if let Some(p) = s.parent.as_deref().and_then(|p| wf.index(p)) {
                if !dag.before[i][p] {
                    return Err(bad(format!(
                        "parent `{}` does not run before this step",
                        wf.key(p)
                    )));
                }
            }
        }
        Ok(())
    }
    if routed {
        routes(wf, &native)?;
    }
    if let Some(l) = &wf.limits {
        for (name, value, max) in [
            ("max_agent_runs", l.max_agent_runs, 1000),
            ("max_parallel", l.max_parallel, 64),
            ("max_wall_seconds", l.max_wall_seconds, 604_800),
        ] {
            if let Some(v) = value.filter(|v| !(1..=max).contains(v)) {
                return Err(file(format!("limits.{name} = {v}: want 1 to {max}")));
            }
        }
    }
    for (k, c) in wf.approvals.iter().enumerate() {
        if wf.approvals[..k].contains(c) {
            return Err(file(format!(
                "approvals.required_for lists {} twice",
                c.as_str()
            )));
        }
    }
    if let Some(max) = wf.limits.and_then(|l| l.max_agent_runs) {
        let worst = worst_case_runs(wf);
        if worst > u64::from(max) {
            return Err(file(format!(
                "the workflow can start up to {worst} agent runs, more than limits.max_agent_runs = {max}"
            )));
        }
    }
    Ok(())
}

/// The most agent runs aibus can start for the workflow: every scheduled step once, plus for a
/// chain two runs per retry (the step and the one before it), and for routes, every time a route
/// back may be taken, everything from the step it goes back to onwards. Each route's
/// `max_iterations` counts every time that route is taken in a run. Native subagents are not
/// counted: they run inside their parent's process, where the vendor bounds them.
pub fn worst_case_runs(wf: &Workflow) -> u64 {
    let n = wf.steps.len();
    let scheduled: Vec<bool> = (0..n).map(|i| !wf.native(i)).collect();
    let mut total = scheduled.iter().filter(|s| **s).count() as u64;
    if wf.routes.is_empty() {
        for (i, s) in wf.steps.iter().enumerate().filter(|(i, _)| scheduled[*i]) {
            let pair = if (0..i).any(|j| scheduled[j]) { 2 } else { 1 };
            total = total.saturating_add(u64::from(s.retries) * pair);
        }
        return total;
    }
    let dag = Dag::of(wf);
    for r in &wf.routes {
        let (Some(k), Some(to)) = (r.max_iterations, wf.index(&r.to)) else {
            continue;
        };
        let after = (0..n)
            .filter(|&j| scheduled[j] && (j == to || dag.before.get(j).is_some_and(|b| b[to])))
            .count() as u64;
        total = total.saturating_add(u64::from(k).saturating_mul(after));
    }
    total
}
