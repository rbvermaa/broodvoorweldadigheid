CREATE FUNCTION is_allergen_list(codes text[]) RETURNS boolean
LANGUAGE sql IMMUTABLE STRICT
RETURN codes <@ ARRAY[
    'cereals_containing_gluten', 'crustaceans', 'eggs', 'fish', 'peanuts', 'soybeans',
    'milk', 'nuts', 'celery', 'mustard', 'sesame_seeds', 'sulphur_dioxide_and_sulphites',
    'lupin', 'molluscs'
]::text[];

CREATE TABLE product (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name_nl text NOT NULL CHECK (name_nl <> ''),
    description_nl text NOT NULL DEFAULT '',
    photo_path text,
    price_incl_cents bigint NOT NULL CHECK (price_incl_cents >= 0),
    vat_rate smallint NOT NULL CHECK (vat_rate IN (9, 21)),
    pay_what_you_want boolean NOT NULL DEFAULT false,
    min_price_cents bigint,
    max_price_cents bigint,
    unit_weight_g integer NOT NULL CHECK (unit_weight_g > 0),
    dough_g_per_piece integer NOT NULL CHECK (dough_g_per_piece >= 0),
    freezer_days integer CHECK (freezer_days > 0),
    lead_time_hours integer CHECK (lead_time_hours BETWEEN 0 AND 168),
    allergens text[] NOT NULL DEFAULT '{}' CHECK (is_allergen_list(allergens)),
    may_contain text[] NOT NULL DEFAULT '{}' CHECK (is_allergen_list(may_contain)),
    ingredient_text text NOT NULL DEFAULT '',
    allergens_approved_by bigint,
    allergens_approved_at timestamptz,
    packaged boolean NOT NULL DEFAULT false,
    sold_online boolean NOT NULL DEFAULT true,
    estimated_cost_cents bigint CHECK (estimated_cost_cents >= 0),
    active boolean NOT NULL DEFAULT true,
    CONSTRAINT product_pwyw_bounds CHECK (
        CASE WHEN pay_what_you_want
            THEN min_price_cents IS NOT NULL AND max_price_cents IS NOT NULL
                AND 0 < min_price_cents AND min_price_cents <= price_incl_cents
                AND price_incl_cents <= max_price_cents
            ELSE min_price_cents IS NULL AND max_price_cents IS NULL
        END
    ),
    CONSTRAINT product_approval_complete CHECK (
        (allergens_approved_by IS NULL) = (allergens_approved_at IS NULL)
    )
);

CREATE INDEX product_active_idx ON product (name_nl) WHERE active;

CREATE FUNCTION clear_allergen_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.allergens IS DISTINCT FROM OLD.allergens
        OR NEW.may_contain IS DISTINCT FROM OLD.may_contain
        OR NEW.ingredient_text IS DISTINCT FROM OLD.ingredient_text
    THEN
        NEW.allergens_approved_by := NULL;
        NEW.allergens_approved_at := NULL;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER product_clear_allergen_approval BEFORE UPDATE ON product
    FOR EACH ROW EXECUTE FUNCTION clear_allergen_approval();
