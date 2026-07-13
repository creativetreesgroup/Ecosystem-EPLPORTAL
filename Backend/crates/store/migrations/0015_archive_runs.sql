-- NOT tenant-scoped: retention is a system-wide maintenance operation
-- (Fase 8), not a per-tenant business record. No RLS on this table.
CREATE TABLE archive_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    table_name TEXT NOT NULL,
    run_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    captured_count BIGINT NOT NULL,
    archived_count BIGINT NOT NULL,
    deleted_count BIGINT NOT NULL,
    archive_path TEXT,
    sha256 TEXT,
    status TEXT NOT NULL DEFAULT 'running' CHECK (status IN ('running', 'completed', 'failed')),
    dry_run BOOLEAN NOT NULL DEFAULT false
);
