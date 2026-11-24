-- Staff who log in through Kanidm (F13). oidc_sub is the ID token's `sub`; for Kanidm that
-- is the person's UUID, which survives renames.
CREATE TABLE staff_user (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    oidc_sub text NOT NULL UNIQUE CHECK (oidc_sub <> ''),
    name text NOT NULL,
    email text,
    role text NOT NULL CHECK (role IN ('owner', 'baker', 'counter')),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);
