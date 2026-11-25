-- One row per email kind (ADR 0044). A new kind is one INSERT in its feature's migration,
-- instead of restating the kind check of email_outbox.
CREATE TABLE email_kind (
    kind text PRIMARY KEY CHECK (kind <> '')
);

INSERT INTO email_kind (kind) VALUES
    ('account_deleted'),
    ('contact_message'),
    ('donation_moved'),
    ('drop_alert'),
    ('login_link'),
    ('order_cancelled'),
    ('order_confirmation'),
    ('pickup_moved'),
    ('pickup_reminder'),
    ('sick_day_cancelled'),
    ('subscription_confirmation'),
    ('vote_confirmation');

ALTER TABLE email_outbox ADD CONSTRAINT email_outbox_kind_fkey
    FOREIGN KEY (kind) REFERENCES email_kind (kind);

-- Widens the kind check of 20261201000800_poll.sql for good: the foreign key decides the
-- kinds now. The name stays, so a branch that still extends the old list keeps working.
ALTER TABLE email_outbox DROP CONSTRAINT email_outbox_kind_check;
ALTER TABLE email_outbox ADD CONSTRAINT email_outbox_kind_check CHECK (kind <> '');
