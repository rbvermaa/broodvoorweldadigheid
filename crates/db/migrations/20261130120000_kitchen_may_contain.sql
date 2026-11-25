-- The kitchen-wide "may contain traces of" list (F10, ADR 0038). A change to the set of
-- allergens clears its approval, whoever writes it; the same set in another order or with
-- duplicates is no change. product.may_contain is no longer read or written; it goes
-- in a later release.
ALTER TABLE company
    ADD COLUMN may_contain text[] NOT NULL DEFAULT '{}'
        CONSTRAINT company_may_contain_check CHECK (is_allergen_list(may_contain)),
    ADD COLUMN may_contain_approved_by bigint
        CONSTRAINT company_may_contain_approved_by_fkey REFERENCES staff_user (id),
    ADD COLUMN may_contain_approved_at timestamptz,
    ADD CONSTRAINT company_may_contain_approval_complete CHECK (
        (may_contain_approved_by IS NULL) = (may_contain_approved_at IS NULL)
    );

CREATE FUNCTION clear_may_contain_approval() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NOT (NEW.may_contain @> OLD.may_contain AND NEW.may_contain <@ OLD.may_contain) THEN
        NEW.may_contain_approved_by := NULL;
        NEW.may_contain_approved_at := NULL;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER company_clear_may_contain_approval BEFORE UPDATE ON company
    FOR EACH ROW EXECUTE FUNCTION clear_may_contain_approval();
