-- F14 (ADR 0037): ingredients and versioned recipes in baker's percentages.
-- Percentages are integer thousandths of a percent: 100% is 100000.

CREATE TABLE ingredient (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL CHECK (name <> ''),
    label_name text NOT NULL DEFAULT '',
    role text NOT NULL CHECK (role IN ('flour', 'water', 'other')),
    supplier_name text NOT NULL DEFAULT '',
    allergens text[] NOT NULL DEFAULT '{}' CHECK (is_allergen_list(allergens))
);

CREATE UNIQUE INDEX ingredient_name_idx ON ingredient (lower(name));

CREATE TABLE recipe (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL CHECK (name <> ''),
    kind text NOT NULL CHECK (kind IN ('dough', 'preferment', 'soaker', 'filling', 'topping')),
    current_version_id bigint
);

CREATE UNIQUE INDEX recipe_name_idx ON recipe (lower(name));

CREATE TABLE recipe_version (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    recipe_id bigint NOT NULL REFERENCES recipe (id),
    version integer NOT NULL CHECK (version > 0),
    frozen_at timestamptz,
    -- References batch once production adds that table (F14b).
    promoted_from_batch_id bigint,
    lead_time_hours integer CHECK (lead_time_hours BETWEEN 0 AND 168),
    notes text NOT NULL DEFAULT '',
    UNIQUE (recipe_id, version),
    UNIQUE (id, recipe_id)
);

-- At most one draft per recipe: "new version" and editing a frozen version reuse it.
CREATE UNIQUE INDEX recipe_version_one_draft_idx ON recipe_version (recipe_id)
    WHERE frozen_at IS NULL;

ALTER TABLE recipe ADD CONSTRAINT recipe_current_version_fk
    FOREIGN KEY (current_version_id, id) REFERENCES recipe_version (id, recipe_id);

CREATE TABLE recipe_line (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    recipe_version_id bigint NOT NULL REFERENCES recipe_version (id),
    position integer NOT NULL CHECK (position >= 0),
    ingredient_id bigint REFERENCES ingredient (id),
    sub_recipe_version_id bigint REFERENCES recipe_version (id),
    bakers_pct_x1000 integer NOT NULL CHECK (bakers_pct_x1000 BETWEEN 1 AND 1000000),
    CONSTRAINT recipe_line_one_part CHECK (
        (ingredient_id IS NULL) <> (sub_recipe_version_id IS NULL)
    ),
    CONSTRAINT recipe_line_not_itself CHECK (sub_recipe_version_id <> recipe_version_id),
    UNIQUE (recipe_version_id, position)
);

CREATE INDEX recipe_line_sub_idx ON recipe_line (sub_recipe_version_id)
    WHERE sub_recipe_version_id IS NOT NULL;

CREATE FUNCTION refuse_frozen_version_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.frozen_at IS NOT NULL AND NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION 'recipe version % is frozen', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER recipe_version_frozen BEFORE UPDATE ON recipe_version
    FOR EACH ROW EXECUTE FUNCTION refuse_frozen_version_change();

CREATE FUNCTION refuse_frozen_line_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    version_id bigint;
BEGIN
    IF TG_OP = 'DELETE' THEN
        version_id := OLD.recipe_version_id;
    ELSE
        version_id := NEW.recipe_version_id;
    END IF;
    IF EXISTS (SELECT 1 FROM recipe_version WHERE id = version_id AND frozen_at IS NOT NULL)
        OR (TG_OP = 'UPDATE' AND EXISTS (
            SELECT 1 FROM recipe_version WHERE id = OLD.recipe_version_id AND frozen_at IS NOT NULL))
    THEN
        RAISE EXCEPTION 'recipe version % is frozen', version_id
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER recipe_line_frozen BEFORE INSERT OR UPDATE OR DELETE ON recipe_line
    FOR EACH ROW EXECUTE FUNCTION refuse_frozen_line_change();

ALTER TABLE product ADD COLUMN recipe_id bigint REFERENCES recipe (id);
