-- A refund asked of the payment provider (F2, F6, ADR 0026). A retry after a failure
-- updates the same row, so reports count each refund once, whatever its status.
CREATE TABLE refund (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    payment_id bigint NOT NULL REFERENCES payment (id),
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    -- What was refunded or owed back on the payment before this refund.
    refunded_before_cents bigint NOT NULL DEFAULT 0 CHECK (refunded_before_cents >= 0),
    status text NOT NULL CHECK (status IN ('requested', 'done', 'failed')),
    provider_refund_id text UNIQUE CHECK (provider_refund_id <> ''),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error text,
    requested_by text NOT NULL CHECK (requested_by <> ''),
    requested_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE INDEX refund_payment_idx ON refund (payment_id);
CREATE INDEX refund_open_idx ON refund (requested_at) WHERE status <> 'done';

-- Only a payment the order accepted can be refunded this way, and never for more than
-- was paid. Locks the payment row, so two inserts for one payment wait for each other.
CREATE FUNCTION check_refund_amount() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    paid bigint;
    others bigint;
BEGIN
    SELECT amount_cents INTO paid FROM payment
    WHERE id = NEW.payment_id AND status = 'paid' AND NOT refund_due
    FOR UPDATE;
    IF paid IS NULL THEN
        RAISE EXCEPTION 'payment % is not an accepted payment', NEW.payment_id
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT COALESCE(sum(amount_cents), 0) INTO others FROM refund
    WHERE payment_id = NEW.payment_id AND id <> NEW.id;
    IF others + NEW.amount_cents > paid THEN
        RAISE EXCEPTION 'refunds of payment % exceed what was paid', NEW.payment_id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER refund_within_payment BEFORE INSERT OR UPDATE OF payment_id, amount_cents
    ON refund FOR EACH ROW EXECUTE FUNCTION check_refund_amount();
CREATE TRIGGER refund_no_delete BEFORE DELETE ON refund
    FOR EACH ROW EXECUTE FUNCTION refuse_row_change();
CREATE TRIGGER refund_no_truncate BEFORE TRUNCATE ON refund
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_row_change();

-- What is refunded or owed back on a payment: the most any refund row says (what was
-- refunded before it plus its own amount, whatever its status) or what the provider
-- reported, never more than was paid.
CREATE FUNCTION payment_refunded_cents(payment bigint) RETURNS bigint
LANGUAGE sql STABLE AS $$
    SELECT LEAST(p.amount_cents, GREATEST(p.refunded_cents,
        COALESCE(max(r.refunded_before_cents + r.amount_cents), 0)))::bigint
    FROM payment p LEFT JOIN refund r ON r.payment_id = p.id
    WHERE p.id = payment
    GROUP BY p.id
$$;
