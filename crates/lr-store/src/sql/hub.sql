CREATE TABLE broker_link (
    broker      TEXT PRIMARY KEY,
    address     TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    key_id      TEXT NOT NULL,
    machine_id  TEXT NOT NULL,
    last_zone   TEXT
);

CREATE TABLE active_order (
    job_id          TEXT PRIMARY KEY,
    broker          TEXT,
    last_seq        INTEGER NOT NULL DEFAULT 0,
    label           TEXT NOT NULL,
    os_user         TEXT NOT NULL,
    submitter_pid   INTEGER NOT NULL,
    submitter_start INTEGER,
    handle_path     TEXT,
    detached        INTEGER NOT NULL
);

CREATE TABLE outbox (
    job_id       TEXT NOT NULL REFERENCES active_order (job_id) ON DELETE CASCADE,
    seq          INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    acked_at     INTEGER,
    PRIMARY KEY (job_id, seq)
);
