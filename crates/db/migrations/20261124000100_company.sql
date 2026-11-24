CREATE TABLE company (
    id integer PRIMARY KEY CHECK (id = 1),
    name text NOT NULL CHECK (name <> ''),
    kvk text,
    btw_id text,
    address text,
    default_lead_time_hours integer NOT NULL CHECK (default_lead_time_hours BETWEEN 0 AND 168)
);

INSERT INTO company (id, name, default_lead_time_hours)
VALUES (1, 'Brood voor Weldadigheid', 36);

CREATE FUNCTION refuse_company_delete() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'the company settings row cannot be removed'
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER company_no_delete BEFORE DELETE ON company
    FOR EACH ROW EXECUTE FUNCTION refuse_company_delete();

CREATE TRIGGER company_no_truncate BEFORE TRUNCATE ON company
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_company_delete();
