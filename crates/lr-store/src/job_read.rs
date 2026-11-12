//! Reads of runs: one record with all its rows, the filtered history, statistics.

use crate::codec::{
    col_enum, col_json, col_opt_time, col_opt_u64, col_time, col_u64, final_list, time, Text,
};
use crate::db::R;
use lr_abstractions::store::compute_stats;
use lr_abstractions::{
    GithubRunRow, JobFilter, JobKey, JobLogRow, JobRecord, JobRow, JobSpecRow, OrderRow, Stats,
    StatsQuery, StepRow,
};
use lr_core::JobId;
use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, OptionalExtension, Row};

const JOB_COLUMNS: &str = "client_key, id, project, source, label, note, branch, cwd, profile, \
    platform, runner_name, commit_sha, client_machine, client_label, os_user, secrets, \
    code_source, queued_at, started_at, finished_at, state, exit_code, cancelled_by, \
    cancel_reason";

fn job_row(r: &Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        key: JobKey {
            client: r.get(0)?,
            job: JobId(r.get(1)?),
        },
        project: r.get(2)?,
        source: col_enum(r, 3)?,
        label: r.get(4)?,
        note: r.get(5)?,
        branch: r.get(6)?,
        cwd: r.get(7)?,
        profile: r.get(8)?,
        platform: r.get(9)?,
        runner: r.get(10)?,
        commit_sha: r.get(11)?,
        client_machine: r.get(12)?,
        client_label: r.get(13)?,
        os_user: r.get(14)?,
        secrets: col_json(r, 15)?,
        code_source: col_enum(r, 16)?,
        queued_at: col_time(r, 17)?,
        started_at: col_opt_time(r, 18)?,
        finished_at: col_opt_time(r, 19)?,
        state: col_enum(r, 20)?,
        exit_code: r.get(21)?,
        cancelled_by: r.get(22)?,
        cancel_reason: r.get(23)?,
    })
}

fn record(conn: &Connection, job: JobRow) -> R<JobRecord> {
    let (c, id) = (job.key.client.clone(), job.key.job.0.clone());
    let mut stmt = conn.prepare(
        "SELECT ord, name, command, state, exit_code, started_at, finished_at, timeout_s \
         FROM step WHERE client_key = ?1 AND job_id = ?2 ORDER BY ord",
    )?;
    let steps = stmt
        .query_map([&c, &id], |r| {
            Ok(StepRow {
                ord: r.get(0)?,
                name: r.get(1)?,
                command: r.get(2)?,
                state: col_enum(r, 3)?,
                exit_code: r.get(4)?,
                started_at: col_opt_time(r, 5)?,
                finished_at: col_opt_time(r, 6)?,
                timeout_s: col_u64(r, 7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let spec = conn.query_row(
        "SELECT resolved_json, profile_commit FROM job_spec WHERE client_key = ?1 AND job_id = ?2",
        [&c, &id],
        |r| {
            Ok(JobSpecRow {
                resolved_json: r.get(0)?,
                profile_commit: r.get(1)?,
            })
        },
    )?;
    let github = conn
        .query_row(
            "SELECT repository, workflow, job_name, run_id, attempt, \"ref\", actor, run_url \
             FROM github_run WHERE client_key = ?1 AND job_id = ?2",
            [&c, &id],
            |r| {
                Ok(GithubRunRow {
                    repository: r.get(0)?,
                    workflow: r.get(1)?,
                    job_name: r.get(2)?,
                    run_id: col_u64(r, 3)?,
                    attempt: col_opt_u64(r, 4)?,
                    git_ref: r.get(5)?,
                    actor: r.get(6)?,
                    run_url: r.get(7)?,
                })
            },
        )
        .optional()?;
    let log = conn
        .query_row(
            "SELECT path, bytes, updated_at FROM job_log WHERE client_key = ?1 AND job_id = ?2",
            [&c, &id],
            |r| {
                Ok(JobLogRow {
                    path: r.get(0)?,
                    bytes: col_u64(r, 1)?,
                    updated_at: col_time(r, 2)?,
                })
            },
        )
        .optional()?;
    Ok(JobRecord {
        job,
        steps,
        spec,
        github,
        log,
    })
}

pub(crate) fn job(conn: &Connection, key: &JobKey) -> R<Option<JobRecord>> {
    let row = conn
        .query_row(
            &format!("SELECT {JOB_COLUMNS} FROM job WHERE client_key = ?1 AND id = ?2"),
            [&key.client, &key.job.0],
            job_row,
        )
        .optional()?;
    row.map(|j| record(conn, j)).transpose()
}

pub(crate) fn jobs(conn: &Connection, filter: &JobFilter) -> R<Vec<JobRecord>> {
    let mut sql = format!("SELECT {JOB_COLUMNS} FROM job WHERE 1 = 1");
    let mut args: Vec<Value> = Vec::new();
    let mut eq = |sql: &mut String, column: &str, value: Option<String>| {
        if let Some(v) = value {
            args.push(Value::Text(v));
            sql.push_str(&format!(" AND {column} = ?{}", args.len()));
        }
    };
    eq(&mut sql, "client_key", filter.client.clone());
    eq(&mut sql, "project", filter.project.clone());
    eq(
        &mut sql,
        "source",
        filter.source.map(|s| s.text().to_string()),
    );
    eq(&mut sql, "label", filter.label.clone());
    eq(&mut sql, "profile", filter.profile.clone());
    eq(
        &mut sql,
        "state",
        filter.state.map(|s| s.text().to_string()),
    );
    eq(&mut sql, "platform", filter.platform.clone());
    eq(&mut sql, "runner_name", filter.runner.clone());
    if let Some(since) = filter.since {
        args.push(Value::Integer(time(since)));
        sql.push_str(&format!(" AND queued_at >= ?{}", args.len()));
    }
    if filter.active_only {
        sql.push_str(&format!(" AND state NOT IN {}", final_list()));
    }
    sql.push_str(" ORDER BY queued_at DESC, client_key DESC, id DESC");
    if let Some(limit) = filter.limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(args.iter()), job_row)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(|j| record(conn, j)).collect()
}

pub(crate) fn orders(conn: &Connection) -> R<Vec<OrderRow>> {
    let mut stmt = conn.prepare(
        "SELECT client_key, job_id, client_machine, spec_json, platform, same_machine, \
         priority_hint, enqueued_at, state FROM \"order\" \
         ORDER BY enqueued_at, client_key, job_id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(OrderRow {
                key: JobKey {
                    client: r.get(0)?,
                    job: JobId(r.get(1)?),
                },
                client_machine: r.get(2)?,
                spec_json: r.get(3)?,
                platform: r.get(4)?,
                same_machine: r.get(5)?,
                priority_hint: r.get(6)?,
                enqueued_at: col_time(r, 7)?,
                state: r.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn stats(conn: &Connection, query: &StatsQuery) -> R<Stats> {
    let filter = JobFilter {
        client: query.client.clone(),
        project: query.project.clone(),
        profile: query.profile.clone(),
        since: query.since,
        ..JobFilter::default()
    };
    let all = jobs(conn, &filter)?;
    Ok(compute_stats(&all, query))
}
