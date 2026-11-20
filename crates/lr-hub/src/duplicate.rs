use crate::engine::Engine;
use lr_proto::{OrderSpec, WireCode};

pub(crate) fn short_commit(spec: &OrderSpec) -> Option<&str> {
    match &spec.code {
        WireCode::Commit { sha, .. } => Some(&sha[..sha.len().min(7)]),
        WireCode::WorkTree { .. } => None,
    }
}

fn same_order(a: &OrderSpec, b: &OrderSpec) -> bool {
    a.profile == b.profile
        && a.identity.label == b.identity.label
        && a.identity.cwd == b.identity.cwd
        && a.identity.branch == b.identity.branch
        && a.args == b.args
}

impl Engine {
    pub(crate) fn duplicate_warning(&self, spec: &OrderSpec) -> Option<String> {
        let (job, existing) =
            self.tracks
                .iter()
                .filter(|(_, t)| !t.cancelled)
                .find_map(|(job, t)| {
                    let other = t.spec.as_ref()?;
                    same_order(spec, other).then_some((job, other))
                })?;
        let at = short_commit(existing).map_or(String::new(), |c| format!(" at commit {c}"));
        Some(format!(
            "warning: an identical order is already queued or running: job {job}{at}; this one is queued as well (cancel one of them if it is a copy)"
        ))
    }
}
