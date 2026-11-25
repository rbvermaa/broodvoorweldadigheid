-- F14 (ADR 0049): optional instructions per recipe version, as free text or as steps.
-- They freeze with the version and never touch allergens, labels or costing.

ALTER TABLE recipe_version
    ADD COLUMN method_kind text NOT NULL DEFAULT 'none'
        CHECK (method_kind IN ('none', 'text', 'steps')),
    ADD COLUMN method_text text NOT NULL DEFAULT ''
        CHECK (char_length(method_text) <= 10000),
    ADD CONSTRAINT recipe_version_method_text CHECK (method_kind = 'text' OR method_text = '');

CREATE TABLE recipe_step (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    recipe_version_id bigint NOT NULL REFERENCES recipe_version (id),
    position integer NOT NULL CHECK (position >= 0),
    name text NOT NULL CHECK (name <> '' AND char_length(name) <= 80),
    duration_min integer CHECK (duration_min BETWEEN 1 AND 10080),
    temperature_c integer CHECK (temperature_c BETWEEN -30 AND 300),
    notes text NOT NULL DEFAULT '' CHECK (char_length(notes) <= 1000),
    UNIQUE (recipe_version_id, position)
);

-- Same check as for lines: nothing in a frozen version changes.
CREATE TRIGGER recipe_step_frozen BEFORE INSERT OR UPDATE OR DELETE ON recipe_step
    FOR EACH ROW EXECUTE FUNCTION refuse_frozen_line_change();

-- Steps exist only on a version whose instructions are steps, so text and steps never
-- both hold content.
CREATE FUNCTION refuse_step_without_steps_kind() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM recipe_version WHERE id = NEW.recipe_version_id AND method_kind = 'steps'
    ) THEN
        RAISE EXCEPTION 'recipe version % does not have steps', NEW.recipe_version_id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER recipe_step_kind BEFORE INSERT OR UPDATE ON recipe_step
    FOR EACH ROW EXECUTE FUNCTION refuse_step_without_steps_kind();

CREATE FUNCTION refuse_method_kind_with_steps() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM recipe_step WHERE recipe_version_id = NEW.id) THEN
        RAISE EXCEPTION 'recipe version % still has steps', NEW.id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER recipe_version_method_kind BEFORE UPDATE OF method_kind ON recipe_version
    FOR EACH ROW WHEN (OLD.method_kind = 'steps' AND NEW.method_kind <> 'steps')
    EXECUTE FUNCTION refuse_method_kind_with_steps();
