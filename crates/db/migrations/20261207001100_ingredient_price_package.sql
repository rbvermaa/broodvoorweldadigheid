-- F16 (ADR 0045): a price as paid, price_cents for weight_g grams. Older rows have neither
-- and mean price_per_kg_cents for 1000 g. On newer rows price_per_kg_cents is the price per
-- kilogram rounded half up to a cent, so a reader that only knows that column still works.

ALTER TABLE ingredient_price
    ADD COLUMN price_cents bigint CHECK (price_cents BETWEEN 0 AND 10000000),
    ADD COLUMN weight_g integer CHECK (weight_g BETWEEN 1 AND 1000000),
    ADD CONSTRAINT ingredient_price_package_check CHECK (
        (price_cents IS NULL AND weight_g IS NULL)
        OR (price_cents IS NOT NULL AND weight_g IS NOT NULL
            AND price_per_kg_cents = (price_cents * 2000 + weight_g) / (2 * weight_g))
    );
