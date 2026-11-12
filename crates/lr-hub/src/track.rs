use crate::engine::ConnId;
use lr_abstractions::{ActiveOrder, Frame};
use lr_core::{Identity, JobId, JobState, Source, Timestamp, Zone};
use lr_proto::{CliReply, JobSummary, OrderSpec, ProtocolError, ReasonCode};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Notify};

/// A CLI connection following a job; `hangup` closes the connection.
pub(crate) type Watcher = (ConnId, mpsc::Sender<CliReply>, Arc<Notify>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Selecting,
    Waiting,
    Sent,
    Accepted,
    Started,
}

impl Stage {
    pub(crate) fn accepted(self) -> bool {
        matches!(self, Stage::Accepted | Stage::Started)
    }
}

pub(crate) struct Track {
    pub(crate) order: ActiveOrder,
    pub(crate) spec: Option<OrderSpec>,
    pub(crate) stage: Stage,
    pub(crate) cancelled: bool,
    pub(crate) excluded: BTreeSet<String>,
    pub(crate) last_rejection: Option<(ReasonCode, String)>,
    pub(crate) gen: u64,
    pub(crate) watchers: Vec<Watcher>,
    pub(crate) log: Option<PathBuf>,
    pub(crate) wait_reason: Option<String>,
    pub(crate) runner: Option<String>,
    pub(crate) next_select: Option<Timestamp>,
}

impl Track {
    pub(crate) fn new(order: ActiveOrder, spec: Option<OrderSpec>, stage: Stage) -> Track {
        let log = order
            .handle_path
            .as_ref()
            .map(|h| PathBuf::from(h).with_extension("log"));
        Track {
            order,
            spec,
            stage,
            cancelled: false,
            excluded: BTreeSet::new(),
            last_rejection: None,
            gen: 0,
            watchers: Vec::new(),
            log,
            wait_reason: None,
            runner: None,
            next_select: None,
        }
    }

    pub(crate) fn same_machine(&self) -> bool {
        self.spec
            .as_ref()
            .is_some_and(|s| s.requirements.same_machine.is_some())
    }

    pub(crate) fn owned_by(&self, os_user: &str, label: &str) -> bool {
        self.order.os_user == os_user && self.order.label == label
    }

    pub(crate) fn owner(&self) -> Identity {
        match &self.spec {
            Some(s) => s.identity.clone(),
            None => Identity {
                label: self.order.label.clone(),
                note: None,
                os_user: self.order.os_user.clone(),
                host: String::new(),
                sender_pid: self.order.submitter_pid,
                sender_start: self.order.submitter_start,
                cwd: PathBuf::new(),
                branch: None,
                worktree_root: None,
                repo_root: None,
                project: String::new(),
                source: Source::Session,
            },
        }
    }

    pub(crate) fn state(&self) -> JobState {
        match self.stage {
            Stage::Started => JobState::Running,
            _ => JobState::Queued,
        }
    }

    pub(crate) fn summary(&self, state: JobState) -> JobSummary {
        let spec = self.spec.as_ref();
        JobSummary {
            job: self.order.job.clone(),
            client_label: String::new(),
            state,
            platform: spec.and_then(|s| s.requirements.platform.clone()),
            runner: self.runner.clone(),
            queued_at: spec.map(|s| s.enqueued_at).unwrap_or_default(),
            started_at: None,
            finished_at: None,
            exit_code: None,
            reason: None,
            project: spec.map(|s| s.identity.project.clone()),
            label: Some(self.order.label.clone()),
            profile: spec.map(|s| s.profile.clone()),
            source: spec.map(|s| match s.identity.source {
                lr_core::Source::Session => lr_proto::WireSource::Session,
                lr_core::Source::Github(_) => lr_proto::WireSource::Github,
            }),
            note: spec.and_then(|s| s.identity.note.clone()),
            branch: spec.and_then(|s| s.identity.branch.clone()),
            cwd: spec.map(|s| s.identity.cwd.display().to_string()),
            timeout_s: spec.map(|s| s.requirements.timeout_s),
            github_run_url: spec.and_then(|s| match &s.identity.source {
                lr_core::Source::Github(g) => Some(g.run_url.clone()),
                lr_core::Source::Session => None,
            }),
        }
    }
}

#[derive(Default)]
pub(crate) struct Link {
    pub(crate) zone: Option<Zone>,
    pub(crate) out: Option<mpsc::Sender<Frame>>,
    pub(crate) kill: Option<Arc<Notify>>,
    pub(crate) sent: BTreeSet<(JobId, u64)>,
    pub(crate) resend_at: Option<Timestamp>,
    pub(crate) flush_later: bool,
    pub(crate) refused: Option<ProtocolError>,
}

impl Link {
    pub(crate) fn acked(&mut self, job: &JobId, seq: u64) -> u64 {
        let top = self
            .sent
            .iter()
            .filter(|(j, s)| j == job && *s <= seq)
            .map(|(_, s)| *s)
            .max()
            .unwrap_or(0);
        self.sent.retain(|(j, s)| j != job || *s > top);
        top
    }

    pub(crate) fn forget(&mut self, job: &JobId) {
        self.sent.retain(|(j, _)| j != job);
    }

    pub(crate) fn close(&mut self) {
        if let Some(k) = &self.kill {
            k.notify_one();
        }
    }
}

pub(crate) struct Finished {
    pub(crate) label: String,
    pub(crate) os_user: String,
    pub(crate) summary: JobSummary,
    pub(crate) reply: CliReply,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(sent: &[(&str, u64)]) -> Link {
        Link {
            sent: sent
                .iter()
                .map(|(j, s)| (JobId(j.to_string()), *s))
                .collect(),
            ..Link::default()
        }
    }

    #[test]
    fn ack_is_clamped_to_the_highest_seq_sent_and_trims_the_sent_set() {
        let mut l = link(&[("a", 1), ("a", 2), ("a", 3), ("b", 1)]);
        assert_eq!(l.acked(&JobId("a".into()), u64::MAX), 3);
        assert_eq!(l.sent, link(&[("b", 1)]).sent);
        let mut l = link(&[("a", 1), ("a", 2), ("a", 3)]);
        assert_eq!(l.acked(&JobId("a".into()), 2), 2);
        assert_eq!(l.sent, link(&[("a", 3)]).sent);
        assert_eq!(l.acked(&JobId("z".into()), 7), 0);
    }
}
