use crate::engine::Engine;
use lr_proto::OrderSpec;

fn same_order(a: &OrderSpec, b: &OrderSpec) -> bool {
    a.profile == b.profile
        && a.identity.label == b.identity.label
        && a.identity.cwd == b.identity.cwd
        && a.identity.branch == b.identity.branch
        && a.args == b.args
}

impl Engine {
    pub(crate) fn duplicate_warning(&self, spec: &OrderSpec) -> Option<String> {
        let (job, _) = self
            .tracks
            .iter()
            .filter(|(_, t)| !t.cancelled)
            .find_map(|(job, t)| {
                let other = t.spec.as_ref()?;
                same_order(spec, other).then_some((job, other))
            })?;
        Some(format!(
            "warning: an identical order is already queued or running: job {job}; this one is queued as well (cancel one of them if it is a copy)"
        ))
    }
}
