//! Writes of a run: intake, grant, requeue, state changes, steps, seq counters.

use crate::codec::{col_enum, col_opt_time, int, json, opt_int, opt_time, time, Text};
use crate::db::{not_found, write, Fail, R};
use crate::seq::decide;
use lr_abstractions::{
    Admission, Applied, Inbound, Intake, JobKey, OrderRow, StepUpdate, StoreError, Transition,
};
use lr_core::{JobState, StepState, Timestamp};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

fn order_snapshot(o: &OrderRow) -> String {
    json(&serde_json::json!({
        "spec_json": o.spec_json,
        "client_machine": o.client_machine,
        "platform": o.platform,
        "same_machine": o.same_machine,
        "priority_hint": o.priority_hint,
        "enqueued_at": o.enqueued_at.0,
        "state": o.state,
    }))
}

fn insert_order(tx: &Transaction<'_>, o: &OrderRow) -> R<()> {
    tx.execute(
        "INSERT INTO \"order\" (client_key, job_id, client_machine, spec_json, platform, \
         same_machine, priority_hint, enqueued_at, state) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            o.key.client,
            o.key.job.0,
            o.client_machine,
            o.spec_json,
            o.platform,
            o.same_machine,
            o.priority_hint,
            time(o.enqueued_at),
            o.state
        ],
    )?;
    Ok(())
}

pub(crate) fn accept(conn: &mut Connection, intake: Intake) -> R<Admission> {
    write(conn, |tx| {
        let (c, id) = (&intake.job.key.client, &intake.job.key.job.0);
        let exists = tx
            .query_row(
                "SELECT 1 FROM job WHERE client_key = ?1 AND id = ?2",
                [c, id],
                |_| Ok(()),
            )
            .optional()?;
        if exists.is_some() {
            return Ok(Admission::Duplicate);
        }
        if intake.order.key != intake.job.key || intake.job.state != JobState::Queued {
            return Err(Fail::Store(StoreError::Conflict(
                "intake needs a queued job and an order of the same key".into(),
            )));
        }
        let j = &intake.job;
        tx.execute(
            "INSERT INTO job (client_key, id, project, source, label, note, branch, cwd, profile, \
             platform, runner_name, commit_sha, client_machine, client_label, os_user, secrets, \
             code_source, queued_at, started_at, finished_at, state, exit_code, cancelled_by, \
             cancel_reason) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,\
             ?18,?19,?20,?21,?22,?23,?24)",
            params![
                j.key.client,
                j.key.job.0,
                j.project,
                j.source.text(),
                j.label,
                j.note,
                j.branch,
                j.cwd,
                j.profile,
                j.platform,
                j.runner,
                j.commit_sha,
                j.client_machine,
                j.client_label,
                j.os_user,
                json(&j.secrets),
                j.code_source.text(),
                time(j.queued_at),
                opt_time(j.started_at),
                opt_time(j.finished_at),
                j.state.text(),
                j.exit_code,
                j.cancelled_by,
                j.cancel_reason
            ],
        )?;
        for s in &intake.steps {
            tx.execute(
                "INSERT INTO step (client_key, job_id, ord, name, command, state, exit_code, \
                 started_at, finished_at, timeout_s) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    c,
                    id,
                    s.ord,
                    s.name,
                    s.command,
                    s.state.text(),
                    s.exit_code,
                    opt_time(s.started_at),
                    opt_time(s.finished_at),
                    int(s.timeout_s)
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO job_spec (client_key, job_id, resolved_json, profile_commit, order_json) \
             VALUES (?1,?2,?3,?4,?5)",
            params![
                c,
                id,
                intake.spec.resolved_json,
                intake.spec.profile_commit,
                order_snapshot(&intake.order)
            ],
        )?;
        if let Some(g) = &intake.github {
            tx.execute(
                "INSERT INTO github_run (client_key, job_id, repository, workflow, job_name, \
                 run_id, attempt, \"ref\", actor, run_url) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    c,
                    id,
                    g.repository,
                    g.workflow,
                    g.job_name,
                    int(g.run_id),
                    opt_int(g.attempt),
                    g.git_ref,
                    g.actor,
                    g.run_url
                ],
            )?;
        }
        insert_order(tx, &intake.order)?;
        Ok(Admission::Accepted)
    })
}

