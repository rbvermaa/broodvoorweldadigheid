-- F14 (ADR 0037, amended 2026-11-26): a starter is an ingredient whose flour and water count
-- in the recipe's total flour and hydration. Its hydration is grams of water per 100 g of
-- flour, in thousandths of a percent (100% is 100000); only a starter has one.
ALTER TABLE ingredient
    DROP CONSTRAINT ingredient_role_check,
    ADD CONSTRAINT ingredient_role_check
        CHECK (role IN ('flour', 'water', 'other', 'starter')),
    ADD COLUMN starter_hydration_x1000 integer
        CONSTRAINT ingredient_starter_hydration_range
            CHECK (starter_hydration_x1000 BETWEEN 0 AND 500000),
    ADD CONSTRAINT ingredient_starter_has_hydration
        CHECK ((role = 'starter') = (starter_hydration_x1000 IS NOT NULL));
