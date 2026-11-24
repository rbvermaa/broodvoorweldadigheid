CREATE TABLE charity (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL CHECK (name <> ''),
    description text NOT NULL DEFAULT '',
    url text NOT NULL CHECK (url <> ''),
    logo_path text,
    iban text,
    anbi_rsin text,
    archived_at timestamptz
);

CREATE FUNCTION refuse_charity_delete() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'charities are archived, never deleted'
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER charity_no_delete BEFORE DELETE ON charity
    FOR EACH ROW EXECUTE FUNCTION refuse_charity_delete();

CREATE TRIGGER charity_no_truncate BEFORE TRUNCATE ON charity
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_charity_delete();
