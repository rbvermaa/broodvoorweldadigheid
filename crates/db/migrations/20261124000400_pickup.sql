CREATE TABLE pickup_location (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL CHECK (name <> ''),
    address text NOT NULL CHECK (address <> ''),
    notes text NOT NULL DEFAULT ''
);

-- weekday is ISO 8601: 1 = Monday ... 7 = Sunday.
CREATE TABLE pickup_window (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    location_id bigint NOT NULL REFERENCES pickup_location (id),
    weekday smallint NOT NULL CHECK (weekday BETWEEN 1 AND 7),
    starts time NOT NULL,
    ends time NOT NULL,
    default_deadline_days_before smallint NOT NULL CHECK (default_deadline_days_before BETWEEN 0 AND 28),
    default_deadline_time time NOT NULL,
    active boolean NOT NULL DEFAULT true,
    CHECK (starts < ends)
);

CREATE INDEX pickup_window_location_idx ON pickup_window (location_id);

WITH de_proef AS (
    INSERT INTO pickup_location (name, address)
    VALUES ('De Proef', 'Majoor van Swietenlaan 15, 8382 CE Frederiksoord')
    RETURNING id
)
INSERT INTO pickup_window
    (location_id, weekday, starts, ends, default_deadline_days_before, default_deadline_time)
SELECT id, 6, '11:00', '12:00', 2, '20:00' FROM de_proef;
