use crate::queue::{ClientView, Order};

/// A string attribute a condition can test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrAttr {
    /// Trusted: client label.
    ClientLabel,
    /// Trusted: client key.
    ClientKey,
    /// Trusted: MachineId of the client.
    MachineId,
    /// Declared: source.
    Source,
    /// Declared: project.
    Project,
    /// Declared: label `--as`.
    SubmitterLabel,
    /// Declared: platform the order asks for.
    Platform,
    /// Declared: any tag of the order, `tags` and `exclusive` together.
    Tag,
    /// Declared: any label of the profile.
    Label,
}

/// A numeric attribute a condition can test; all are declared by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumAttr {
    /// Memory in MB.
    MemMb,
    /// CPU count.
    Cpu,
    /// Disk in MB.
    DiskMb,
    /// Timeout in seconds.
    TimeoutS,
}

/// A test of a string attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrTest {
    /// Equal to the value.
    Eq(String),
    /// Different from the value; passes when the attribute is missing.
    Ne(String),
    /// Equal to one of the values.
    In(Vec<String>),
    /// Starts with the value.
    Prefix(String),
    /// Matches the pattern (`*` any run, `?` one character, case-sensitive).
    Glob(String),
}

/// A test of a numeric attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumTest {
    /// Equal.
    Eq(u64),
    /// Different.
    Ne(u64),
    /// Equal to one of the values.
    In(Vec<u64>),
    /// Less than.
    Lt(u64),
    /// Less than or equal.
    Le(u64),
    /// Greater than.
    Gt(u64),
    /// Greater than or equal.
    Ge(u64),
}

/// One attribute test of a condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Term {
    /// A string attribute test.
    Str(StrAttr, StrTest),
    /// A numeric attribute test.
    Num(NumAttr, NumTest),
}

/// `when` of a rule: every set field, every term, some `any` child (when `any` is not empty)
/// and not the `not` child must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Condition {
    /// Trusted: client label.
    pub client_label: Option<String>,
    /// Trusted: client key.
    pub client_key: Option<String>,
    /// Trusted: MachineId of the client.
    pub machine_id: Option<String>,
    /// Declared: source.
    pub source: Option<String>,
    /// Declared: project.
    pub project: Option<String>,
    /// Declared: label `--as`.
    pub submitter_label: Option<String>,
    /// Declared: a tag the order holds.
    pub tag: Option<String>,
    /// Declared size: the order needs at least this much memory.
    pub min_mem_mb: Option<u64>,
    /// Further attribute tests.
    pub terms: Vec<Term>,
    /// At least one of these must match; empty means no restriction.
    pub any: Vec<Condition>,
    /// This must not match.
    pub not: Option<Box<Condition>>,
}

impl StrAttr {
    /// Whether the broker, not the client, supplies the attribute.
    pub fn is_trusted(self) -> bool {
        matches!(
            self,
            StrAttr::ClientLabel | StrAttr::ClientKey | StrAttr::MachineId
        )
    }

    /// The key of the attribute in the policy file.
    pub fn name(self) -> &'static str {
        match self {
            StrAttr::ClientLabel => "client_label",
            StrAttr::ClientKey => "client_key",
            StrAttr::MachineId => "machine_id",
            StrAttr::Source => "source",
            StrAttr::Project => "project",
            StrAttr::SubmitterLabel => "submitter_label",
            StrAttr::Platform => "platform",
            StrAttr::Tag => "tag",
            StrAttr::Label => "label",
        }
    }

    fn values<'a>(self, order: &'a Order, client: &'a ClientView) -> Vec<&'a str> {
        let r = &order.requirements;
        match self {
            StrAttr::ClientLabel => vec![client.label.as_str()],
            StrAttr::ClientKey => vec![client.key.as_str()],
            StrAttr::MachineId => vec![client.machine_id.as_str()],
            StrAttr::Source => order.declared.source.as_deref().into_iter().collect(),
            StrAttr::Project => order.declared.project.as_deref().into_iter().collect(),
            StrAttr::SubmitterLabel => order.declared.label.as_deref().into_iter().collect(),
            StrAttr::Platform => r.platform.as_deref().into_iter().collect(),
            StrAttr::Tag => r
                .tags
                .iter()
                .chain(r.exclusive.iter())
                .map(String::as_str)
                .collect(),
            StrAttr::Label => order.declared.labels.iter().map(String::as_str).collect(),
        }
    }
}

