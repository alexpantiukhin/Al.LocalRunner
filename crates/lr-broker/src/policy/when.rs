use lr_core::{Condition, NumAttr, NumTest, StrAttr, StrTest, Term};
use serde::de::value::MapAccessDeserializer;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::fmt;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct StrTable {
    eq: Option<String>,
    ne: Option<String>,
    #[serde(rename = "in")]
    in_: Option<Vec<String>>,
    prefix: Option<String>,
    glob: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NumTable {
    eq: Option<u64>,
    ne: Option<u64>,
    #[serde(rename = "in")]
    in_: Option<Vec<u64>>,
    lt: Option<u64>,
    le: Option<u64>,
    gt: Option<u64>,
    ge: Option<u64>,
}

#[derive(Debug)]
enum StrSpec {
    Plain(String),
    Table(StrTable),
}

#[derive(Debug)]
enum NumSpec {
    Plain(u64),
    Table(NumTable),
}

struct StrVisitor;

impl<'de> Visitor<'de> for StrVisitor {
    type Value = StrSpec;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string or a table with eq, ne, in, prefix, glob")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<StrSpec, E> {
        Ok(StrSpec::Plain(v.to_string()))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<StrSpec, A::Error> {
        StrTable::deserialize(MapAccessDeserializer::new(map)).map(StrSpec::Table)
    }
}

impl<'de> Deserialize<'de> for StrSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(StrVisitor)
    }
}

struct NumVisitor;

impl<'de> Visitor<'de> for NumVisitor {
    type Value = NumSpec;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a non-negative integer or a table with eq, ne, in, lt, le, gt, ge")
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<NumSpec, E> {
        Ok(NumSpec::Plain(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<NumSpec, E> {
        u64::try_from(v)
            .map(NumSpec::Plain)
            .map_err(|_| E::invalid_value(de::Unexpected::Signed(v), &self))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<NumSpec, A::Error> {
        NumTable::deserialize(MapAccessDeserializer::new(map)).map(NumSpec::Table)
    }
}

impl<'de> Deserialize<'de> for NumSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(NumVisitor)
    }
}

/// The `when` table of a `[[rule]]` as written in the file.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWhen {
    client_label: Option<StrSpec>,
    client_key: Option<StrSpec>,
    machine_id: Option<StrSpec>,
    source: Option<StrSpec>,
    project: Option<StrSpec>,
    submitter_label: Option<StrSpec>,
    platform: Option<StrSpec>,
    label: Option<StrSpec>,
    tag: Option<StrSpec>,
    min_mem_mb: Option<u64>,
    mem_mb: Option<NumSpec>,
    cpu: Option<NumSpec>,
    disk_mb: Option<NumSpec>,
    timeout_s: Option<NumSpec>,
    any: Option<Vec<RawWhen>>,
    not: Option<Box<RawWhen>>,
}

fn str_terms(attr: StrAttr, t: StrTable, out: &mut Vec<Term>) -> Result<(), String> {
    let name = attr.name();
    let before = out.len();
    if let Some(v) = t.eq {
        out.push(Term::Str(attr, StrTest::Eq(v)));
    }
    if let Some(v) = t.ne {
        out.push(Term::Str(attr, StrTest::Ne(v)));
    }
    if let Some(v) = t.in_ {
        if v.is_empty() {
            return Err(format!("{name}: `in` must list at least one value"));
        }
        out.push(Term::Str(attr, StrTest::In(v)));
    }
    if let Some(v) = t.prefix {
        out.push(Term::Str(attr, StrTest::Prefix(v)));
    }
    if let Some(v) = t.glob {
        out.push(Term::Str(attr, StrTest::Glob(v)));
    }
    if out.len() == before {
        return Err(format!(
            "{name}: the table needs at least one of eq, ne, in, prefix, glob"
        ));
    }
    Ok(())
}

fn num_terms(attr: NumAttr, spec: NumSpec, out: &mut Vec<Term>) -> Result<(), String> {
    let name = attr.name();
    let t = match spec {
        NumSpec::Plain(v) => {
            out.push(Term::Num(attr, NumTest::Eq(v)));
            return Ok(());
        }
        NumSpec::Table(t) => t,
    };
    let before = out.len();
    if let Some(v) = t.eq {
        out.push(Term::Num(attr, NumTest::Eq(v)));
    }
    if let Some(v) = t.ne {
        out.push(Term::Num(attr, NumTest::Ne(v)));
    }
    if let Some(v) = t.in_ {
        if v.is_empty() {
            return Err(format!("{name}: `in` must list at least one value"));
        }
        out.push(Term::Num(attr, NumTest::In(v)));
    }
    let bounds = [
        (t.lt, NumTest::Lt as fn(u64) -> NumTest),
        (t.le, NumTest::Le),
        (t.gt, NumTest::Gt),
        (t.ge, NumTest::Ge),
    ];
    for (v, make) in bounds {
        if let Some(v) = v {
            out.push(Term::Num(attr, make(v)));
        }
    }
    if out.len() == before {
        return Err(format!(
            "{name}: the table needs at least one of eq, ne, in, lt, le, gt, ge"
        ));
    }
    Ok(())
}

impl RawWhen {
    /// Converts the table to a [`Condition`]; `Err` carries what is wrong, without the rule.
    pub(crate) fn into_condition(self) -> Result<Condition, String> {
        let mut c = Condition {
            min_mem_mb: self.min_mem_mb,
            ..Condition::default()
        };
        let strs = [
            (StrAttr::ClientLabel, self.client_label),
            (StrAttr::ClientKey, self.client_key),
            (StrAttr::MachineId, self.machine_id),
            (StrAttr::Source, self.source),
            (StrAttr::Project, self.project),
            (StrAttr::SubmitterLabel, self.submitter_label),
            (StrAttr::Platform, self.platform),
            (StrAttr::Tag, self.tag),
            (StrAttr::Label, self.label),
        ];
        for (attr, spec) in strs {
            match spec {
                None => {}
                Some(StrSpec::Table(t)) => str_terms(attr, t, &mut c.terms)?,
                Some(StrSpec::Plain(v)) => match attr {
                    StrAttr::ClientLabel => c.client_label = Some(v),
                    StrAttr::ClientKey => c.client_key = Some(v),
                    StrAttr::MachineId => c.machine_id = Some(v),
                    StrAttr::Source => c.source = Some(v),
                    StrAttr::Project => c.project = Some(v),
                    StrAttr::SubmitterLabel => c.submitter_label = Some(v),
                    StrAttr::Tag => c.tag = Some(v),
                    StrAttr::Platform | StrAttr::Label => {
                        c.terms.push(Term::Str(attr, StrTest::Eq(v)))
                    }
                },
            }
        }
        let nums = [
            (NumAttr::MemMb, self.mem_mb),
            (NumAttr::Cpu, self.cpu),
            (NumAttr::DiskMb, self.disk_mb),
            (NumAttr::TimeoutS, self.timeout_s),
        ];
        for (attr, spec) in nums {
            if let Some(spec) = spec {
                num_terms(attr, spec, &mut c.terms)?;
            }
        }
        if let Some(any) = self.any {
            if any.is_empty() {
                return Err("any: the list must hold at least one condition".to_string());
            }
            for child in any {
                let child = child.into_condition()?;
                if child == Condition::default() {
                    return Err("any: a condition in the list must not be empty".to_string());
                }
                c.any.push(child);
            }
        }
        if let Some(not) = self.not {
            let inner = not.into_condition()?;
            if inner == Condition::default() {
                return Err("not: the condition must not be empty".to_string());
            }
            c.not = Some(Box::new(inner));
        }
        Ok(c)
    }
}
