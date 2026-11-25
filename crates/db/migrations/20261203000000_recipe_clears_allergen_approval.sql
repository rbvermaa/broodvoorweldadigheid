-- F10, F17 (ADR 0037, owner 2026-11-25): changing a product's recipe or an ingredient's
-- allergens clears the product's allergen approval, whoever writes the change.
-- allergens_cleared_reason says why for staff until the next approval.
-- allergens_recipe_rev goes up with every such change, approved or not; an approval must
-- name the revision it reviewed, so a page opened before the change cannot approve.
ALTER TABLE product
    ADD COLUMN allergens_cleared_reason text
        CONSTRAINT product_allergens_cleared_reason_check
            CHECK (allergens_cleared_reason IN ('recipe', 'ingredient')),
    ADD COLUMN allergens_recipe_rev bigint NOT NULL DEFAULT 0,
    ADD CONSTRAINT product_cleared_reason_unapproved CHECK (
        allergens_cleared_reason IS NULL OR allergens_approved_by IS NULL
    );

-- Locks the products in id order first, the same order as checkout (ADR 0038), and waits
-- for an approval still in progress so it is cleared too.
CREATE FUNCTION clear_recipe_allergen_approval(recipe_ids bigint[], why text)
RETURNS void
LANGUAGE sql AS $$
    UPDATE product
    SET allergens_recipe_rev = allergens_recipe_rev + 1,
        allergens_approved_by = NULL, allergens_approved_at = NULL,
        allergens_cleared_reason = CASE WHEN allergens_approved_by IS NOT NULL
            THEN why ELSE allergens_cleared_reason END
    WHERE id IN (
        SELECT id FROM product WHERE recipe_id = ANY(recipe_ids) ORDER BY id FOR UPDATE
    );
$$;

-- Products whose recipe's version in use is one of `version_ids` or uses one of them as
-- sub-recipe, at any depth.
CREATE FUNCTION clear_allergen_approval_for_versions(version_ids bigint[], why text)
RETURNS void
LANGUAGE sql AS $$
    SELECT clear_recipe_allergen_approval(ARRAY(
        WITH RECURSIVE up (version_id) AS (
            SELECT unnest(version_ids)
            UNION
            SELECT l.recipe_version_id
            FROM recipe_line l JOIN up u ON l.sub_recipe_version_id = u.version_id
        )
        SELECT r.id FROM recipe r JOIN up u ON r.current_version_id = u.version_id
    ), why);
$$;

CREATE FUNCTION product_recipe_allergen_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.allergens_approved_by IS NOT NULL THEN
        NEW.allergens_cleared_reason := NULL;
    END IF;
    IF NEW.recipe_id IS DISTINCT FROM OLD.recipe_id THEN
        NEW.allergens_recipe_rev := OLD.allergens_recipe_rev + 1;
        IF NEW.allergens_approved_by IS NOT NULL THEN
            NEW.allergens_approved_by := NULL;
            NEW.allergens_approved_at := NULL;
            NEW.allergens_cleared_reason := 'recipe';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER product_recipe_allergen_approval BEFORE UPDATE ON product
    FOR EACH ROW EXECUTE FUNCTION product_recipe_allergen_approval();

CREATE FUNCTION recipe_current_clears_allergen_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM clear_recipe_allergen_approval(ARRAY[NEW.id], 'recipe');
    RETURN NULL;
END;
$$;

CREATE TRIGGER recipe_current_clears_allergen_approval AFTER UPDATE OF current_version_id
    ON recipe
    FOR EACH ROW WHEN (NEW.current_version_id IS DISTINCT FROM OLD.current_version_id)
    EXECUTE FUNCTION recipe_current_clears_allergen_approval();

CREATE FUNCTION recipe_line_clears_allergen_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM clear_allergen_approval_for_versions(ARRAY[NEW.recipe_version_id], 'recipe');
    ELSIF TG_OP = 'DELETE' THEN
        PERFORM clear_allergen_approval_for_versions(ARRAY[OLD.recipe_version_id], 'recipe');
    ELSIF NEW IS DISTINCT FROM OLD THEN
        PERFORM clear_allergen_approval_for_versions(
            ARRAY[OLD.recipe_version_id, NEW.recipe_version_id], 'recipe');
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER recipe_line_clears_allergen_approval AFTER INSERT OR UPDATE OR DELETE
    ON recipe_line
    FOR EACH ROW EXECUTE FUNCTION recipe_line_clears_allergen_approval();

-- The label name feeds the ingredient text (F17); an empty one means the name.
CREATE FUNCTION ingredient_clears_allergen_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM clear_allergen_approval_for_versions(
        ARRAY(SELECT DISTINCT recipe_version_id FROM recipe_line WHERE ingredient_id = NEW.id),
        'ingredient');
    RETURN NULL;
END;
$$;

CREATE TRIGGER ingredient_clears_allergen_approval AFTER UPDATE ON ingredient
    FOR EACH ROW WHEN (
        NOT (NEW.allergens @> OLD.allergens AND NEW.allergens <@ OLD.allergens)
        OR coalesce(nullif(NEW.label_name, ''), NEW.name)
            IS DISTINCT FROM coalesce(nullif(OLD.label_name, ''), OLD.name)
    )
    EXECUTE FUNCTION ingredient_clears_allergen_approval();
