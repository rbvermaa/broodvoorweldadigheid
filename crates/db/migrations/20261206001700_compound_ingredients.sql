-- F17 (ADR 0046, owner 2026-11-25): a sub-recipe can show on the ingredient list as a
-- compound ingredient, with its own ingredients in brackets. label_compound NULL follows
-- the kind (fillings and toppings are compound); an empty label_name means the name.
ALTER TABLE recipe
    ADD COLUMN label_compound boolean,
    ADD COLUMN label_name text NOT NULL DEFAULT ''
        CONSTRAINT recipe_label_name_length CHECK (char_length(label_name) <= 80);

-- The recipe's name as a compound ingredient; NULL when its ingredients merge into the
-- list of the recipe that uses it. Same rule as recipes::Recipe::compound_name.
CREATE FUNCTION recipe_compound_name(
    kind text, label_compound boolean, label_name text, name text
) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE WHEN coalesce(label_compound, kind IN ('filling', 'topping'))
        THEN coalesce(nullif(label_name, ''), name) END
$$;

-- Clears the products whose version in use holds a version of `recipe_id` as sub-recipe,
-- at any depth. The recipe's own products are left alone: at the top the setting does
-- not count.
CREATE FUNCTION clear_allergen_approval_for_sub_recipe(recipe_id bigint)
RETURNS void
LANGUAGE sql AS $$
    SELECT clear_allergen_approval_for_versions(ARRAY(
        SELECT DISTINCT l.recipe_version_id
        FROM recipe_line l JOIN recipe_version v ON v.id = l.sub_recipe_version_id
        WHERE v.recipe_id = clear_allergen_approval_for_sub_recipe.recipe_id
    ), 'recipe');
$$;

CREATE FUNCTION recipe_label_clears_allergen_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM clear_allergen_approval_for_sub_recipe(NEW.id);
    RETURN NULL;
END;
$$;

CREATE TRIGGER recipe_label_clears_allergen_approval AFTER UPDATE ON recipe
    FOR EACH ROW WHEN (
        recipe_compound_name(NEW.kind, NEW.label_compound, NEW.label_name, NEW.name)
            IS DISTINCT FROM
        recipe_compound_name(OLD.kind, OLD.label_compound, OLD.label_name, OLD.name)
    )
    EXECUTE FUNCTION recipe_label_clears_allergen_approval();

-- A list's entries in pre-order: a compound at depth 0, its ingredients after it at
-- depth 1. NULL on a list stored before this column: every entry at depth 0.
ALTER TABLE product
    ADD COLUMN ingredient_depth smallint[],
    ADD CONSTRAINT product_ingredient_depth_complete CHECK (
        (ingredient_depth IS NULL OR ingredient_names IS NOT NULL)
        AND cardinality(ingredient_depth) = cardinality(ingredient_names)
    );

ALTER TABLE batch
    ADD COLUMN ingredient_depth smallint[],
    ADD CONSTRAINT batch_ingredient_depth_complete CHECK (
        (ingredient_depth IS NULL OR ingredient_names IS NOT NULL)
        AND cardinality(ingredient_depth) = cardinality(ingredient_names)
    );

CREATE OR REPLACE FUNCTION product_ingredient_list_clears_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.allergens_recipe_rev IS DISTINCT FROM OLD.allergens_recipe_rev
        OR NEW.recipe_id IS DISTINCT FROM OLD.recipe_id
    THEN
        NEW.ingredient_names := NULL;
        NEW.ingredient_emphasis := NULL;
        NEW.ingredient_depth := NULL;
        IF NEW.recipe_id IS DISTINCT FROM OLD.recipe_id
            AND NEW.allergens_approved_by IS NOT NULL
        THEN
            NEW.allergens_cleared_reason := 'recipe';
        END IF;
    END IF;
    IF NEW.ingredient_names IS DISTINCT FROM OLD.ingredient_names
        OR NEW.ingredient_emphasis IS DISTINCT FROM OLD.ingredient_emphasis
        OR NEW.ingredient_depth IS DISTINCT FROM OLD.ingredient_depth
    THEN
        NEW.allergens_approved_by := NULL;
        NEW.allergens_approved_at := NULL;
    END IF;
    RETURN NEW;
END;
$$;

-- Fillings and toppings already in use now compute a compound ingredient: their
-- products are approved again from the new list.
SELECT clear_allergen_approval_for_sub_recipe(id) FROM recipe
WHERE kind IN ('filling', 'topping');
