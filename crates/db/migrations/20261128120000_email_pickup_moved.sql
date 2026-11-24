-- The email that tells a customer their ophaalmoment moved (F1, F4a). Extends the kind
-- check of 20261127120000_customer_login.sql.
ALTER TABLE email_outbox DROP CONSTRAINT email_outbox_kind_check;
ALTER TABLE email_outbox ADD CONSTRAINT email_outbox_kind_check CHECK (kind IN (
    'order_confirmation', 'pickup_reminder', 'order_cancelled',
    'sick_day_cancelled', 'drop_alert', 'subscription_confirmation',
    'contact_message', 'login_link', 'pickup_moved'
));
