-- F14 (ADR 0053): a line's percentage is now relative to the version's own flour lines
-- (the main dough), no longer to all its flour with starters and sub-recipes. Versions
-- written the old way keep their grams: every stored percentage is multiplied by total
-- flour ÷ main flour, rounded half up to 0.001%, and so are the lines of the batches made
-- from them. Old-way versions are the frozen ones, and drafts whose flour adds up to 100%
-- over all flour but not over their own flour lines (copied from a frozen version).
-- Scaling splits a weight in proportion to the percentages, so the grams stay the same up
-- to that rounding. Rounding can still turn equal weights into unequal ones and so
-- reorder an ingredient list: a product or batch whose list order changes loses its
-- allergen approval; the others keep it.

-- All flour of a version in the units of its own lines: own flour lines count in full,
-- a starter for 100 / (100 + hydration), a sub-recipe for its own share of flour.
CREATE FUNCTION main_dough_total_flour(version bigint) RETURNS numeric
LANGUAGE plpgsql STABLE AS $$
DECLARE
    total numeric := 0;
    sub_sum numeric;
    line record;
BEGIN
    FOR line IN
        SELECT l.bakers_pct_x1000 AS pct, l.sub_recipe_version_id AS sub, i.role,
            i.starter_hydration_x1000 AS hydration
        FROM recipe_line l LEFT JOIN ingredient i ON i.id = l.ingredient_id
        WHERE l.recipe_version_id = version
    LOOP
        IF line.sub IS NOT NULL THEN
            SELECT sum(bakers_pct_x1000) INTO sub_sum
            FROM recipe_line WHERE recipe_version_id = line.sub;
            IF sub_sum > 0 THEN
                total := total + line.pct * main_dough_total_flour(line.sub) / sub_sum;
            END IF;
        ELSIF line.role = 'flour' THEN
            total := total + line.pct;
        ELSIF line.role = 'starter' AND line.hydration IS NOT NULL THEN
            total := total + line.pct * 100000.0 / (100000 + line.hydration);
        END IF;
    END LOOP;
    RETURN total;
END;
$$;

-- Every ingredient line below a version with its share of the dough, its path of
-- positions (pre-order: the order a recipe first uses an ingredient) and the outermost
-- compound sub-recipe it is in, as recipes::rollup weighs them.
CREATE FUNCTION main_dough_leaves(
    p_version bigint, p_weight numeric, p_compound bigint, p_path text
) RETURNS TABLE (leaf_path text, leaf_ingredient bigint, leaf_weight numeric, leaf_compound bigint)
LANGUAGE plpgsql STABLE AS $$
DECLARE
    total numeric;
    line record;
BEGIN
    SELECT sum(bakers_pct_x1000) INTO total FROM recipe_line WHERE recipe_version_id = p_version;
    IF total IS NULL OR total = 0 THEN
        RETURN;
    END IF;
    FOR line IN
        SELECT l.position, l.bakers_pct_x1000 AS pct, l.ingredient_id,
            l.sub_recipe_version_id AS sub, r.id AS recipe_id,
            recipe_compound_name(r.kind, r.label_compound, r.label_name, r.name) AS compound_name
        FROM recipe_line l
        LEFT JOIN recipe_version sv ON sv.id = l.sub_recipe_version_id
        LEFT JOIN recipe r ON r.id = sv.recipe_id
        WHERE l.recipe_version_id = p_version
        ORDER BY l.position
    LOOP
        IF line.sub IS NULL THEN
            RETURN QUERY SELECT p_path || lpad(line.position::text, 4, '0'),
                line.ingredient_id, p_weight * line.pct / total, p_compound;
        ELSE
            RETURN QUERY SELECT * FROM main_dough_leaves(line.sub, p_weight * line.pct / total,
                coalesce(p_compound,
                    CASE WHEN line.compound_name IS NOT NULL THEN line.recipe_id END),
                p_path || lpad(line.position::text, 4, '0') || '.');
        END IF;
    END LOOP;
END;
$$;

