-- Staff who sign in to the back office (F13). oidc_sub is the identity provider's subject;
-- for Kanidm that is the person's UUID, which survives renames.
CREATE TABLE staff_user (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    oidc_sub text NOT NULL UNIQUE CHECK (oidc_sub <> ''),
    name text NOT NULL CHECK (name <> ''),
    email text,
    role text NOT NULL CHECK (role IN ('owner', 'baker', 'counter')),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- One row per signed-in browser; the session holds only the id. Deleting the row signs
-- that browser out, even if a request still in flight saves its session again.
CREATE TABLE staff_login (
    id text PRIMARY KEY
        DEFAULT replace(gen_random_uuid()::text || gen_random_uuid()::text, '-', ''),
    staff_id bigint NOT NULL REFERENCES staff_user (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL
);

CREATE INDEX staff_login_staff_idx ON staff_login (staff_id);

-- Promised in ADR 0006.
ALTER TABLE product ADD CONSTRAINT product_allergens_approved_by_fkey
    FOREIGN KEY (allergens_approved_by) REFERENCES staff_user (id);

-- The bootstrap owner only gets in while there is no owner, so the last active owner may
-- not be deleted, demoted or deactivated. The advisory lock is auth's BOOTSTRAP_LOCK; it
-- keeps two transactions from each removing one of the last two owners.
CREATE FUNCTION keep_last_owner() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.role = 'owner' AND OLD.active
        AND (TG_OP = 'DELETE' OR NEW.role <> 'owner' OR NOT NEW.active)
    THEN
        PERFORM pg_advisory_xact_lock(7094989514744491634);
        IF NOT EXISTS (
            SELECT 1 FROM staff_user WHERE role = 'owner' AND active AND id <> OLD.id
        ) THEN
            RAISE EXCEPTION 'staff user % is the last active owner', OLD.id
                USING ERRCODE = 'check_violation', CONSTRAINT = 'staff_user_last_owner';
        END IF;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER staff_user_keep_last_owner BEFORE UPDATE OR DELETE ON staff_user
    FOR EACH ROW EXECUTE FUNCTION keep_last_owner();
