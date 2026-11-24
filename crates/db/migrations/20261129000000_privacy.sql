-- F12 (ADR 0031): an erased customer keeps their row, because the orders kept for the
-- bookkeeping point at it, but nothing in it identifies them any more.
ALTER TABLE customer ADD COLUMN erased_at timestamptz;

-- Extends the kind check of 20261128120000_email_pickup_moved.sql.
ALTER TABLE email_outbox DROP CONSTRAINT email_outbox_kind_check;
ALTER TABLE email_outbox ADD CONSTRAINT email_outbox_kind_check CHECK (kind IN (
    'order_confirmation', 'pickup_reminder', 'order_cancelled',
    'sick_day_cancelled', 'drop_alert', 'subscription_confirmation',
    'contact_message', 'login_link', 'donation_moved', 'pickup_moved', 'account_deleted'
));

-- For the cleanup of expired login links and logins.
CREATE INDEX customer_login_link_expires_idx ON customer_login_link (expires_at);
CREATE INDEX customer_login_last_seen_idx ON customer_login (last_seen_at);
