CREATE TABLE customer (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    email text NOT NULL CHECK (email <> '' AND email = lower(btrim(email))),
    name text NOT NULL CHECK (btrim(name) <> ''),
    phone text CHECK (btrim(phone) <> ''),
    marketing_consent boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL,
    UNIQUE (email)
);

ALTER TABLE pickup_slot ADD CONSTRAINT pickup_slot_id_bake_day_key UNIQUE (id, bake_day_id);

-- One row per calendar year in Europe/Amsterdam. Checkout bumps it with an upsert, which
-- keeps the row locked until commit; it is the last lock checkout takes (ADR 0012).
-- Numbers never repeat; a rolled-back checkout leaves no gap, an abandoned order keeps
-- its number.
CREATE TABLE order_number (
    year smallint PRIMARY KEY CHECK (year BETWEEN 2000 AND 9999),
    last integer NOT NULL CHECK (last > 0)
);

CREATE TABLE "order" (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    number_year smallint NOT NULL,
    number_seq integer NOT NULL CHECK (number_seq > 0),
    customer_id bigint NOT NULL REFERENCES customer (id),
    customer_name text NOT NULL CHECK (btrim(customer_name) <> ''),
    customer_phone text CHECK (btrim(customer_phone) <> ''),
    marketing_opt_in boolean NOT NULL DEFAULT false,
    bake_day_id bigint NOT NULL REFERENCES bake_day (id),
    pickup_slot_id bigint NOT NULL,
    channel text NOT NULL CHECK (channel IN ('web', 'manual')),
    payment_mode text NOT NULL CHECK (payment_mode IN ('online', 'on_account')),
    status text NOT NULL CHECK (
        status IN ('pending_payment', 'paid', 'confirmed', 'picked_up', 'cancelled', 'refunded')
    ),
    cancel_reason text CHECK (cancel_reason IN ('hold_expired', 'staff', 'customer', 'day_cancelled')),
    total_cents bigint NOT NULL CHECK (total_cents >= 0),
    hold_expires_at timestamptz,
    paid_at timestamptz,
    created_at timestamptz NOT NULL,
    UNIQUE (number_year, number_seq),
    FOREIGN KEY (number_year) REFERENCES order_number (year),
    FOREIGN KEY (pickup_slot_id, bake_day_id) REFERENCES pickup_slot (id, bake_day_id),
    CONSTRAINT order_pending_has_hold CHECK (
        status <> 'pending_payment' OR (hold_expires_at IS NOT NULL AND paid_at IS NULL)
    ),
    CONSTRAINT order_on_account_skips_payment CHECK (
        payment_mode = 'online' OR status NOT IN ('pending_payment', 'paid')
    ),
    CONSTRAINT order_paid_has_payment CHECK (
        status <> 'paid' OR paid_at IS NOT NULL
    ),
    CONSTRAINT order_cancel_reason CHECK (
        (status <> 'cancelled' OR cancel_reason IS NOT NULL)
        AND (cancel_reason IS NULL OR status IN ('cancelled', 'refunded'))
    )
);

CREATE INDEX order_bake_day_idx ON "order" (bake_day_id, status);
CREATE INDEX order_pickup_slot_idx ON "order" (pickup_slot_id);
CREATE INDEX order_customer_idx ON "order" (customer_id);
CREATE INDEX order_pending_hold_idx ON "order" (hold_expires_at) WHERE status = 'pending_payment';

-- Lines copy the price and BTW rate at the moment of sale. unit_price_cents is the
-- chosen price of a pay-what-you-want product; suggested_price_cents is set only then.
CREATE TABLE order_line (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_id bigint NOT NULL REFERENCES "order" (id),
    product_id bigint NOT NULL REFERENCES product (id),
    qty integer NOT NULL CHECK (qty > 0),
    unit_price_cents bigint NOT NULL CHECK (unit_price_cents >= 0),
    suggested_price_cents bigint CHECK (suggested_price_cents > 0),
    vat_rate smallint NOT NULL CHECK (vat_rate IN (9, 21))
);

CREATE INDEX order_line_order_idx ON order_line (order_id);
CREATE INDEX order_line_product_idx ON order_line (product_id);

CREATE TABLE donation_allocation (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_id bigint NOT NULL REFERENCES "order" (id),
    charity_id bigint REFERENCES charity (id),
    amount_cents bigint NOT NULL CHECK (amount_cents >= 0),
    status text NOT NULL CHECK (status IN ('earmarked', 'saved', 'transferred')),
    choose_token text UNIQUE CHECK (choose_token <> ''),
    choose_by timestamptz,
    created_at timestamptz NOT NULL,
    CONSTRAINT donation_allocation_choice CHECK ((choose_token IS NULL) = (choose_by IS NULL)),
    CONSTRAINT donation_allocation_saved CHECK (
        CASE WHEN status = 'saved'
            THEN charity_id IS NULL AND choose_token IS NOT NULL
            ELSE charity_id IS NOT NULL
        END
    )
);

CREATE INDEX donation_allocation_order_idx ON donation_allocation (order_id);
CREATE INDEX donation_allocation_charity_idx ON donation_allocation (charity_id, status);

-- Every payment the provider reports as completed, also one the order cannot take:
-- then refund_due is set with its reason (ADR 0012). Only refunded_cents, method and
-- fee_cents change later, when the provider reports them.
CREATE TABLE payment (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    order_id bigint NOT NULL REFERENCES "order" (id),
    provider_payment_id text NOT NULL UNIQUE CHECK (provider_payment_id <> ''),
    method text CHECK (method <> ''),
    amount_cents bigint NOT NULL CHECK (amount_cents >= 0),
    fee_cents bigint CHECK (fee_cents >= 0),
    status text NOT NULL CHECK (
        status IN ('open', 'pending', 'authorized', 'paid', 'failed', 'canceled', 'expired')
    ),
    refunded_cents bigint NOT NULL DEFAULT 0
        CHECK (refunded_cents BETWEEN 0 AND amount_cents),
    refund_due boolean NOT NULL DEFAULT false,
    refund_reason text CHECK (refund_reason IN (
        'hold_expired', 'day_final', 'order_cancelled', 'order_confirmed', 'paid_twice',
        'amount_mismatch'
    )),
    received_at timestamptz NOT NULL,
    CONSTRAINT payment_refund_reason CHECK (refund_due = (refund_reason IS NOT NULL))
);

CREATE INDEX payment_order_idx ON payment (order_id);
CREATE INDEX payment_refund_due_idx ON payment (received_at)
    WHERE refund_due AND refunded_cents < amount_cents;

CREATE TABLE audit_log (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    entity text NOT NULL CHECK (entity <> ''),
    entity_id bigint NOT NULL,
    action text NOT NULL CHECK (action <> ''),
    before jsonb,
    after jsonb,
    actor text NOT NULL CHECK (actor <> ''),
    at timestamptz NOT NULL
);

CREATE INDEX audit_log_entity_idx ON audit_log (entity, entity_id, id);

CREATE FUNCTION refuse_audit_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'the audit log is append-only'
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER audit_log_append_only BEFORE UPDATE OR DELETE ON audit_log
    FOR EACH ROW EXECUTE FUNCTION refuse_audit_change();

CREATE TRIGGER audit_log_no_truncate BEFORE TRUNCATE ON audit_log
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_audit_change();
