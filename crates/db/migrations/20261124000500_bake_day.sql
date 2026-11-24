CREATE TABLE bake_day (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    date date NOT NULL,
    online_ordering boolean NOT NULL DEFAULT true,
    charity_id bigint REFERENCES charity (id),
    menu_publish_at timestamptz,
    opens_at timestamptz,
    cutoff_at timestamptz,
    bake_starts_at timestamptz NOT NULL,
    status text NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'open', 'closed', 'cancelled', 'done')),
    cancel_message text,
    CONSTRAINT bake_day_online_needs_cutoff CHECK (NOT online_ordering OR cutoff_at IS NOT NULL),
    CONSTRAINT bake_day_production_only_never_opens CHECK (
        online_ordering OR (opens_at IS NULL AND status <> 'open')
    ),
    CONSTRAINT bake_day_opens_before_cutoff CHECK (opens_at < cutoff_at),
    CONSTRAINT bake_day_cancel_message CHECK ((status = 'cancelled') = (cancel_message IS NOT NULL))
);

CREATE INDEX bake_day_date_idx ON bake_day (date);

CREATE TABLE pickup_slot (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    bake_day_id bigint NOT NULL REFERENCES bake_day (id),
    location_id bigint NOT NULL REFERENCES pickup_location (id),
    starts_at timestamptz NOT NULL,
    ends_at timestamptz NOT NULL,
    max_orders integer CHECK (max_orders > 0),
    CHECK (starts_at < ends_at)
);

CREATE INDEX pickup_slot_bake_day_idx ON pickup_slot (bake_day_id);

-- Checkout and extra production lock this row (SELECT ... FOR UPDATE) while they check
-- the pool's remaining units (ADR 0007).
CREATE TABLE capacity_pool (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    bake_day_id bigint NOT NULL REFERENCES bake_day (id),
    name text NOT NULL CHECK (name <> ''),
    max_units integer NOT NULL CHECK (max_units >= 0),
    UNIQUE (bake_day_id, name),
    UNIQUE (id, bake_day_id)
);

CREATE TABLE bake_day_product (
    bake_day_id bigint NOT NULL REFERENCES bake_day (id),
    product_id bigint NOT NULL REFERENCES product (id),
    max_qty integer CHECK (max_qty >= 0),
    capacity_pool_id bigint,
    units_per_item integer NOT NULL DEFAULT 1 CHECK (units_per_item > 0),
    PRIMARY KEY (bake_day_id, product_id),
    FOREIGN KEY (capacity_pool_id, bake_day_id) REFERENCES capacity_pool (id, bake_day_id)
);

CREATE TABLE production_extra (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    bake_day_id bigint NOT NULL REFERENCES bake_day (id),
    product_id bigint NOT NULL REFERENCES product (id),
    qty integer NOT NULL CHECK (qty > 0),
    purpose text NOT NULL CHECK (purpose IN ('freezer', 'sumup', 'samples', 'other')),
    note text
);

CREATE INDEX production_extra_bake_day_idx ON production_extra (bake_day_id, product_id);
