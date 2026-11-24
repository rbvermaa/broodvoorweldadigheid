Docker Compose stack for local runs and the Gate 1 smoke test (spec "Architecture").
Needs Docker Compose 2.23.1 or newer, for `configs.content` in compose.yaml.

    make images                      # build and load the bvw-*:dev images
    nix develop -c cargo run -p cli -- dev-secrets deploy/secrets   # with an older secrets dir: mkdir -m 755 deploy/secrets/oidc
    make up                          # https://localhost:8443/ and https://idm.localhost:8443/
    make smoke                       # throwaway stack on 18080/18443: health, Kanidm, redirects, session, backup and restore test
    make down

Ports bind to 127.0.0.1; set `BVW_BIND_ADDRESS=0.0.0.0` to expose them. `make up` uses the
subnet 10.89.0.0/24 and `make smoke` 10.89.1.0/24, so both can run at once; `BVW_SUBNET` and
`BVW_CADDY_IP` change them if they clash with another network.

Caddy's local CA is in the `caddy` volume at /data/caddy/pki/authorities/local/root.crt.

## Staff login (F13)

Staff log in at https://localhost:8443/staff through Kanidm with a passkey. Passkeys need a
real browser, so these steps are manual. Run them once per fresh `kanidm` volume:

    make up
    make kanidm-setup        # OAuth2 client bvw-staff; writes secrets/oidc/client-secret and kanidm-ca.pem
    make kanidm-person NAME=bas DISPLAY_NAME="Bas" MAIL=bas@example.org

`kanidm-person` prints the person's `sub` (their Kanidm UUID) and a reset link. Open the
link, add a passkey, then let the person in. The very first owner can come from the
environment instead; it only works while there is no owner yet:

    BVW_BOOTSTRAP_OWNER_EMAIL=bas@example.org make up     # or BVW_BOOTSTRAP_OWNER_SUB=<uuid>

Everyone after that is added with a role (owner, baker or counter):

    make staff-add SUB=<uuid> NAME="Tineke" ROLE=counter

Then open https://localhost:8443/staff, log in with the passkey, and you land on the staff
page. Before `make kanidm-setup`, `/staff/login` answers 503. Trust Caddy's CA (above) in
the browser, or accept the warnings for localhost and idm.localhost.

`make kanidm-setup` resets the `idm_admin` password each time it runs; nothing else uses it
locally. Kanidm has the network alias `idm.localhost` and listens on the published HTTPS port
number, so the server reaches the same issuer URL as the browser (ADR 0010).

If postgres stops with "PG_VERSION but no .bvw-init-done and no postmaster.opts", an
earlier first start was interrupted or the volume holds something else. Remove only the
postgres volume to start fresh (`bvw` is the `make up` project name):

    docker compose --file deploy/compose.yaml --project-name bvw stop postgres
    docker compose --file deploy/compose.yaml --project-name bvw rm -f postgres
    docker volume rm bvw_pgdata

Do not use `docker compose down --volumes`: it also deletes Kanidm, the Caddy CA and the
backup repository.