pub(crate) fn grant(conn: &mut Connection, key: &JobKey, runner: &str) -> R<()> {
    write(conn, |tx| {
        let removed = tx.execute(
            "DELETE FROM \"order\" WHERE client_key = ?1 AND job_id = ?2",
            [&key.client, &key.job.0],
        )?;
        if removed == 0 {
            return Err(not_found(format!("order {}/{}", key.client, key.job)));
        }
        tx.execute(
            "UPDATE job SET runner_name = ?3 WHERE client_key = ?1 AND id = ?2",
            params![key.client, key.job.0, runner],
        )?;
        Ok(())
    })
}

type Current = (JobState, Option<Timestamp>, Option<String>);

fn current(tx: &Transaction<'_>, key: &JobKey) -> R<Current> {
    tx.query_row(
        "SELECT state, started_at, runner_name FROM job WHERE client_key = ?1 AND id = ?2",
        [&key.client, &key.job.0],
        |r| Ok((col_enum(r, 0)?, col_opt_time(r, 1)?, r.get(2)?)),
    )
    .optional()?
    .ok_or_else(|| not_found(format!("job {}/{}", key.client, key.job)))
}

pub(crate) fn requeue(conn: &mut Connection, key: &JobKey) -> R<()> {
    write(conn, |tx| {
        let order_exists = tx
            .query_row(
                "SELECT 1 FROM \"order\" WHERE client_key = ?1 AND job_id = ?2",
                [&key.client, &key.job.0],
                |_| Ok(()),
            )
            .optional()?;
        if order_exists.is_some() {
            return Err(Fail::Store(StoreError::Duplicate(format!(
                "order {}/{}",
                key.client, key.job
            ))));
        }
        let (state, started, _) = current(tx, key)?;
        if state != JobState::Queued || started.is_some() {
            return Err(Fail::Store(StoreError::Conflict(format!(
                "job {} already started, cannot be queued again",
                key.job
            ))));
        }
        let snapshot: String = tx.query_row(
            "SELECT order_json FROM job_spec WHERE client_key = ?1 AND job_id = ?2",
            [&key.client, &key.job.0],
            |r| r.get(0),
        )?;
        let v: serde_json::Value = serde_json::from_str(&snapshot).map_err(|e| {
            Fail::Store(StoreError::Io(format!("stored order of {}: {e}", key.job)))
        })?;
        let text = |name: &str| v[name].as_str().unwrap_or_default().to_string();
        let order = OrderRow {
            key: key.clone(),
            client_machine: text("client_machine"),
            spec_json: text("spec_json"),
            platform: v["platform"].as_str().map(str::to_string),
            same_machine: v["same_machine"].as_bool().unwrap_or(false),
            priority_hint: v["priority_hint"].as_i64(),
            enqueued_at: Timestamp(v["enqueued_at"].as_u64().unwrap_or(0)),
            state: text("state"),
        };
        tx.execute(
            "UPDATE job SET runner_name = NULL WHERE client_key = ?1 AND id = ?2",
            [&key.client, &key.job.0],
        )?;
        insert_order(tx, &order)
    })
}