-- The order of an ingredient list, as recipes::rollup::compute builds it: entries by
-- label name (case-insensitive) or compound name, heaviest first, equal weights in the
-- order of first use; a compound's ingredients the same way inside it. For a version,
-- or for a batch's own lines when `batch` is given.
CREATE FUNCTION main_dough_list_order(version bigint, batch bigint) RETURNS text
LANGUAGE sql STABLE AS $$
    WITH one AS (SELECT 1.0000000000000000000000000000000000000000::numeric AS w),
    top AS (
        SELECT l.position, l.actual_pct_x1000 AS pct, l.ingredient_id,
            l.sub_recipe_version_id AS sub
        FROM batch_line l
        WHERE l.batch_id = main_dough_list_order.batch AND l.actual_pct_x1000 > 0
    ),
    top_sum AS (SELECT sum(pct) AS total FROM top),
    leaves AS (
        SELECT x.*
        FROM one CROSS JOIN LATERAL main_dough_leaves(main_dough_list_order.version, one.w, NULL, '') x
        WHERE main_dough_list_order.batch IS NULL
        UNION ALL
        SELECT lpad(t.position::text, 4, '0'), t.ingredient_id, one.w * t.pct / s.total, NULL
        FROM top t CROSS JOIN one CROSS JOIN top_sum s
        WHERE t.ingredient_id IS NOT NULL
        UNION ALL
        SELECT x.*
        FROM top t CROSS JOIN one CROSS JOIN top_sum s
        JOIN recipe_version sv ON sv.id = t.sub
        JOIN recipe r ON r.id = sv.recipe_id
        CROSS JOIN LATERAL main_dough_leaves(t.sub, one.w * t.pct / s.total,
            CASE WHEN recipe_compound_name(r.kind, r.label_compound, r.label_name, r.name)
                IS NOT NULL THEN r.id END,
            lpad(t.position::text, 4, '0') || '.') x
    ),
    named AS (
        SELECT l.leaf_path, round(l.leaf_weight, 30) AS weight,
            lower(coalesce(nullif(i.label_name, ''), i.name)) AS label,
            lower(recipe_compound_name(r.kind, r.label_compound, r.label_name, r.name)) AS compound
        FROM leaves l
        JOIN ingredient i ON i.id = l.leaf_ingredient
        LEFT JOIN recipe r ON r.id = l.leaf_compound
    ),
    entries AS (
        SELECT CASE WHEN compound IS NULL THEN 'i:' || label ELSE 'c:' || compound END AS key,
            sum(weight) AS weight, min(leaf_path) AS first
        FROM named GROUP BY 1
    ),
    parts AS (
        SELECT 'c:' || compound AS key, label, sum(weight) AS weight, min(leaf_path) AS first
        FROM named WHERE compound IS NOT NULL GROUP BY 1, 2
    )
    SELECT coalesce(string_agg(e.key || coalesce('(' || (
        SELECT string_agg(p.label, ',' ORDER BY p.weight DESC, p.first)
        FROM parts p WHERE p.key = e.key
    ) || ')', ''), ';' ORDER BY e.weight DESC, e.first), '')
    FROM entries e
$$;

CREATE TEMPORARY TABLE main_dough_factor ON COMMIT DROP AS
SELECT v.id AS version_id, main_dough_total_flour(v.id) AS total_flour, m.main_flour,
    v.frozen_at IS NOT NULL AS frozen
FROM recipe_version v
CROSS JOIN LATERAL (
    SELECT coalesce(sum(l.bakers_pct_x1000), 0)::numeric AS main_flour
    FROM recipe_line l JOIN ingredient i ON i.id = l.ingredient_id
    WHERE l.recipe_version_id = v.id AND i.role = 'flour'
) m
WHERE m.main_flour > 0;

DELETE FROM main_dough_factor
WHERE total_flour = main_flour
    OR (NOT frozen AND round(total_flour / 100) <> 1000);

-- A version whose lines, or whose batches' lines, would go above the 1000% a line may
-- hold keeps its numbers and shows the main-flour warning.
DELETE FROM main_dough_factor f
WHERE EXISTS (
    SELECT 1 FROM recipe_line l
    WHERE l.recipe_version_id = f.version_id
        AND round(l.bakers_pct_x1000 * f.total_flour / f.main_flour) > 1000000
) OR EXISTS (
    SELECT 1 FROM batch b JOIN batch_line l ON l.batch_id = b.id
    WHERE b.recipe_version_id = f.version_id
        AND (round(l.actual_pct_x1000 * f.total_flour / f.main_flour) > 1000000
            OR round(coalesce(l.base_pct_x1000, 0) * f.total_flour / f.main_flour) > 1000000)
);

CREATE TEMPORARY TABLE main_dough_list ON COMMIT DROP AS
SELECT 'recipe' AS owner, r.id, main_dough_list_order(r.current_version_id, NULL) AS before
FROM recipe r WHERE r.current_version_id IS NOT NULL
UNION ALL
SELECT 'batch', b.id, main_dough_list_order(NULL, b.id)
FROM batch b WHERE b.allergens_approved_by IS NOT NULL;

ALTER TABLE recipe_line
    DISABLE TRIGGER recipe_line_frozen,
    DISABLE TRIGGER recipe_line_clears_allergen_approval;
ALTER TABLE batch_line DISABLE TRIGGER batch_line_locked;

UPDATE recipe_line l
SET bakers_pct_x1000 = round(l.bakers_pct_x1000 * f.total_flour / f.main_flour)
FROM main_dough_factor f
WHERE l.recipe_version_id = f.version_id;

UPDATE batch_line l
SET base_pct_x1000 = round(l.base_pct_x1000 * f.total_flour / f.main_flour),
    actual_pct_x1000 = round(l.actual_pct_x1000 * f.total_flour / f.main_flour)
FROM batch b JOIN main_dough_factor f ON f.version_id = b.recipe_version_id
WHERE l.batch_id = b.id;

ALTER TABLE recipe_line
    ENABLE TRIGGER recipe_line_frozen,
    ENABLE TRIGGER recipe_line_clears_allergen_approval;
ALTER TABLE batch_line ENABLE TRIGGER batch_line_locked;

SELECT clear_recipe_allergen_approval(ARRAY(
    SELECT m.id FROM main_dough_list m JOIN recipe r ON r.id = m.id
    WHERE m.owner = 'recipe'
        AND m.before <> main_dough_list_order(r.current_version_id, NULL)
    ORDER BY m.id
), 'recipe');

UPDATE batch b
SET allergens = NULL, ingredient_names = NULL, ingredient_emphasis = NULL,
    ingredient_depth = NULL, allergens_approved_by = NULL, allergens_approved_at = NULL
FROM main_dough_list m
WHERE m.owner = 'batch' AND m.id = b.id AND m.before <> main_dough_list_order(NULL, b.id);

DROP FUNCTION main_dough_list_order(bigint, bigint);
DROP FUNCTION main_dough_leaves(bigint, numeric, bigint, text);
DROP FUNCTION main_dough_total_flour(bigint);
