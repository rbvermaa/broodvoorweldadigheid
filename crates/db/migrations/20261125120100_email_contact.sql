-- Contact form emails to the bakery (F4f, ADR 0018): a Reply-To with the sender's address
-- and a new kind. Extends the kind check of 20261125000100_email_outbox.sql.
ALTER TABLE email_outbox ADD COLUMN reply_to text CHECK (reply_to <> '');

ALTER TABLE email_outbox DROP CONSTRAINT email_outbox_kind_check;
ALTER TABLE email_outbox ADD CONSTRAINT email_outbox_kind_check CHECK (kind IN (
    'order_confirmation', 'pickup_reminder', 'order_cancelled',
    'sick_day_cancelled', 'drop_alert', 'subscription_confirmation',
    'contact_message'
));
