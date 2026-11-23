# Gate 1: back up a seeded database with `cli backup run`, verify it with
# `cli backup restore-test`, then prove the restore test fails on a forged manifest,
# on a backup older than --max-age and on a repository with missing pack files.
{
  pkgs,
  cli,
  postgres,
}:
pkgs.runCommand "restore-check"
  {
    nativeBuildInputs = postgres.packages ++ [
      cli
      pkgs.rustic
      pkgs.jq
    ];
  }
  ''
    ${postgres.start}

    psql --quiet -v ON_ERROR_STOP=1 <<'SQL'
    CREATE TABLE loaves (id integer PRIMARY KEY, name text NOT NULL);
    INSERT INTO loaves VALUES (1, 'sourdough'), (2, 'spelt'), (3, 'rye');
    CREATE TABLE backup_sentinel (value text NOT NULL);
    INSERT INTO backup_sentinel VALUES ('restore-check-sentinel');
    SQL

    work="$TMPDIR/work"
    mkdir -p "$work/uploads"
    printf 'check-only-password\n' > "$work/password"
    printf 'not really a photo\n' > "$work/uploads/photo.txt"

    # Trust auth ignores the password; it is there to prove it never reaches a snapshot.
    dbPassword=check-only-db-secret
    sourceUrl="''${DATABASE_URL/postgres@/postgres:$dbPassword@}"
    [[ "$sourceUrl" == *"$dbPassword"* ]]
    cli backup run --repo "$work/local" --repo "$work/offsite" --password-file "$work/password" \
      --database-url "$sourceUrl" --uploads "$work/uploads"
    cli backup restore-test --repo "$work/local" --repo "$work/offsite" \
      --password-file "$work/password" --database-url "$DATABASE_URL" \
      | tee "$work/restore-test.out"
    grep -c "migrations applied" "$work/restore-test.out" | grep -qx 2

    for repo in local offsite; do
      snapshots=$(rustic --repository "$work/$repo" --password-file "$work/password" \
        --no-cache --no-progress snapshots --json)
      if [[ "$snapshots" == *"$dbPassword"* ]]; then
        echo "the database password is stored in a snapshot in $repo" >&2
        exit 1
      fi
    done

    expectFailure() {
      local what="$1" expected="$2" repo="$3"
      shift 3
      if cli backup restore-test --repo "$repo" --password-file "$work/password" \
        --database-url "$DATABASE_URL" "$@" 2> "$work/stderr"; then
        echo "restore-test passed despite $what" >&2
        exit 1
      fi
      cat "$work/stderr" >&2
      grep -qF -- "$expected" "$work/stderr" || {
        echo "restore-test failed for $what, but without '$expected'" >&2
        exit 1
      }
      echo "restore-test failed as expected: $what"
    }

    rustic=(rustic --repository "$work/local" --password-file "$work/password" \
      --no-cache --no-progress --log-level warn)
    manifests=$("''${rustic[@]}" snapshots --json --group-by "" --filter-tags manifest)
    id=$(jq -r '.[-1].id' <<< "$manifests")
    run=$(jq -r '.[-1].tags[] | select(startswith("run-"))' <<< "$manifests")
    "''${rustic[@]}" dump "$id:manifest.json" \
      | jq '.tables["public.loaves"].rows += 1' \
      | "''${rustic[@]}" backup --force --tag "manifest,$run" --stdin-filename manifest.json -
    expectFailure "a forged manifest" "public.loaves: expected 4 rows, restored 3" "$work/local"

    sleep 2
    expectFailure "an old backup" "old; the limit is 1s" "$work/offsite" --max-age 1s

    rm -r "$work/offsite/data"
    expectFailure "missing pack files" "Failed to open the file \`$work/offsite/data/" "$work/offsite"

    if psql --quiet --tuples-only -c "SELECT datname FROM pg_database" | grep -q restore_test_; then
      echo "restore-test left a scratch database behind" >&2
      exit 1
    fi

    ${postgres.stop}
    touch "$out"
  ''
