# Throwaway PostgreSQL for checks: unix socket only (no TCP, so it works in the Linux
# sandbox), trust auth, data in the build directory. Exports PGHOST and DATABASE_URL.
#
#   postgres = import ./nix/postgres.nix { postgresql = pkgs.postgresql_17; };
#   nativeBuildInputs = postgres.packages;
#   preCheck = postgres.start;
#   postCheck = postgres.stop;
{ postgresql }:
{
  packages = [ postgresql ];

  start = ''
    export PGDATA="$TMPDIR/pgdata"
    export PGHOST="$TMPDIR/pgsock"
    export PGUSER=postgres
    mkdir -p "$PGHOST"
    ${postgresql}/bin/initdb --auth=trust --username=postgres --no-instructions \
      --encoding=UTF8 --locale=C >/dev/null
    ${postgresql}/bin/pg_ctl --wait --log="$TMPDIR/postgres.log" \
      -o "-c listen_addresses= -c unix_socket_directories=$PGHOST -c fsync=off -c max_connections=200" \
      start
    stopPostgres() { ${postgresql}/bin/pg_ctl --wait --mode=immediate stop >/dev/null 2>&1 || true; }
    failureHooks+=(stopPostgres)
    export DATABASE_URL="postgres://postgres@localhost/postgres?host=$PGHOST"
  '';

  stop = ''
    ${postgresql}/bin/pg_ctl --wait --mode=fast stop
  '';
}
