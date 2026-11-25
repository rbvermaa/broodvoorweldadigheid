-- F18, F18a (ADR 0047): what labels need beyond the allergen data, and the pieces of a
-- batch that went into the freezer.

ALTER TABLE product
    ADD COLUMN shelf_life_days integer CHECK (shelf_life_days BETWEEN 1 AND 3650),
    ADD COLUMN storage_nl text NOT NULL DEFAULT '';

ALTER TABLE product
    ADD CONSTRAINT product_freezer_days_max CHECK (freezer_days <= 3650);

-- frozen_on is the bakery's date when the pieces were first recorded; a correction keeps it.
ALTER TABLE batch_output
    ADD COLUMN frozen_pieces integer CHECK (frozen_pieces BETWEEN 1 AND 100000),
    ADD COLUMN frozen_on date,
    ADD CONSTRAINT batch_output_frozen_on CHECK ((frozen_pieces IS NULL) = (frozen_on IS NULL)),
    ADD CONSTRAINT batch_output_frozen_good
        CHECK (frozen_pieces IS NULL
            OR (actual_pieces IS NOT NULL AND frozen_pieces <= actual_pieces - waste_pieces));