pub(crate) fn transition(conn: &mut Connection, key: &JobKey, change: Transition) -> R<Applied> {
    write(conn, |tx| {
        let (state, _, runner) = current(tx, key)?;
        if state.is_final() {
            return Ok(Applied::Ignored);
        }
        if !state.can_become(change.state) {
            return Err(Fail::Store(StoreError::InvalidTransition {
                from: state,
                to: change.state,
            }));
        }
        if change.state == JobState::Running && runner.is_none() {
            return Err(Fail::Store(StoreError::Conflict(format!(
                "job {} is not granted",
                key.job
            ))));
        }
        let (started, finished, exit) = match change.state {
            JobState::Running => (Some(time(change.at)), None, None),
            s if s.is_final() => (None, Some(time(change.at)), change.exit_code),
            _ => (None, None, None),
        };
        tx.execute(
            "UPDATE job SET state = ?3, started_at = COALESCE(?4, started_at), \
             finished_at = COALESCE(?5, finished_at), exit_code = COALESCE(?6, exit_code), \
             cancelled_by = COALESCE(cancelled_by, ?7), \
             cancel_reason = COALESCE(cancel_reason, ?8) \
             WHERE client_key = ?1 AND id = ?2",
            params![
                key.client,
                key.job.0,
                change.state.text(),
                started,
                finished,
                exit,
                change.cancelled_by,
                change.cancel_reason
            ],
        )?;
        if change.state.is_final() {
            tx.execute(
                "DELETE FROM \"order\" WHERE client_key = ?1 AND job_id = ?2",
                [&key.client, &key.job.0],
            )?;
        }
        Ok(Applied::Changed)
    })
}

pub(crate) fn update_step(conn: &mut Connection, key: &JobKey, u: StepUpdate) -> R<Applied> {
    write(conn, |tx| {
        let (state, _, _) = current(tx, key)?;
        if state.is_final() {
            return Ok(Applied::Ignored);
        }
        let step: Option<StepState> = tx
            .query_row(
                "SELECT state FROM step WHERE client_key = ?1 AND job_id = ?2 AND ord = ?3",
                params![key.client, key.job.0, u.ord],
                |r| col_enum(r, 0),
            )
            .optional()?;
        let step = step.ok_or_else(|| not_found(format!("step {} of {}", u.ord, key.job)))?;
        if !matches!(step, StepState::Queued | StepState::Running) {
            return Ok(Applied::Ignored);
        }
        tx.execute(
            "UPDATE step SET state = ?4, exit_code = COALESCE(?5, exit_code), \
             started_at = COALESCE(?6, started_at), finished_at = COALESCE(?7, finished_at) \
             WHERE client_key = ?1 AND job_id = ?2 AND ord = ?3",
            params![
                key.client,
                key.job.0,
                u.ord,
                u.state.text(),
                u.exit_code,
                opt_time(u.started_at),
                opt_time(u.finished_at)
            ],
        )?;
        Ok(Applied::Changed)
    })
}

fn seq_column(from: Inbound) -> &'static str {
    match from {
        Inbound::Hub => "recv_seq_hub",
        Inbound::Runner => "recv_seq_runner",
    }
}

pub(crate) fn last_seq(conn: &Connection, key: &JobKey, from: Inbound) -> R<u64> {
    let sql = format!(
        "SELECT {} FROM job WHERE client_key = ?1 AND id = ?2",
        seq_column(from)
    );
    conn.query_row(&sql, [&key.client, &key.job.0], |r| {
        Ok(r.get::<_, i64>(0)? as u64)
    })
    .optional()?
    .ok_or_else(|| not_found(format!("job {}/{}", key.client, key.job)))
}

pub(crate) fn accept_seq(conn: &mut Connection, key: &JobKey, from: Inbound, seq: u64) -> R<bool> {
    write(conn, |tx| {
        let next = decide(last_seq(tx, key, from)?, seq)?;
        if next {
            tx.execute(
                &format!(
                    "UPDATE job SET {} = ?3 WHERE client_key = ?1 AND id = ?2",
                    seq_column(from)
                ),
                params![key.client, key.job.0, int(seq)],
            )?;
        }
        Ok(next)
    })
}

pub(crate) fn next_seq_to_runner(conn: &mut Connection, key: &JobKey) -> R<u64> {
    write(conn, |tx| {
        let n = tx.execute(
            "UPDATE job SET send_seq_runner = send_seq_runner + 1 \
             WHERE client_key = ?1 AND id = ?2",
            [&key.client, &key.job.0],
        )?;
        if n == 0 {
            return Err(not_found(format!("job {}/{}", key.client, key.job)));
        }
        let seq: i64 = tx.query_row(
            "SELECT send_seq_runner FROM job WHERE client_key = ?1 AND id = ?2",
            [&key.client, &key.job.0],
            |r| r.get(0),
        )?;
        Ok(seq as u64)
    })
}
