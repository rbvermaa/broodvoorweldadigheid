CREATE TABLE business_account (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id bigint NOT NULL UNIQUE REFERENCES customer (id),
    company_name text NOT NULL CHECK (btrim(company_name) <> ''),
    kvk text NOT NULL CHECK (kvk ~ '^[0-9]{8}$'),
    btw_id text CHECK (btrim(btw_id) <> ''),
    invoice_address text NOT NULL CHECK (btrim(invoice_address) <> ''),
    invoice_email text NOT NULL
        CHECK (invoice_email <> '' AND invoice_email = lower(btrim(invoice_email))),
    on_account_allowed boolean NOT NULL DEFAULT false
);

-- An invoice made by hand from the overview (F11a). Kept for 7 years; never changed.
CREATE TABLE manual_invoice (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id bigint NOT NULL REFERENCES customer (id),
    invoice_number text NOT NULL UNIQUE CHECK (btrim(invoice_number) <> ''),
    invoice_date date NOT NULL,
    period_from date NOT NULL,
    period_to date NOT NULL,
    total_cents bigint NOT NULL CHECK (total_cents >= 0),
    recorded_at timestamptz NOT NULL,
    recorded_by text NOT NULL CHECK (recorded_by <> ''),
    CHECK (period_from <= period_to)
);

ALTER TABLE "order" ADD COLUMN manual_invoice_id bigint REFERENCES manual_invoice (id);
ALTER TABLE "order" ADD CONSTRAINT order_invoice_on_account
    CHECK (manual_invoice_id IS NULL OR payment_mode = 'on_account');

CREATE INDEX order_uninvoiced_idx ON "order" (customer_id)
    WHERE payment_mode = 'on_account' AND manual_invoice_id IS NULL;

CREATE FUNCTION refuse_row_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% rows are never changed or deleted', TG_TABLE_NAME
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER manual_invoice_append_only BEFORE UPDATE OR DELETE ON manual_invoice
    FOR EACH ROW EXECUTE FUNCTION refuse_row_change();
CREATE TRIGGER manual_invoice_no_truncate BEFORE TRUNCATE ON manual_invoice
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_row_change();

-- An invoiced order is locked: only confirmed -> picked_up may still happen.
-- Corrections are new orders (spec, "Data model").
CREATE FUNCTION refuse_invoiced_order_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.manual_invoice_id IS NOT NULL AND (
        (to_jsonb(NEW) - 'status') IS DISTINCT FROM (to_jsonb(OLD) - 'status')
        OR (NEW.status <> OLD.status
            AND NOT (OLD.status = 'confirmed' AND NEW.status = 'picked_up'))
    ) THEN
        RAISE EXCEPTION 'order % is invoiced and cannot change', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER order_invoiced_locked BEFORE UPDATE ON "order"
    FOR EACH ROW EXECUTE FUNCTION refuse_invoiced_order_change();

CREATE FUNCTION refuse_invoiced_line_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    invoiced boolean;
BEGIN
    SELECT bool_or(manual_invoice_id IS NOT NULL) INTO invoiced FROM "order"
    WHERE id IN (
        CASE WHEN TG_OP = 'DELETE' THEN NULL ELSE NEW.order_id END,
        CASE WHEN TG_OP = 'INSERT' THEN NULL ELSE OLD.order_id END
    );
    IF invoiced THEN
        RAISE EXCEPTION 'the order is invoiced; its lines cannot change'
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END;
$$;

CREATE TRIGGER order_line_invoiced_locked BEFORE INSERT OR UPDATE OR DELETE ON order_line
    FOR EACH ROW EXECUTE FUNCTION refuse_invoiced_line_change();

CREATE TABLE drop_cost (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    bake_day_id bigint NOT NULL REFERENCES bake_day (id),
    kind text NOT NULL CHECK (kind IN ('packaging', 'energy', 'other')),
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    note text CHECK (btrim(note) <> ''),
    created_at timestamptz NOT NULL
);

CREATE INDEX drop_cost_bake_day_idx ON drop_cost (bake_day_id);

-- A bank transfer to a charity (F11b). Kept for 7 years; never changed.
CREATE TABLE charity_transfer (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    charity_id bigint NOT NULL REFERENCES charity (id),
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    transferred_on date NOT NULL,
    bank_reference text NOT NULL CHECK (btrim(bank_reference) <> ''),
    recorded_at timestamptz NOT NULL,
    recorded_by text NOT NULL CHECK (recorded_by <> '')
);

CREATE INDEX charity_transfer_charity_idx ON charity_transfer (charity_id, transferred_on);

-- What one transfer settled per allocation: paid_cents is the allocation's amount the
-- first time it is covered and 0 afterwards; refunded_cents is the part of its refunds
-- settled by this transfer. A refund after a transfer is settled by the next one.
CREATE TABLE charity_transfer_allocation (
    transfer_id bigint NOT NULL REFERENCES charity_transfer (id),
    allocation_id bigint NOT NULL REFERENCES donation_allocation (id),
    paid_cents bigint NOT NULL CHECK (paid_cents >= 0),
    refunded_cents bigint NOT NULL CHECK (refunded_cents >= 0),
    PRIMARY KEY (transfer_id, allocation_id)
);

CREATE INDEX charity_transfer_allocation_allocation_idx
    ON charity_transfer_allocation (allocation_id);

CREATE TRIGGER charity_transfer_append_only BEFORE UPDATE OR DELETE ON charity_transfer
    FOR EACH ROW EXECUTE FUNCTION refuse_row_change();
CREATE TRIGGER charity_transfer_no_truncate BEFORE TRUNCATE ON charity_transfer
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_row_change();
CREATE TRIGGER charity_transfer_allocation_append_only
    BEFORE UPDATE OR DELETE ON charity_transfer_allocation
    FOR EACH ROW EXECUTE FUNCTION refuse_row_change();
CREATE TRIGGER charity_transfer_allocation_no_truncate BEFORE TRUNCATE ON charity_transfer_allocation
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_row_change();

-- Once transferred, an allocation never changes (spec, "Data model").
CREATE FUNCTION refuse_transferred_allocation_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'transferred' THEN
        RAISE EXCEPTION 'donation allocation % is transferred and cannot change', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN CASE WHEN TG_OP = 'DELETE' THEN OLD ELSE NEW END;
END;
$$;

CREATE TRIGGER donation_allocation_transferred_locked
    BEFORE UPDATE OR DELETE ON donation_allocation
    FOR EACH ROW EXECUTE FUNCTION refuse_transferred_allocation_change();