impl NumAttr {
    /// The key of the attribute in the policy file.
    pub fn name(self) -> &'static str {
        match self {
            NumAttr::MemMb => "mem_mb",
            NumAttr::Cpu => "cpu",
            NumAttr::DiskMb => "disk_mb",
            NumAttr::TimeoutS => "timeout_s",
        }
    }

    fn value(self, order: &Order) -> u64 {
        let r = &order.requirements;
        match self {
            NumAttr::MemMb => r.mem_mb,
            NumAttr::Cpu => u64::from(r.cpu),
            NumAttr::DiskMb => r.disk_mb,
            NumAttr::TimeoutS => r.timeout_s,
        }
    }
}

impl StrTest {
    fn passes(&self, value: &str) -> bool {
        match self {
            StrTest::Eq(w) => value == w,
            StrTest::Ne(w) => value != w,
            StrTest::In(ws) => ws.iter().any(|w| value == w),
            StrTest::Prefix(p) => value.starts_with(p.as_str()),
            StrTest::Glob(g) => glob_match(g, value),
        }
    }
}

impl NumTest {
    fn passes(&self, v: u64) -> bool {
        match self {
            NumTest::Eq(w) => v == *w,
            NumTest::Ne(w) => v != *w,
            NumTest::In(ws) => ws.contains(&v),
            NumTest::Lt(w) => v < *w,
            NumTest::Le(w) => v <= *w,
            NumTest::Gt(w) => v > *w,
            NumTest::Ge(w) => v >= *w,
        }
    }
}

impl Term {
    fn matches(&self, order: &Order, client: &ClientView) -> bool {
        match self {
            Term::Num(attr, test) => test.passes(attr.value(order)),
            Term::Str(attr, StrTest::Ne(w)) => attr.values(order, client).iter().all(|v| v != w),
            Term::Str(attr, test) => attr.values(order, client).iter().any(|v| test.passes(v)),
        }
    }
}

impl Condition {
    /// Whether the condition holds for `order` from `client`.
    pub fn matches(&self, order: &Order, client: &ClientView) -> bool {
        let eq = |want: &Option<String>, attr: StrAttr| {
            want.as_deref()
                .is_none_or(|w| attr.values(order, client).contains(&w))
        };
        eq(&self.client_label, StrAttr::ClientLabel)
            && eq(&self.client_key, StrAttr::ClientKey)
            && eq(&self.machine_id, StrAttr::MachineId)
            && eq(&self.source, StrAttr::Source)
            && eq(&self.project, StrAttr::Project)
            && eq(&self.submitter_label, StrAttr::SubmitterLabel)
            && eq(&self.tag, StrAttr::Tag)
            && self
                .min_mem_mb
                .is_none_or(|m| order.requirements.mem_mb >= m)
            && self.terms.iter().all(|t| t.matches(order, client))
            && (self.any.is_empty() || self.any.iter().any(|c| c.matches(order, client)))
            && self.not.as_ref().is_none_or(|c| !c.matches(order, client))
    }

    /// Whether the condition names a declared attribute anywhere in its tree (spec 3.5: such
    /// rules need a trusted label condition too; the loader enforces it).
    pub fn uses_declared(&self) -> bool {
        !self.declared_names().is_empty()
    }

