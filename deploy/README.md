Docker Compose stack for local runs and the Gate 1 smoke test (spec "Architecture").
Needs Docker Compose 2.23.1 or newer, for `configs.content` in compose.yaml.

    make images                      # build and load the bvw-*:dev images
    nix develop -c cargo run -p cli -- dev-secrets deploy/secrets   # with an older secrets dir: mkdir -m 755 deploy/secrets/oidc
    make up                          # https://localhost:8443/ and https://idm.localhost:8443/
                                     # and Mailpit, which catches all email, at https://mail.localhost:8443/
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
    make kanidm-setup        # group bvw_staff and OAuth2 client bvw-staff; writes secrets/oidc/client-secret and kanidm-ca.pem
    make kanidm-person NAME=bas DISPLAY_NAME="Bas" MAIL=bas@example.org

`kanidm-person` adds the person to `bvw_staff`, the only group that may log in, and prints
their `sub` (their Kanidm UUID) and a reset link. Open the
link, add a passkey, then let the person in. The very first owner can come from the
environment instead; it only works while there is no owner yet:

    BVW_BOOTSTRAP_OWNER_SUB=<uuid> make up     # or BVW_BOOTSTRAP_OWNER_EMAIL=bas@example.org

The sub is safer: the email rule trusts Kanidm's `email_verified`.

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

Do not use `docker compose down --volumes` (`-v`): it also deletes Kanidm, the Caddy CA and
the backup volume. `make down` never does.

## Backups of the local stack

`make down` and `make up` keep the data; it lives in Docker volumes. To also survive
`down -v` or a broken volume, back it up to a directory on the Mac (ADR 0050):

    make backup              # database and Kanidm to ~/Backups/bvw, then thin out old runs
    make backup-schedule     # the same every hour; log in ~/Backups/bvw/backup.log
    make backup-unschedule   # stop the hourly backups
    make backup-list         # the runs, newest first

`BVW_BACKUP_DIR=/other/dir` puts the backups elsewhere (an absolute path). The backups keep
24 hourly, 14 daily and 8 weekly runs. The first backup copies `secrets/rustic-password` to
the backup directory; keep it, since the backups only open with it.

To put a run back (the server stops for a moment; the data from before stays as a second
database, `postgres_before_<number>`):

    make restore-local CONFIRM=yes                          # the newest run
    make restore-local SNAPSHOT=1795640400000000000 CONFIRM=yes   # a run from backup-list

After `docker compose down -v`, start an empty stack with `make up` first, then restore.
This restores the database only. Kanidm (staff and passkeys) comes back with
`make kanidm-setup`, `make kanidm-person` and `make staff-add` (new persons get a new
sub), or from the `kanidm` part of a run (ADR 0050).

The server sends email through Mailpit (`SMTP_HOST=mailpit`). Order emails need the
company's address and KvK number, which have no form yet; until they are set, payments
go through without an email and the server logs why. For the local stack:

    docker compose --file deploy/compose.yaml --project-name bvw exec postgres \
      psql -U postgres -c "UPDATE company SET address = 'Teststraat 1, 1234 AB Testdorp', kvk = '12345678'"
