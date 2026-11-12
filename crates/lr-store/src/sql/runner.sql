CREATE TABLE runner_outbox (
    job_id       TEXT NOT NULL,
    seq          INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    acked_at     INTEGER,
    PRIMARY KEY (job_id, seq)
);

CREATE TABLE runner_recv (
    job_id   TEXT PRIMARY KEY,
    recv_seq INTEGER NOT NULL
);

CREATE TABLE service_registry (
    name       TEXT PRIMARY KEY,
    pid        INTEGER NOT NULL,
    pid_start  INTEGER,
    state      TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    mem_mb     INTEGER NOT NULL
);

CREATE TABLE cache_index (
    client    TEXT NOT NULL,
    project   TEXT NOT NULL,
    name      TEXT NOT NULL,
    path      TEXT NOT NULL,
    bytes     INTEGER NOT NULL,
    last_used INTEGER NOT NULL,
    PRIMARY KEY (client, project, name)
);