    /// Sorted, deduplicated keys of the declared attributes named anywhere in the tree.
    pub fn declared_names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        self.collect_declared(&mut names);
        names.sort_unstable();
        names.dedup();
        names
    }

    fn collect_declared(&self, out: &mut Vec<&'static str>) {
        let flat = [
            (&self.source, "source"),
            (&self.project, "project"),
            (&self.submitter_label, "submitter_label"),
            (&self.tag, "tag"),
        ];
        out.extend(flat.iter().filter(|(v, _)| v.is_some()).map(|(_, n)| *n));
        if self.min_mem_mb.is_some() {
            out.push("min_mem_mb");
        }
        for term in &self.terms {
            match term {
                Term::Str(attr, _) if !attr.is_trusted() => out.push(attr.name()),
                Term::Str(..) => {}
                Term::Num(attr, _) => out.push(attr.name()),
            }
        }
        for child in self.any.iter().chain(self.not.as_deref()) {
            child.collect_declared(out);
        }
    }

    /// Whether the top-level conjunction has a positive restriction on the trusted client
    /// label: an equality, `in`, `prefix` or `glob`, or an `any` whose every branch has one.
    /// `ne` and `not` never count.
    pub fn guarded_by_label(&self) -> bool {
        self.client_label.is_some()
            || self.terms.iter().any(|t| {
                matches!(
                    t,
                    Term::Str(
                        StrAttr::ClientLabel,
                        StrTest::Eq(_) | StrTest::In(_) | StrTest::Prefix(_) | StrTest::Glob(_)
                    )
                )
            })
            || (!self.any.is_empty() && self.any.iter().all(Condition::guarded_by_label))
    }

    /// Conservative "every order matching `later` also matches `self`": `false` whenever that
    /// cannot be shown from equal parts.
    pub fn covers(&self, later: &Condition) -> bool {
        let same = |a: &Option<String>, b: &Option<String>| a.is_none() || a == b;
        same(&self.client_label, &later.client_label)
            && same(&self.client_key, &later.client_key)
            && same(&self.machine_id, &later.machine_id)
            && same(&self.source, &later.source)
            && same(&self.project, &later.project)
            && same(&self.submitter_label, &later.submitter_label)
            && same(&self.tag, &later.tag)
            && self
                .min_mem_mb
                .is_none_or(|m| later.min_mem_mb.is_some_and(|l| l >= m))
            && self.terms.iter().all(|t| later.terms.contains(t))
            && (self.any.is_empty()
                || (!later.any.is_empty() && later.any.iter().all(|c| self.any.contains(c))))
            && (self.not.is_none() || self.not == later.not)
    }

    /// A one-line description for `policy check`; an empty condition is `everyone`.
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let flat = [
            ("client_label", &self.client_label),
            ("client_key", &self.client_key),
            ("machine_id", &self.machine_id),
            ("source", &self.source),
            ("project", &self.project),
            ("submitter_label", &self.submitter_label),
            ("tag", &self.tag),
        ];
        parts.extend(
            flat.iter()
                .filter_map(|(n, v)| v.as_ref().map(|v| format!("{n} = {v:?}"))),
        );
        if let Some(m) = self.min_mem_mb {
            parts.push(format!("mem_mb >= {m}"));
        }
        parts.extend(self.terms.iter().map(Term::describe));
        if !self.any.is_empty() {
            let inner: Vec<String> = self.any.iter().map(Condition::describe).collect();
            parts.push(format!("any of ({})", inner.join(" | ")));
        }
        if let Some(n) = &self.not {
            parts.push(format!("not ({})", n.describe()));
        }
        if parts.is_empty() {
            "everyone".to_string()
        } else {
            parts.join(", ")
        }
    }
}

impl Term {
    fn describe(&self) -> String {
        match self {
            Term::Str(attr, test) => {
                let n = attr.name();
                match test {
                    StrTest::Eq(v) => format!("{n} = {v:?}"),
                    StrTest::Ne(v) => format!("{n} != {v:?}"),
                    StrTest::In(vs) => format!("{n} in {vs:?}"),
                    StrTest::Prefix(v) => format!("{n} prefix {v:?}"),
                    StrTest::Glob(v) => format!("{n} glob {v:?}"),
                }
            }
            Term::Num(attr, test) => {
                let n = attr.name();
                match test {
                    NumTest::Eq(v) => format!("{n} = {v}"),
                    NumTest::Ne(v) => format!("{n} != {v}"),
                    NumTest::In(vs) => format!("{n} in {vs:?}"),
                    NumTest::Lt(v) => format!("{n} < {v}"),
                    NumTest::Le(v) => format!("{n} <= {v}"),
                    NumTest::Gt(v) => format!("{n} > {v}"),
                    NumTest::Ge(v) => format!("{n} >= {v}"),
                }
            }
        }
    }
}

/// Whether `text` matches `pattern`: `*` stands for any run of characters, `?` for exactly one,
/// everything else for itself, case-sensitively.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}
