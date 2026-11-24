CREATE TABLE charity (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL CHECK (name <> ''),
    description text NOT NULL DEFAULT '',
    url text,
    logo_path text,
    iban text,
    anbi_rsin text,
    archived_at timestamptz
);

CREATE FUNCTION refuse_charity_delete() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'charities are archived, never deleted (charity %)', OLD.id
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER charity_no_delete BEFORE DELETE ON charity
    FOR EACH ROW EXECUTE FUNCTION refuse_charity_delete();
