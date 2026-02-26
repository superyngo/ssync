-- ssync DB schema v1

CREATE TABLE IF NOT EXISTS check_snapshots (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    host         TEXT    NOT NULL,
    collected_at INTEGER NOT NULL,
    online       INTEGER NOT NULL,
    raw_json     TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_check_snapshots_host_time
    ON check_snapshots (host, collected_at DESC);

CREATE TABLE IF NOT EXISTS host_last_seen (
    host        TEXT    PRIMARY KEY,
    last_seen   INTEGER NOT NULL,
    last_online INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sync_state (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    sync_group   TEXT    NOT NULL,
    host         TEXT    NOT NULL,
    path         TEXT    NOT NULL,
    mtime        INTEGER NOT NULL,
    size_bytes   INTEGER NOT NULL,
    blake3       TEXT    NOT NULL,
    synced_at    INTEGER NOT NULL,
    UNIQUE (sync_group, host, path)
);

CREATE INDEX IF NOT EXISTS idx_sync_state_group
    ON sync_state (sync_group, host);

CREATE TABLE IF NOT EXISTS operation_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp   INTEGER NOT NULL,
    command     TEXT    NOT NULL,
    host        TEXT    NOT NULL,
    action      TEXT    NOT NULL,
    status      TEXT    NOT NULL,
    duration_ms INTEGER,
    note        TEXT
);

CREATE INDEX IF NOT EXISTS idx_operation_log_time
    ON operation_log (timestamp DESC);

CREATE INDEX IF NOT EXISTS idx_operation_log_host
    ON operation_log (host, timestamp DESC);
