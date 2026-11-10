use crate::db::{read, Db};
use crate::outbox::{self, RESULTS};
use crate::{job_read, job_write, logs, prune, registry, schema};
use async_trait::async_trait;
use lr_abstractions::{
    Admission, Applied, BrokerStore, Inbound, Intake, JobFilter, JobKey, JobRecord, Join,
    JoinCodeRow, LogAppend, MachineRow, OrderRow, OutboxEntry, OutboxKind, PruneLimits,
    PruneReport, Revoked, RunnerRow, RunnerState, Stats, StatsQuery, StepUpdate, StoreError,
    Transition,
};
use lr_core::{JobId, Timestamp};
use std::any::Any;
use std::path::Path;

/// `broker.db` with `logs/` next to it (spec 14), on SQLite: WAL, foreign keys, one connection
/// whose work runs on the blocking thread pool (see the crate docs).
pub struct SqliteBrokerStore {
    db: Db,
}

impl SqliteBrokerStore {
    /// Opens `broker.db` at `path`, creating the file, its directory and the schema when they
    /// do not exist; refuses a damaged file, another kind of database and a newer schema.
    pub fn open(path: &Path) -> Result<SqliteBrokerStore, StoreError> {
        Ok(SqliteBrokerStore {
            db: Db::open(path, &schema::BROKER, true)?,
        })
    }

    /// Like [`SqliteBrokerStore::open`] but fails when the file does not exist, for commands
    /// that only read what a daemon wrote.
    pub fn open_existing(path: &Path) -> Result<SqliteBrokerStore, StoreError> {
        Ok(SqliteBrokerStore {
            db: Db::open(path, &schema::BROKER, false)?,
        })
    }

    /// Closes the connection and opens the same file again, as after a restart.
    pub fn reopen(self) -> Result<SqliteBrokerStore, StoreError> {
        Ok(SqliteBrokerStore {
            db: self.db.reopen()?,
        })
    }

    /// Ties `guard` (a temporary directory, say) to the life of this handle; it survives
    /// [`SqliteBrokerStore::reopen`].
    pub fn keep_alive(self, guard: impl Any + Send + Sync) -> SqliteBrokerStore {
        SqliteBrokerStore {
            db: self.db.keep_alive(guard),
        }
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        self.db.path()
    }
}

#[async_trait]
impl BrokerStore for SqliteBrokerStore {
    async fn accept(&self, intake: Intake) -> Result<Admission, StoreError> {
        self.db.run(move |c| job_write::accept(c, intake)).await
    }

    async fn orders(&self) -> Result<Vec<OrderRow>, StoreError> {
        self.db.run(|c| read(c, job_read::orders)).await
    }

    async fn grant(&self, key: &JobKey, runner: &str) -> Result<(), StoreError> {
        let (key, runner) = (key.clone(), runner.to_string());
        self.db
            .run(move |c| job_write::grant(c, &key, &runner))
            .await
    }

    async fn requeue(&self, key: &JobKey) -> Result<(), StoreError> {
        let key = key.clone();
        self.db.run(move |c| job_write::requeue(c, &key)).await
    }

    async fn transition(&self, key: &JobKey, change: Transition) -> Result<Applied, StoreError> {
        let key = key.clone();
        self.db
            .run(move |c| job_write::transition(c, &key, change))
            .await
    }

    async fn update_step(&self, key: &JobKey, update: StepUpdate) -> Result<Applied, StoreError> {
        let key = key.clone();
        self.db
            .run(move |c| job_write::update_step(c, &key, update))
            .await
    }

    async fn accept_seq(&self, key: &JobKey, from: Inbound, seq: u64) -> Result<bool, StoreError> {
        let key = key.clone();
        self.db
            .run(move |c| job_write::accept_seq(c, &key, from, seq))
            .await
    }

    async fn last_seq(&self, key: &JobKey, from: Inbound) -> Result<u64, StoreError> {
        let key = key.clone();
        self.db
            .run(move |c| job_write::last_seq(c, &key, from))
            .await
    }

    async fn next_seq_to_runner(&self, key: &JobKey) -> Result<u64, StoreError> {
        let key = key.clone();
        self.db
            .run(move |c| job_write::next_seq_to_runner(c, &key))
            .await
    }

    async fn append_log(
        &self,
        key: &JobKey,
        data: &[u8],
        at: Timestamp,
        limit_bytes: u64,
    ) -> Result<LogAppend, StoreError> {
        let (key, data, dir) = (key.clone(), data.to_vec(), self.db.dir());
        self.db
            .run(move |c| logs::append(c, &dir, &key, &data, at, limit_bytes))
            .await
    }

    async fn read_log(&self, key: &JobKey) -> Result<Vec<u8>, StoreError> {
        let (key, dir) = (key.clone(), self.db.dir());
        self.db
            .run(move |c| read(c, |c| logs::read(c, &dir, &key)))
            .await
    }

