-- F16 (ADR 0045): ingredient prices in whole cents per kilogram from a date on; rows are
-- only ever added.

CREATE TABLE ingredient_price (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ingredient_id bigint NOT NULL REFERENCES ingredient (id),
    price_per_kg_cents bigint NOT NULL CHECK (price_per_kg_cents BETWEEN 0 AND 10000000),
    -- A date in Europe/Amsterdam; the price counts from the start of that day.
    valid_from date NOT NULL,
    created_at timestamptz NOT NULL,
    created_by bigint REFERENCES staff_user (id)
);

-- Several rows with the same valid_from: the one entered last (highest id) counts.
CREATE INDEX ingredient_price_valid_idx
    ON ingredient_price (ingredient_id, valid_from DESC, id DESC);

CREATE FUNCTION refuse_ingredient_price_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'ingredient prices are history; add a new price instead'
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER ingredient_price_history BEFORE UPDATE OR DELETE ON ingredient_price
    FOR EACH ROW EXECUTE FUNCTION refuse_ingredient_price_change();
