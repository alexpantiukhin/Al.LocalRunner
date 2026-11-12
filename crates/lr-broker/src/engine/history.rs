use super::read::{page, passes, SCAN_LIMIT};
use super::Engine;
use crate::api::{
    detail_of, mask_detail, mask_summary, record_summary, wire_source, Access, Viewer,
};
use lr_abstractions::{JobFilter, JobRecord};
use lr_proto::{HistoryFilter, JobDetail, JobSummary, ReadReply, WireSource};

impl Engine {
    async fn visible_runs(
        &self,
        viewer: &Viewer,
        f: &HistoryFilter,
    ) -> Result<Vec<(JobRecord, Access, JobSummary)>, ReadReply> {
        let client = match f.mine {
            true => Some(viewer.key.clone()),
            false => f.client.clone(),
        };
        let filter = JobFilter {
            client,
            project: f.project.clone(),
            source: f.source.map(|s| match s {
                WireSource::Session => lr_abstractions::SourceKind::Session,
                WireSource::Github => lr_abstractions::SourceKind::Github,
            }),
            label: f.label.clone(),
            profile: f.profile.clone(),
            state: f.state,
            platform: f.platform.clone(),
            runner: f.runner.clone(),
            since: f.since,
            active_only: f.active_only,
            limit: Some(SCAN_LIMIT),
        };
        let Ok(rows) = self.store.jobs(&filter).await else {
            return Err(ReadReply::Failed {
                message: "history is unavailable".into(),
            });
        };
        Ok(rows
            .into_iter()
            .filter_map(|rec| {
                let a = self.access_to(viewer, &rec.job.key.client);
                let s = mask_summary(record_summary(&rec), a)?;
                let source = a.sees_fields().then(|| wire_source(rec.job.source));
                let profile_ok = f
                    .profile
                    .as_ref()
                    .is_none_or(|p| s.profile.as_ref() == Some(p));
                (profile_ok && passes(&f.project, &f.source, &f.label, &s, source))
                    .then_some((rec, a, s))
            })
            .collect())
    }

    pub(super) async fn history(&self, viewer: &Viewer, f: HistoryFilter) -> ReadReply {
        match self.visible_runs(viewer, &f).await {
            Ok(runs) => {
                let jobs = runs.into_iter().map(|(_, _, s)| s).collect();
                let (jobs, next) = page(jobs, &f.cursor, f.effective_limit());
                ReadReply::History { jobs, next }
            }
            Err(reply) => reply,
        }
    }

    pub(super) async fn history_details(&self, viewer: &Viewer, f: HistoryFilter) -> ReadReply {
        match self.visible_runs(viewer, &f).await {
            Ok(runs) => {
                let (runs, next) = page(runs, &f.cursor, f.effective_limit());
                let jobs: Vec<JobDetail> = runs
                    .into_iter()
                    .filter_map(|(rec, a, _)| {
                        let wait = self.waits.get(&rec.job.key).cloned();
                        mask_detail(detail_of(&rec, wait), a)
                    })
                    .collect();
                ReadReply::Details { jobs, next }
            }
            Err(reply) => reply,
        }
    }
}
