CREATE TABLE machine (
    name          TEXT PRIMARY KEY,
    machine_id    TEXT NOT NULL,
    kind          TEXT NOT NULL,
    label         TEXT NOT NULL,
    public_key    TEXT NOT NULL,
    registered_at INTEGER NOT NULL,
    last_seen     INTEGER,
    revoked_at    INTEGER
);
CREATE INDEX machine_public_key ON machine (public_key);

CREATE TABLE join_code (
    code_hash    TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    name         TEXT,
    label        TEXT,
    uses_left    INTEGER NOT NULL,
    issued_at    INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    redeemed_at  INTEGER,
    machine_name TEXT REFERENCES machine (name)
);

CREATE TABLE runner (
    name          TEXT PRIMARY KEY,
    machine_name  TEXT NOT NULL REFERENCES machine (name),
    programs      TEXT NOT NULL,
    platforms     TEXT NOT NULL,
    tags          TEXT NOT NULL,
    max_timeout_s INTEGER,
    cache_max_mb  INTEGER NOT NULL,
    cache_used_mb INTEGER NOT NULL,
    free_disk_mb  INTEGER NOT NULL,
    services      TEXT NOT NULL,
    accept        TEXT,
    last_seen     INTEGER,
    state         TEXT NOT NULL,
    enforcement   TEXT
);

CREATE TABLE job (
    client_key      TEXT NOT NULL,
    id              TEXT NOT NULL,
    project         TEXT NOT NULL,
    source          TEXT NOT NULL,
    label           TEXT NOT NULL,
    note            TEXT,
    branch          TEXT,
    cwd             TEXT NOT NULL,
    profile         TEXT NOT NULL,
    platform        TEXT,
    runner_name     TEXT,
    commit_sha      TEXT,
    client_machine  TEXT NOT NULL,
    client_label    TEXT NOT NULL,
    os_user         TEXT NOT NULL,
    secrets         TEXT NOT NULL,
    code_source     TEXT NOT NULL,
    queued_at       INTEGER NOT NULL,
    started_at      INTEGER,
    finished_at     INTEGER,
    state           TEXT NOT NULL,
    exit_code       INTEGER,
    cancelled_by    TEXT,
    cancel_reason   TEXT,
    peak_mem_mb     INTEGER,
    peak_disk_mb    INTEGER,
    recv_seq_hub    INTEGER NOT NULL DEFAULT 0,
    recv_seq_runner INTEGER NOT NULL DEFAULT 0,
    send_seq_runner INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (client_key, id)
);
CREATE INDEX job_queued_at ON job (queued_at);
CREATE INDEX job_state_finished ON job (state, finished_at);

CREATE TRIGGER job_final_state_is_written_once
BEFORE UPDATE OF state, finished_at, exit_code ON job
WHEN OLD.state IN {FINAL}
BEGIN
    SELECT RAISE(ABORT, 'the final state of a job is written once');
END;

CREATE TABLE step (
    client_key  TEXT NOT NULL,
    job_id      TEXT NOT NULL,
    ord         INTEGER NOT NULL,
    name        TEXT NOT NULL,
    command     TEXT NOT NULL,
    state       TEXT NOT NULL,
    exit_code   INTEGER,
    started_at  INTEGER,
    finished_at INTEGER,
    timeout_s   INTEGER NOT NULL,
    PRIMARY KEY (client_key, job_id, ord),
    FOREIGN KEY (client_key, job_id) REFERENCES job (client_key, id) ON DELETE CASCADE
);

CREATE TABLE job_spec (
    client_key     TEXT NOT NULL,
    job_id         TEXT NOT NULL,
    resolved_json  TEXT NOT NULL,
    profile_commit TEXT,
    order_json     TEXT NOT NULL,
    PRIMARY KEY (client_key, job_id),
    FOREIGN KEY (client_key, job_id) REFERENCES job (client_key, id) ON DELETE CASCADE
);

CREATE TABLE github_run (
    client_key TEXT NOT NULL,
    job_id     TEXT NOT NULL,
    repository TEXT NOT NULL,
    workflow   TEXT NOT NULL,
    job_name   TEXT NOT NULL,
    run_id     INTEGER NOT NULL,
    attempt    INTEGER,
    "ref"      TEXT NOT NULL,
    actor      TEXT NOT NULL,
    run_url    TEXT NOT NULL,
    PRIMARY KEY (client_key, job_id),
    FOREIGN KEY (client_key, job_id) REFERENCES job (client_key, id) ON DELETE CASCADE
);

CREATE TABLE job_log (
    client_key TEXT NOT NULL,
    job_id     TEXT NOT NULL,
    path       TEXT NOT NULL,
    bytes      INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (client_key, job_id),
    FOREIGN KEY (client_key, job_id) REFERENCES job (client_key, id) ON DELETE CASCADE
);

CREATE TABLE "order" (
    client_key     TEXT NOT NULL,
    job_id         TEXT NOT NULL,
    client_machine TEXT NOT NULL,
    spec_json      TEXT NOT NULL,
    platform       TEXT,
    same_machine   INTEGER NOT NULL,
    priority_hint  INTEGER,
    enqueued_at    INTEGER NOT NULL,
    state          TEXT NOT NULL,
    PRIMARY KEY (client_key, job_id),
    FOREIGN KEY (client_key, job_id) REFERENCES job (client_key, id) ON DELETE CASCADE
);
CREATE INDEX order_enqueued_at ON "order" (enqueued_at);

CREATE TABLE result_outbox (
    client_machine TEXT NOT NULL,
    job_id         TEXT NOT NULL,
    seq            INTEGER NOT NULL,
    kind           TEXT NOT NULL,
    payload_json   TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    acked_at       INTEGER,
    PRIMARY KEY (client_machine, job_id, seq)
);
