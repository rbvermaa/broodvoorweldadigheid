-- ADR 0048: removes what earlier releases left unused. Never deployed; owner approved.

-- ADR 0026: capacity pools. The column's foreign key goes with it.
ALTER TABLE bake_day_product DROP COLUMN capacity_pool_id, DROP COLUMN units_per_item;
DROP TABLE capacity_pool;

-- ADR 0006, 0027: lead times now come from the sourdough rule and the recipe version.
ALTER TABLE product DROP COLUMN lead_time_hours;
ALTER TABLE company DROP COLUMN default_lead_time_hours;

-- ADR 0038: the kitchen-wide list replaced the product's own may-contain list.
CREATE OR REPLACE FUNCTION clear_allergen_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.allergens IS DISTINCT FROM OLD.allergens
        OR NEW.ingredient_text IS DISTINCT FROM OLD.ingredient_text
    THEN
        NEW.allergens_approved_by := NULL;
        NEW.allergens_approved_at := NULL;
    END IF;
    RETURN NEW;
END;
$$;
ALTER TABLE product DROP COLUMN may_contain;

-- ADR 0030: only the hash of a choose link is stored.
ALTER TABLE donation_allocation DROP CONSTRAINT donation_allocation_choice;
ALTER TABLE donation_allocation DROP CONSTRAINT donation_allocation_saved;
ALTER TABLE donation_allocation DROP COLUMN choose_token;
ALTER TABLE donation_allocation ADD CONSTRAINT donation_allocation_choice CHECK (
    (choose_token_hash IS NULL) = (choose_by IS NULL)
);
ALTER TABLE donation_allocation ADD CONSTRAINT donation_allocation_saved CHECK (
    CASE WHEN status = 'saved'
        THEN charity_id IS NULL AND choose_token_hash IS NOT NULL
        ELSE charity_id IS NOT NULL
    END
);

-- ADR 0044: the foreign key to email_kind already refuses an unknown or empty kind.
ALTER TABLE email_outbox DROP CONSTRAINT email_outbox_kind_check;
