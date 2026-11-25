-- F17 (ADR 0046): a product made from a recipe takes only computed allergen data. A hand
-- approval it still holds (no stored ingredient list) is taken away, so the shop blocks
-- until the baker approves the computed values.
UPDATE product
SET allergens_approved_by = NULL, allergens_approved_at = NULL,
    allergens_cleared_reason = 'recipe'
WHERE recipe_id IS NOT NULL AND ingredient_names IS NULL
    AND allergens_approved_by IS NOT NULL;
