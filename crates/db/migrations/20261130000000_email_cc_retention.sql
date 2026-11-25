-- Orders on account (F11a, ADR 0032): a copy of the confirmation to the invoice email.
ALTER TABLE email_outbox ADD COLUMN cc_address text CHECK (cc_address <> '');

-- Retention (F12, ADR 0031): emails are deleted 90 days after they were sent or failed for
-- good. Rows that failed before this column existed count from their last attempt.
ALTER TABLE email_outbox ADD COLUMN failed_at timestamptz;

CREATE INDEX email_outbox_finished_idx
    ON email_outbox ((coalesce(sent_at, failed_at, next_attempt_at)))
    WHERE status <> 'pending';
