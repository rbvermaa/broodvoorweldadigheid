# 0050 Local backups of the Compose stack

Status: accepted, 2026-11-26

Owner request: back up the local stack regularly, so the data survives more than
`docker compose down` (which already keeps the volumes): also `down -v` or a broken volume.

| Choice | Why |
|---|---|
| `make backup` runs the existing `cli backup run` into a rustic repository in `BVW_BACKUP_DIR` (default `~/Backups/bvw`), bind-mounted through `${BVW_BACKUP_DIR:-backup}:/backup`. Without the variable Compose keeps the named volume `backup`, as before | A host directory outside the checkout survives `down -v`, a broken Docker VM and `git clean` |
| `make backup` skips quietly (exit 0, one log line) when the stack's postgres is not running | The hourly job must not fill the log with errors while Docker Desktop is off |
| The rustic password is copied next to the repository on the first backup; later runs fail if `deploy/secrets/rustic-password` differs | The repository opens only with its own password; a new secrets directory would otherwise lock the backups away. Same machine, same trust as `deploy/secrets` |
| Kanidm writes online backups to `/data/backups` every 15 minutes, keeping 4 (`KANIDM_ONLINE_BACKUP_*`). The backup service mounts the `kanidm` volume read-only and stores that directory with each run (tag `kanidm`); while it is empty the run stores the database only and warns. The entrypoint creates the directory | `kanidmd database backup` is offline only; the online backup is Kanidm's supported way while it runs. The staff persons and passkeys are then at most 15 minutes older than the database |
| Kanidm is not restored by a make target. To restore it: stop kanidm, take the JSON from the run (`rustic restore <snapshot>`), then `kanidmd database restore` with the kanidm image. Or recreate it with `make kanidm-setup` and `make kanidm-person` | Passkeys rarely change, and a wrong restore locks staff out. The backup is there when it is needed |
| Retention: `cli backup prune --keep-hourly/--keep-daily/--keep-weekly`, counted like restic's `--keep-*` on whole runs (the newest run of each hour, day or ISO week in Europe/Amsterdam), then `rustic prune`. `make backup` uses 24 hourly, 14 daily and 8 weekly. Incomplete runs older than the newest complete run are forgotten; newer ones may still be running and stay | rustic's own `forget` groups snapshots by path, so the db, manifest and kanidm snapshots of one run could fall into different hours and be split up. The policy is local only: production keeps business records for 7 years (spec "Retention") and does not call prune |
| `cli backup restore --run <id or latest> --yes-overwrite` restores into a scratch database, checks it against the manifest and migrates it, then renames the current database to `<name>_before_<run id>` and the scratch database to the current name. Without `--yes-overwrite` it does nothing | Nothing is lost until the backup has proved complete, and the data from before stays until someone drops it |
| `make restore-local SNAPSHOT=<run> CONFIRM=yes` stops the server, runs the restore and starts the stack again; `make backup-list` shows the run ids | One command for the owner; the confirmation is asked twice (make and cli) |
| `make backup-schedule` writes a LaunchAgent from `deploy/launchd/` that runs `make backup` every hour (`StartInterval` 3600, not at load) with the installing shell's `PATH`, logging to `BVW_BACKUP_DIR/backup.log`; `make backup-unschedule` removes it | launchd starts jobs with a bare `PATH`, without nix or docker |
| `make smoke` sets `BVW_BACKUP_DIR=backup` and a Kanidm backup every minute, waits for a run with Kanidm in it, then prunes, runs the restore test, drops a table, restores and checks the row is back | The throwaway stack never writes to the owner's backups, and every new command runs against real images |

Not covered: uploads (the stack has none yet; `--uploads` exists for when it does) and the
Caddy CA (Caddy makes a new one; browsers then need to trust it again).
