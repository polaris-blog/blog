-- Full task fields and attempt results are stored in versioned JSON documents.
CREATE TABLE IF NOT EXISTS scheduler_jobs (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id VARCHAR(64) NOT NULL UNIQUE,
    status VARCHAR(16) NOT NULL,
    run_at BIGINT NOT NULL,
    job_type VARCHAR(160) NOT NULL,
    created_at BIGINT NOT NULL,
    priority BIGINT NOT NULL,
    paused BIGINT NOT NULL DEFAULT 0,
    lease_until BIGINT,
    locked_by VARCHAR(128),
    locked_at BIGINT,
    owner VARCHAR(160),
    document TEXT NOT NULL,
    version BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX scheduler_due ON scheduler_jobs (status, paused, run_at, priority);
CREATE INDEX scheduler_leases ON scheduler_jobs (status, lease_until);
CREATE INDEX scheduler_type ON scheduler_jobs (job_type);
CREATE INDEX scheduler_created ON scheduler_jobs (created_at);
CREATE TABLE IF NOT EXISTS scheduler_attempts (
    id VARCHAR(128) PRIMARY KEY,
    job_id VARCHAR(64) NOT NULL,
    started_at BIGINT NOT NULL,
    document TEXT NOT NULL,
    FOREIGN KEY (job_id) REFERENCES scheduler_jobs(id) ON DELETE CASCADE
);
CREATE INDEX scheduler_attempt_history ON scheduler_attempts (job_id, started_at);
