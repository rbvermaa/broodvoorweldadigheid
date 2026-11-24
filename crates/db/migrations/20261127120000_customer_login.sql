-- Customer magic links (spec, Security; ADR 0028). Only the SHA-256 of a link's token is
-- stored. A link works once, until expires_at.
CREATE TABLE customer_login_link (
    token_hash bytea PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    customer_id bigint NOT NULL REFERENCES customer (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL CHECK (expires_at > created_at),
    used_at timestamptz
);

CREATE INDEX customer_login_link_customer_idx ON customer_login_link (customer_id, created_at);

-- One row per signed-in browser, like staff_login; the session holds only the id.
CREATE TABLE customer_login (
    id text PRIMARY KEY
        DEFAULT replace(gen_random_uuid()::text || gen_random_uuid()::text, '-', ''),
    customer_id bigint NOT NULL REFERENCES customer (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL
);

CREATE INDEX customer_login_customer_idx ON customer_login (customer_id);

-- Extends the kind check of 20261125120100_email_contact.sql.
ALTER TABLE email_outbox DROP CONSTRAINT email_outbox_kind_check;
ALTER TABLE email_outbox ADD CONSTRAINT email_outbox_kind_check CHECK (kind IN (
    'order_confirmation', 'pickup_reminder', 'order_cancelled',
    'sick_day_cancelled', 'drop_alert', 'subscription_confirmation',
    'contact_message', 'login_link'
));

-- An email that carries a secret, such as a login link, is not sent after expires_at, and
-- its bodies are blanked once it is sent or failed.
ALTER TABLE email_outbox ADD COLUMN expires_at timestamptz;
