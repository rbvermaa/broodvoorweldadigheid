-- F4d: the customer's donation choice at checkout, followed when the order is paid. Neither
-- set means the drop's own charity.
ALTER TABLE "order" ADD COLUMN donation_charity_id bigint REFERENCES charity (id);
ALTER TABLE "order" ADD COLUMN donation_saved boolean NOT NULL DEFAULT false;
ALTER TABLE "order" ADD CONSTRAINT order_donation_choice
    CHECK (NOT (donation_saved AND donation_charity_id IS NOT NULL));

-- A saved donation's choose link: only the SHA-256 of its token is stored, like
-- customer_login_link. choose_token (plaintext) is no longer written.
ALTER TABLE donation_allocation ADD COLUMN choose_token_hash bytea UNIQUE
    CHECK (octet_length(choose_token_hash) = 32);

-- Extend the checks of 20261124000900_orders.sql to accept the hash.
ALTER TABLE donation_allocation DROP CONSTRAINT donation_allocation_choice;
ALTER TABLE donation_allocation ADD CONSTRAINT donation_allocation_choice CHECK (
    (choose_token IS NULL AND choose_token_hash IS NULL) = (choose_by IS NULL)
);
ALTER TABLE donation_allocation DROP CONSTRAINT donation_allocation_saved;
ALTER TABLE donation_allocation ADD CONSTRAINT donation_allocation_saved CHECK (
    CASE WHEN status = 'saved'
        THEN charity_id IS NULL AND (choose_token IS NOT NULL OR choose_token_hash IS NOT NULL)
        ELSE charity_id IS NOT NULL
    END
);

CREATE INDEX donation_allocation_saved_idx ON donation_allocation (choose_by)
    WHERE status = 'saved';

-- Extends the kind check of 20261127120000_customer_login.sql.
ALTER TABLE email_outbox DROP CONSTRAINT email_outbox_kind_check;
ALTER TABLE email_outbox ADD CONSTRAINT email_outbox_kind_check CHECK (kind IN (
    'order_confirmation', 'pickup_reminder', 'order_cancelled',
    'sick_day_cancelled', 'drop_alert', 'subscription_confirmation',
    'contact_message', 'login_link', 'donation_moved'
));
