# Starts a throwaway PostgreSQL around a crane check's test phase and exports
# DATABASE_URL for #[sqlx::test]. It listens on a unix socket only, so it needs
# no network and works in the Linux and macOS build sandboxes.
{ postgresql }:
{
  nativeCheckInputs = [ postgresql ];

  preCheck = ''
    stopPostgres() {
      pg_ctl --pgdata="$PGDATA" --wait --mode=fast stop || true
    }
    failureHooks+=(stopPostgres)

    export PGDATA="$TMPDIR/pgdata"
    # Unix socket paths are limited to 104 bytes on macOS.
    PGHOST="$TMPDIR/pgs"
    if [ "''${#PGHOST}" -gt 80 ]; then
      PGHOST="$(mktemp -d /tmp/pgs.XXXXXX)"
    fi
    export PGHOST
    mkdir -p "$PGHOST"

    initdb --auth=trust --username=postgres --no-instructions --encoding=UTF8 --locale=C >/dev/null
    pg_ctl --wait --log="$TMPDIR/postgres.log" \
      -o "-c listen_addresses= -c unix_socket_directories=$PGHOST -c fsync=off -c max_connections=200" \
      start
    export DATABASE_URL="postgres://postgres@localhost/postgres?host=$PGHOST"
  '';

  postCheck = ''
    stopPostgres
  '';
}
