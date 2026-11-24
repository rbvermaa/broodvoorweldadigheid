-- Emails waiting for the SMTP relay (F9, ADR 0014). Rows are queued in the same transaction
-- as the change they report and sent by notify::outbox::send_pending.
CREATE TABLE email_outbox (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Local part of the Message-ID, the same on every retry.
    message_id uuid NOT NULL UNIQUE DEFAULT gen_random_uuid(),
    -- Set by the caller, such as 'order-confirmation:42', so the same email is never queued twice.
    dedupe_key text UNIQUE CHECK (dedupe_key <> ''),
    kind text NOT NULL CHECK (kind IN (
        'order_confirmation', 'pickup_reminder', 'order_cancelled',
        'sick_day_cancelled', 'drop_alert', 'subscription_confirmation'
    )),
    to_address text NOT NULL CHECK (to_address <> ''),
    subject text NOT NULL CHECK (subject <> ''),
    text_body text NOT NULL,
    html_body text NOT NULL,
    list_unsubscribe text CHECK (list_unsubscribe LIKE 'https://%'),
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at timestamptz NOT NULL,
    last_error text,
    created_at timestamptz NOT NULL,
    sent_at timestamptz,
    CHECK ((status = 'sent') = (sent_at IS NOT NULL))
);

CREATE INDEX email_outbox_due_idx ON email_outbox (next_attempt_at, id) WHERE status = 'pending';