    async fn read_log_tail(&self, key: &JobKey, max_bytes: u64) -> Result<Vec<u8>, StoreError> {
        let (key, dir) = (key.clone(), self.db.dir());
        self.db
            .run(move |c| read(c, |c| logs::read_tail(c, &dir, &key, max_bytes)))
            .await
    }

    async fn job(&self, key: &JobKey) -> Result<Option<JobRecord>, StoreError> {
        let key = key.clone();
        self.db
            .run(move |c| read(c, |c| job_read::job(c, &key)))
            .await
    }

    async fn jobs(&self, filter: &JobFilter) -> Result<Vec<JobRecord>, StoreError> {
        let filter = filter.clone();
        self.db
            .run(move |c| read(c, |c| job_read::jobs(c, &filter)))
            .await
    }

    async fn stats(&self, query: &StatsQuery) -> Result<Stats, StoreError> {
        let query = query.clone();
        self.db
            .run(move |c| read(c, |c| job_read::stats(c, &query)))
            .await
    }

    async fn prune(&self, limits: PruneLimits) -> Result<PruneReport, StoreError> {
        let dir = self.db.dir();
        self.db.run(move |c| prune::prune(c, &dir, limits)).await
    }

    async fn issue_code(&self, code: JoinCodeRow) -> Result<(), StoreError> {
        self.db.run(move |c| registry::issue_code(c, code)).await
    }

    async fn join_code(&self, code_hash: &str) -> Result<Option<JoinCodeRow>, StoreError> {
        let hash = code_hash.to_string();
        self.db.run(move |c| registry::join_code(c, &hash)).await
    }

    async fn join_codes(&self) -> Result<Vec<JoinCodeRow>, StoreError> {
        self.db.run(|c| registry::join_codes(c)).await
    }

    async fn redeem(&self, join: Join) -> Result<MachineRow, StoreError> {
        self.db.run(move |c| registry::redeem(c, join)).await
    }

    async fn machines(&self) -> Result<Vec<MachineRow>, StoreError> {
        self.db.run(|c| registry::machines(c)).await
    }

    async fn machine_by_key(&self, public_key: &str) -> Result<Option<MachineRow>, StoreError> {
        let key = public_key.to_string();
        self.db
            .run(move |c| registry::machine_by_key(c, &key))
            .await
    }

    async fn touch_machine(&self, name: &str, at: Timestamp) -> Result<(), StoreError> {
        let name = name.to_string();
        self.db
            .run(move |c| registry::touch_machine(c, &name, at))
            .await
    }

    async fn revoke(&self, name: &str, at: Timestamp) -> Result<Revoked, StoreError> {
        let name = name.to_string();
        self.db.run(move |c| registry::revoke(c, &name, at)).await
    }

    async fn upsert_runner(&self, runner: RunnerRow) -> Result<(), StoreError> {
        self.db
            .run(move |c| registry::upsert_runner(c, runner))
            .await
    }

    async fn set_runner_state(
        &self,
        name: &str,
        state: RunnerState,
        at: Timestamp,
    ) -> Result<(), StoreError> {
        let name = name.to_string();
        self.db
            .run(move |c| registry::set_runner_state(c, &name, state, at))
            .await
    }

    async fn runners(&self) -> Result<Vec<RunnerRow>, StoreError> {
        self.db.run(|c| registry::runners(c)).await
    }

    async fn push_result(
        &self,
        client: &str,
        job: &JobId,
        kind: OutboxKind,
        payload: String,
        at: Timestamp,
        limit_bytes: u64,
    ) -> Result<Option<u64>, StoreError> {
        let (client, job) = (client.to_string(), job.clone());
        self.db
            .run(move |c| {
                crate::db::write(c, |tx| {
                    let used = outbox::pending_bytes(tx, RESULTS, &client)?;
                    if kind == OutboxKind::Chunk && used + payload.len() as u64 > limit_bytes {
                        return Ok(None);
                    }
                    outbox::push(tx, RESULTS, &client, &job, kind, &payload, at).map(Some)
                })
            })
            .await
    }

    async fn pending_results(&self, client: &str) -> Result<Vec<OutboxEntry>, StoreError> {
        let client = client.to_string();
        self.db
            .run(move |c| outbox::pending(c, RESULTS, &client))
            .await
    }

    async fn ack_results(&self, client: &str, job: &JobId, seq: u64) -> Result<usize, StoreError> {
        let (client, job) = (client.to_string(), job.clone());
        self.db
            .run(move |c| {
                crate::db::write(c, |tx| {
                    outbox::close(tx, RESULTS, &client, Some(&job), Some(seq))
                })
            })
            .await
    }

    async fn expire_results(&self, before: Timestamp) -> Result<usize, StoreError> {
        self.db
            .run(move |c| crate::db::write(c, |tx| outbox::expire(tx, RESULTS, before)))
            .await
    }
}
