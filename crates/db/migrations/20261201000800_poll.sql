-- Bread vote (F9b, ADR 0039). A vote belongs to an email address, not to a drop-alert
-- subscriber: voting must not put anyone on the drop-alert list.
CREATE TABLE poll (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    title text NOT NULL CHECK (title <> '' AND char_length(title) <= 200),
    bake_day_id bigint REFERENCES bake_day (id),
    closes_at timestamptz NOT NULL,
    public_results boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL
);

CREATE TABLE poll_option (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    poll_id bigint NOT NULL REFERENCES poll (id),
    product_id bigint REFERENCES product (id),
    name text NOT NULL CHECK (name <> '' AND char_length(name) <= 100),
    position integer NOT NULL CHECK (position >= 0),
    UNIQUE (poll_id, position),
    UNIQUE (poll_id, id)
);

CREATE TABLE vote (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    poll_id bigint NOT NULL REFERENCES poll (id),
    poll_option_id bigint NOT NULL,
    -- NULL once the voter's data is erased (F12); the vote still counts.
    email text CHECK (email <> '' AND email = btrim(email)),
    wish text CHECK (wish <> '' AND char_length(wish) <= 500),
    confirmed_at timestamptz,
    -- The open confirmation link; cleared once used.
    confirm_token_hash bytea UNIQUE CHECK (octet_length(confirm_token_hash) = 32),
    confirm_expires_at timestamptz,
    confirm_sent_at timestamptz,
    created_at timestamptz NOT NULL,
    FOREIGN KEY (poll_id, poll_option_id) REFERENCES poll_option (poll_id, id),
    CHECK ((confirm_token_hash IS NULL) = (confirm_expires_at IS NULL)),
    CHECK (email IS NOT NULL OR (confirmed_at IS NOT NULL AND wish IS NULL))
);

CREATE UNIQUE INDEX vote_poll_email_key ON vote (poll_id, lower(email));
CREATE INDEX vote_email_idx ON vote (lower(email));

-- Extends the kind check of 20261129000000_privacy.sql.
ALTER TABLE email_outbox DROP CONSTRAINT email_outbox_kind_check;
ALTER TABLE email_outbox ADD CONSTRAINT email_outbox_kind_check CHECK (kind IN (
    'order_confirmation', 'pickup_reminder', 'order_cancelled',
    'sick_day_cancelled', 'drop_alert', 'subscription_confirmation',
    'contact_message', 'login_link', 'donation_moved', 'pickup_moved', 'account_deleted',
    'vote_confirmation'
));
