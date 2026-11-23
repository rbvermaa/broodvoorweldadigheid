# Throwaway PostgreSQL for checks: unix socket only (no TCP, so it needs no network
# in the Linux or macOS sandbox), trust auth, data in the build directory. `start`
# exports PGDATA, PGHOST, PGUSER and DATABASE_URL and stops the server on failure.
#
#   postgres = import ./nix/postgres.nix { postgresql = pkgs.postgresql_17; };
#   nativeBuildInputs = postgres.packages;
#   preCheck = postgres.start;    # preBuild for cargoLlvmCov
#   postCheck = postgres.stop;
{ postgresql }:
{
  packages = [ postgresql ];

  start = ''
    stopPostgres() {
      ${postgresql}/bin/pg_ctl --pgdata="$PGDATA" --wait --mode=fast stop || true
      if [ -n "''${pgSocketFallback:-}" ]; then rm -rf "$pgSocketFallback"; fi
    }
    failureHooks+=(stopPostgres)

    export PGDATA="$TMPDIR/pgdata"
    export PGUSER=postgres
    # Unix socket paths are limited to 104 bytes on macOS.
    PGHOST="$TMPDIR/pgs"
    if [ "''${#PGHOST}" -gt 80 ]; then
      pgSocketFallback="$(mktemp -d /tmp/pgs.XXXXXX)"
      PGHOST="$pgSocketFallback"
    fi
    export PGHOST
    mkdir -p "$PGHOST"

    ${postgresql}/bin/initdb --auth=trust --username=postgres --no-instructions \
      --encoding=UTF8 --locale=C >/dev/null
    ${postgresql}/bin/pg_ctl --wait --log="$TMPDIR/postgres.log" \
      -o "-c listen_addresses= -c unix_socket_directories=$PGHOST -c fsync=off -c max_connections=200" \
      start
    export DATABASE_URL="postgres://postgres@localhost/postgres?host=$PGHOST"
  '';

  stop = ''
    stopPostgres
  '';
}
