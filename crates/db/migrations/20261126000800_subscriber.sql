-- Drop alerts (F9a, ADR 0021). Subscribers only come from sign-ups on the site, so there is
-- no source column. Tokens are stored as SHA-256 hashes; the links carry the token itself.
CREATE TABLE subscriber (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email text NOT NULL CHECK (email <> '' AND email = btrim(email)),
    confirmed_at timestamptz,
    unsubscribed_at timestamptz,
    -- The open confirmation link; cleared once used.
    confirm_token_hash bytea UNIQUE CHECK (octet_length(confirm_token_hash) = 32),
    confirm_expires_at timestamptz,
    confirm_sent_at timestamptz,
    created_at timestamptz NOT NULL,
    CHECK ((confirm_token_hash IS NULL) = (confirm_expires_at IS NULL))
);

CREATE UNIQUE INDEX subscriber_email_key ON subscriber (lower(email));

-- Every drop alert carries its own unsubscribe link, so no token is ever stored in clear.
CREATE TABLE subscriber_unsubscribe_token (
    token_hash bytea PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    subscriber_id bigint NOT NULL REFERENCES subscriber (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL
);

CREATE INDEX subscriber_unsubscribe_token_subscriber_idx
    ON subscriber_unsubscribe_token (subscriber_id);

-- One row per drop and alert kind: the alert went out (or was queued) exactly once.
CREATE TABLE drop_alert_sent (
    bake_day_id bigint NOT NULL REFERENCES bake_day (id),
    kind text NOT NULL CHECK (kind IN ('menu_published', 'ordering_open')),
    recipients integer NOT NULL CHECK (recipients >= 0),
    sent_at timestamptz NOT NULL,
    PRIMARY KEY (bake_day_id, kind)
);
