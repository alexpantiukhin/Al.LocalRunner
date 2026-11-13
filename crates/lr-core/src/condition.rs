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

    /// Whether the condition names a declared attribute (spec 3.5: such rules need a trusted
    /// label condition too; the loader enforces it).
    pub fn uses_declared(&self) -> bool {
        self.source.is_some()
            || self.project.is_some()
            || self.submitter_label.is_some()
            || self.tag.is_some()
            || self.min_mem_mb.is_some()
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
