Docker Compose stack for local runs and the Gate 1 smoke test (spec "Architecture").

    make images                      # build and load the bvw-*:dev images
    nix develop -c cargo run -p cli -- dev-secrets deploy/secrets
    make up                          # https://localhost:8443/ and https://idm.localhost:8443/
    make smoke                       # throwaway stack: health, Kanidm, backup and restore test
    make down

Caddy's local CA is in the `caddy` volume at /data/caddy/pki/authorities/local/root.crt.
