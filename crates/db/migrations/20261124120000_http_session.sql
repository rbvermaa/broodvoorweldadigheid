-- Session store for tower-sessions-sqlx-store (apps/server/src/session.rs), which would
-- otherwise create this table itself; keeping it here leaves MIGRATOR the only schema source.
CREATE TABLE http_session (
    id text PRIMARY KEY NOT NULL,
    data bytea NOT NULL,
    expiry_date timestamptz NOT NULL
);

CREATE INDEX http_session_expiry_date ON http_session (expiry_date);
