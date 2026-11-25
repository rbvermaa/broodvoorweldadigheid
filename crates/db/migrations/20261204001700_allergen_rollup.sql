-- F17 (ADR 0046): the ingredient list computed from the recipe, as approved for a product
-- and, when a batch differs from that, for the batch itself. A list is stored as two
-- arrays of the same length: the names in label order and whether each is an allergen
-- (bold on labels). NULL on a product means its ingredient text was entered by hand.
ALTER TABLE product
    ADD COLUMN ingredient_names text[],
    ADD COLUMN ingredient_emphasis boolean[],
    ADD CONSTRAINT product_ingredient_list_complete CHECK (
        (ingredient_names IS NULL) = (ingredient_emphasis IS NULL)
        AND cardinality(ingredient_names) = cardinality(ingredient_emphasis)
    );

CREATE FUNCTION product_ingredient_list_clears_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.ingredient_names IS DISTINCT FROM OLD.ingredient_names
        OR NEW.ingredient_emphasis IS DISTINCT FROM OLD.ingredient_emphasis
    THEN
        NEW.allergens_approved_by := NULL;
        NEW.allergens_approved_at := NULL;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER product_ingredient_list_clears_approval BEFORE UPDATE ON product
    FOR EACH ROW EXECUTE FUNCTION product_ingredient_list_clears_approval();

-- A batch's own approval for labels: the allergens and list someone approved. It counts
-- only while the batch's computed values still equal them, so nothing clears it.
ALTER TABLE batch
    ADD COLUMN allergens text[] CHECK (is_allergen_list(allergens)),
    ADD COLUMN ingredient_names text[],
    ADD COLUMN ingredient_emphasis boolean[],
    ADD COLUMN allergens_approved_by bigint REFERENCES staff_user (id),
    ADD COLUMN allergens_approved_at timestamptz,
    ADD CONSTRAINT batch_label_approval_complete CHECK (
        (allergens IS NULL) = (allergens_approved_by IS NULL)
        AND (allergens IS NULL) = (allergens_approved_at IS NULL)
        AND (allergens IS NULL) = (ingredient_names IS NULL)
        AND (allergens IS NULL) = (ingredient_emphasis IS NULL)
        AND cardinality(ingredient_names) = cardinality(ingredient_emphasis)
    );
