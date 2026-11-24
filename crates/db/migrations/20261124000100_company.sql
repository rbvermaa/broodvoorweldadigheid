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
