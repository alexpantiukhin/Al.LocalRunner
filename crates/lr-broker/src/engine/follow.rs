use super::conn::ConnId;
use super::Engine;
use crate::api::Viewer;
use crate::MAX_SUBSCRIPTIONS;
use lr_abstractions::{Frame, JobKey};
use lr_core::JobId;
use lr_proto::{split_text, BrokerToHub, LogChunk, ReadReply, CHUNK_TEXT_LIMIT};

pub(crate) struct Follow {
    pub(crate) conn: ConnId,
    req: u64,
    key: JobKey,
    step: Option<Option<u32>>,
}

fn frames(req: u64, text: &str, end: bool) -> Vec<Frame> {
    let mut out: Vec<Frame> = split_text(text, CHUNK_TEXT_LIMIT)
        .into_iter()
        .filter(|p| !p.is_empty())
        .filter_map(|p| {
            Frame::json(&BrokerToHub::LogChunk(LogChunk {
                req,
                text: p.to_string(),
                end: false,
            }))
            .ok()
        })
        .collect();
    if end {
        out.extend(
            Frame::json(&BrokerToHub::LogChunk(LogChunk {
                req,
                text: String::new(),
                end: true,
            }))
            .ok(),
        );
    }
    out
}

impl Engine {
    pub(crate) fn held_streams(&self, conn: ConnId) -> usize {
        let subs = self.conns.get(&conn).map_or(0, |c| c.subs.len());
        subs + self.follows.iter().filter(|f| f.conn == conn).count()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn follow_log(
        &mut self,
        conn: ConnId,
        viewer: &Viewer,
        req: u64,
        job: &JobId,
        client: Option<&str>,
        step: Option<String>,
        tail: Option<u64>,
    ) -> Result<(), ReadReply> {
        if self.held_streams(conn) >= MAX_SUBSCRIPTIONS {
            return Err(ReadReply::Failed {
                message: format!(
                    "at most {MAX_SUBSCRIPTIONS} subscriptions and followed logs per connection"
                ),
            });
        }
        let (rec, _) = self.run_of(viewer, job, client).await?;
        let text = self.log(viewer, job, client, step.clone(), tail).await?;
        let key = rec.job.key;
        let Some(live) = self.find(&key) else {
            self.send_log(conn, req, &text);
            return Ok(());
        };
        let filter = step.map(|name| {
            live.spec
                .steps
                .iter()
                .position(|s| s.name == name)
                .map(|i| i as u32)
        });
        self.send_ordered(conn, frames(req, &text, false));
        self.follows.push(Follow {
            conn,
            req,
            key,
            step: filter,
        });
        Ok(())
    }

    pub(crate) fn feed_follows(&mut self, key: &JobKey, step: Option<u32>, text: &str) {
        let targets: Vec<(ConnId, u64)> = self
            .follows
            .iter()
            .filter(|f| &f.key == key && f.step.is_none_or(|s| s.is_some() && s == step))
            .map(|f| (f.conn, f.req))
            .collect();
        for (conn, req) in targets {
            self.send_ordered(conn, frames(req, text, false));
        }
    }

    pub(crate) fn end_follows(&mut self, key: &JobKey) {
        let (done, keep): (Vec<Follow>, Vec<Follow>) = std::mem::take(&mut self.follows)
            .into_iter()
            .partition(|f| &f.key == key);
        self.follows = keep;
        for f in done {
            self.send_ordered(f.conn, frames(f.req, "", true));
        }
    }
}
