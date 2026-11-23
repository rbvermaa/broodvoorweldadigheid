Docker Compose stack for local runs and the Gate 1 smoke test (spec "Architecture").
Needs Docker Compose 2.23.1 or newer, for `configs.content` in compose.yaml.

    make images                      # build and load the bvw-*:dev images
    nix develop -c cargo run -p cli -- dev-secrets deploy/secrets
    make up                          # https://localhost:8443/ and https://idm.localhost:8443/
    make smoke                       # throwaway stack on 18080/18443: health, Kanidm, redirects, backup and restore test
    make down

Ports bind to 127.0.0.1; set `BVW_BIND_ADDRESS=0.0.0.0` to expose them. `make up` uses the
subnet 10.89.0.0/24 and `make smoke` 10.89.1.0/24, so both can run at once; `BVW_SUBNET` and
`BVW_CADDY_IP` change them if they clash with another network.

Caddy's local CA is in the `caddy` volume at /data/caddy/pki/authorities/local/root.crt.

If postgres stops with "PG_VERSION but no .bvw-init-done and no global/pg_filenode.map", an
earlier first start was interrupted or the volume holds something else. Remove only the
postgres volume to start fresh (`bvw` is the `make up` project name):

    docker compose --file deploy/compose.yaml --project-name bvw stop postgres
    docker compose --file deploy/compose.yaml --project-name bvw rm -f postgres
    docker volume rm bvw_pgdata

Do not use `docker compose down --volumes`: it also deletes Kanidm, the Caddy CA and the
backup repository.
