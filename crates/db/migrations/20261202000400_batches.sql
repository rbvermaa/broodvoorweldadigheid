-- F14a (ADR 0042): batches per bake day, their own copy of the recipe lines, and what
-- came out of the oven. Percentages are integer thousandths of a percent, as in recipe_line.

-- Dough per batch in grams. NULL until the owner gives a value; then only manual batches.
ALTER TABLE company
    ADD COLUMN mixer_capacity_g integer CONSTRAINT company_mixer_capacity_g
        CHECK (mixer_capacity_g BETWEEN 1 AND 1000000);

CREATE TABLE batch (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    bake_day_id bigint NOT NULL REFERENCES bake_day (id),
    recipe_version_id bigint NOT NULL REFERENCES recipe_version (id),
    -- The batch code is code_date as YYMMDD, a dash and mix_no; it never changes once
    -- printed, also when the bake day moves.
    code_date date NOT NULL,
    mix_no integer NOT NULL CHECK (mix_no > 0),
    origin text NOT NULL CHECK (origin IN ('cutoff', 'manual')),
    status text NOT NULL DEFAULT 'planned'
        CHECK (status IN ('planned', 'adjusted', 'mixed', 'baked')),
    notes text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL,
    mixed_at timestamptz,
    baked_at timestamptz,
    UNIQUE (code_date, mix_no),
    CONSTRAINT batch_mixed_at CHECK ((status IN ('mixed', 'baked')) = (mixed_at IS NOT NULL)),
    CONSTRAINT batch_baked_at CHECK ((status = 'baked') = (baked_at IS NOT NULL))
);

CREATE INDEX batch_bake_day_idx ON batch (bake_day_id, id);

CREATE TABLE batch_line (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    batch_id bigint NOT NULL REFERENCES batch (id),
    position integer NOT NULL CHECK (position >= 0),
    ingredient_id bigint REFERENCES ingredient (id),
    sub_recipe_version_id bigint REFERENCES recipe_version (id),
    -- NULL for a line the baker added; 0 in actual means the line is left out.
    base_pct_x1000 integer CHECK (base_pct_x1000 BETWEEN 1 AND 1000000),
    actual_pct_x1000 integer NOT NULL CHECK (actual_pct_x1000 BETWEEN 0 AND 1000000),
    reason text,
    CONSTRAINT batch_line_one_part CHECK (
        (ingredient_id IS NULL) <> (sub_recipe_version_id IS NULL)
    ),
    UNIQUE (batch_id, position)
);

CREATE TABLE batch_output (
    batch_id bigint NOT NULL REFERENCES batch (id),
    product_id bigint NOT NULL REFERENCES product (id),
    -- Copied from the product, so a later change does not move a batch's dough weight.
    dough_g_per_piece integer NOT NULL CHECK (dough_g_per_piece BETWEEN 1 AND 1000000),
    planned_pieces integer NOT NULL CHECK (planned_pieces BETWEEN 0 AND 100000),
    actual_pieces integer CHECK (actual_pieces BETWEEN 0 AND 100000),
    waste_pieces integer CHECK (waste_pieces BETWEEN 0 AND 100000),
    PRIMARY KEY (batch_id, product_id),
    CONSTRAINT batch_output_waste CHECK (waste_pieces <= actual_pieces),
    CONSTRAINT batch_output_actuals CHECK ((actual_pieces IS NULL) = (waste_pieces IS NULL))
);

-- The bake days the deadline split has run for, so the jobs tick splits a day once.
CREATE TABLE batch_split (
    bake_day_id bigint PRIMARY KEY REFERENCES bake_day (id),
    split_at timestamptz NOT NULL
);

-- A mixed batch is locked: its version, code and origin never change, its status only
-- moves on to baked, and its lines and planned pieces stay as they were mixed.
CREATE FUNCTION refuse_locked_batch_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'batch % cannot be removed', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF NEW.id <> OLD.id OR NEW.bake_day_id <> OLD.bake_day_id
        OR NEW.recipe_version_id <> OLD.recipe_version_id
        OR NEW.code_date <> OLD.code_date OR NEW.mix_no <> OLD.mix_no
        OR NEW.origin <> OLD.origin OR NEW.created_at <> OLD.created_at
    THEN
        RAISE EXCEPTION 'batch % keeps its recipe version and code', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.status IN ('mixed', 'baked') AND (
        NEW.status NOT IN ('mixed', 'baked')
        OR NEW.mixed_at IS DISTINCT FROM OLD.mixed_at
        OR (OLD.status = 'baked' AND NEW.status <> 'baked')
    ) THEN
        RAISE EXCEPTION 'batch % is mixed', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER batch_locked BEFORE UPDATE OR DELETE ON batch
    FOR EACH ROW EXECUTE FUNCTION refuse_locked_batch_change();

CREATE FUNCTION batch_is_mixed(id bigint) RETURNS boolean
LANGUAGE sql AS $$
    SELECT status IN ('mixed', 'baked') FROM batch WHERE batch.id = $1 FOR SHARE
$$;

CREATE FUNCTION refuse_mixed_batch_line_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF (TG_OP <> 'INSERT' AND batch_is_mixed(OLD.batch_id))
        OR (TG_OP <> 'DELETE' AND batch_is_mixed(NEW.batch_id))
    THEN
        RAISE EXCEPTION 'batch is mixed; its lines are locked'
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER batch_line_locked BEFORE INSERT OR UPDATE OR DELETE ON batch_line
    FOR EACH ROW EXECUTE FUNCTION refuse_mixed_batch_line_change();

-- After mixing only the actual and waste pieces of an output may change.
CREATE FUNCTION refuse_mixed_batch_output_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.batch_id = OLD.batch_id AND NEW.product_id = OLD.product_id
        AND NEW.dough_g_per_piece = OLD.dough_g_per_piece
        AND NEW.planned_pieces = OLD.planned_pieces
    THEN
        RETURN NEW;
    END IF;
    IF (TG_OP <> 'INSERT' AND batch_is_mixed(OLD.batch_id))
        OR (TG_OP <> 'DELETE' AND batch_is_mixed(NEW.batch_id))
    THEN
        RAISE EXCEPTION 'batch is mixed; its planned pieces are locked'
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER batch_output_locked BEFORE INSERT OR UPDATE OR DELETE ON batch_output
    FOR EACH ROW EXECUTE FUNCTION refuse_mixed_batch_output_change();

-- Actual pieces are recorded only once the batch is mixed.
CREATE FUNCTION refuse_early_actuals() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.actual_pieces IS NOT NULL AND NOT batch_is_mixed(NEW.batch_id) THEN
        RAISE EXCEPTION 'batch is not mixed yet'
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER batch_output_actuals_after_mixing BEFORE INSERT OR UPDATE ON batch_output
    FOR EACH ROW EXECUTE FUNCTION refuse_early_actuals();

ALTER TABLE recipe_version ADD CONSTRAINT recipe_version_promoted_from_batch_fk
    FOREIGN KEY (promoted_from_batch_id) REFERENCES batch (id);
