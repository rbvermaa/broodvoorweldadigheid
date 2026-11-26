-- F14 (ADR 0053): a line's percentage is now relative to the version's own flour lines
-- (the main dough), no longer to all its flour with starters and sub-recipes. Frozen
-- versions and the batches made from them keep their grams: every stored percentage is
-- multiplied by total flour ÷ main flour, rounded half up to 0.001%. Drafts keep their
-- numbers. Versions without flour lines of their own, or whose flour is all in their own
-- lines, do not change. Scaling splits a weight in proportion to the percentages, so
-- multiplying all of a version's lines by one factor leaves the grams as they were, up to
-- the rounding to 0.001%; no allergen approval is cleared for it.

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

CREATE TEMPORARY TABLE main_dough_factor ON COMMIT DROP AS
SELECT v.id AS version_id, main_dough_total_flour(v.id) AS total_flour, m.main_flour
FROM recipe_version v
CROSS JOIN LATERAL (
    SELECT coalesce(sum(l.bakers_pct_x1000), 0)::numeric AS main_flour
    FROM recipe_line l JOIN ingredient i ON i.id = l.ingredient_id
    WHERE l.recipe_version_id = v.id AND i.role = 'flour'
) m
WHERE v.frozen_at IS NOT NULL AND m.main_flour > 0;

DELETE FROM main_dough_factor WHERE total_flour = main_flour;

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

DROP FUNCTION main_dough_total_flour(bigint);
