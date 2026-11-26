#![forbid(unsafe_code)]

pub mod backup;
pub mod dev_secrets;
pub mod entrypoint;
pub mod kanidm;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "cli", about = "Brood voor Weldadigheid maintenance tasks")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(subcommand, about = "Encrypted backups with rustic")]
    Backup(BackupCommand),
    #[command(about = "Write random secrets for deploy/compose.yaml into DIR; never overwrites")]
    DevSecrets { dir: PathBuf },
    #[command(
        subcommand,
        about = "Container entrypoints: first-start setup, then exec the service"
    )]
    Entrypoint(entrypoint::Service),
    #[command(about = "Apply the database migrations, then exit")]
    Migrate {
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: String,
    },
    #[command(
        subcommand,
        about = "Local Kanidm set-up for staff login; runs inside the kanidm container"
    )]
    Kanidm(kanidm::KanidmCommand),
    #[command(subcommand, about = "Staff who may log in to the back office (F13)")]
    Staff(StaffCommand),
}

#[derive(Debug, Subcommand)]
pub enum StaffCommand {
    #[command(about = "Let a person into the back office with a role; --sub is their Kanidm UUID")]
    Add {
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: String,
        #[arg(long, env = "STAFF_SUB", value_parser = uuid_sub)]
        sub: String,
        #[arg(long, env = "STAFF_NAME", value_parser = trimmed)]
        name: String,
        #[arg(long, env = "STAFF_EMAIL", help = "optional; empty means none")]
        email: Option<String>,
        #[arg(long, env = "STAFF_ROLE", help = "owner, baker or counter")]
        role: auth::Role,
    },
}

/// Kanidm's `sub` is the person's UUID; anything else is most likely their user name.
fn uuid_sub(raw: &str) -> Result<String, String> {
    auth::identity::canonical_uuid(raw.trim())
        .ok_or_else(|| format!("{raw:?} is not a UUID; use the person's Kanidm UUID"))
}

fn trimmed(raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.is_empty() {
        Err("must not be empty".to_string())
    } else {
        Ok(value.to_string())
    }
}

#[derive(Debug, Subcommand)]
pub enum BackupCommand {
    #[command(about = "Back up the database (and uploads) to every repository")]
    Run(RunArgs),
    #[command(about = "Restore the latest backup into a scratch database and verify it")]
    RestoreTest(RestoreTestArgs),
    #[command(about = "List the backup runs in a repository, newest first")]
    List(RepoArgs),
    #[command(about = "Forget the runs the --keep-* policy drops, then remove their data")]
    Prune(PruneArgs),
    #[command(
        about = "Replace the database with a backup run; stop the server first (make restore-local)"
    )]
    Restore(RestoreArgs),
}

#[derive(Debug, Args)]
pub struct RepoArgs {
    #[arg(long)]
    pub repo: String,
    #[arg(long)]
    pub password_file: PathBuf,
}

#[derive(Debug, Args)]
pub struct PruneArgs {
    #[command(flatten)]
    pub repo: RepoArgs,
    #[arg(
        long,
        default_value_t = 0,
        help = "newest run of each of the last N hours"
    )]
    pub keep_hourly: usize,
    #[arg(
        long,
        default_value_t = 0,
        help = "newest run of each of the last N days"
    )]
    pub keep_daily: usize,
    #[arg(
        long,
        default_value_t = 0,
        help = "newest run of each of the last N ISO weeks"
    )]
    pub keep_weekly: usize,
}

#[derive(Debug, Args)]
pub struct RestoreArgs {
    #[command(flatten)]
    pub repo: RepoArgs,
    #[arg(
        long,
        env = "DATABASE_URL",
        hide_env_values = true,
        help = "the database to replace; the login must be allowed to drop and create databases"
    )]
    pub database_url: String,
    #[arg(
        long,
        default_value = "latest",
        help = "run id from `backup list`, or latest"
    )]
    pub run: String,
    #[arg(
        long,
        help = "required: the current database is replaced (kept as <name>_before_<n>)"
    )]
    pub yes_overwrite: bool,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(
        long = "repo",
        required = true,
        help = "rustic repository; repeat for off-site copies"
    )]
    pub repos: Vec<String>,
    #[arg(long)]
    pub password_file: PathBuf,
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: String,
    #[arg(long)]
    pub uploads: Option<PathBuf>,
    #[arg(
        long,
        help = "directory with Kanidm's online backups; skipped with a warning while it is empty"
    )]
    pub kanidm: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RestoreTestArgs {
    #[arg(long = "repo", required = true)]
    pub repos: Vec<String>,
    #[arg(long)]
    pub password_file: PathBuf,
    #[arg(
        long,
        env = "DATABASE_URL",
        hide_env_values = true,
        help = "connection with the right to create and drop databases"
    )]
    pub database_url: String,
    #[arg(
        long,
        default_value = "26h",
        help = "fail if the newest backup is older"
    )]
    pub max_age: jiff::SignedDuration,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Backup(#[from] backup::Error),
    #[error(transparent)]
    DevSecrets(#[from] dev_secrets::Error),
    #[error(transparent)]
    Db(#[from] db::Error),
    #[error(transparent)]
    Kanidm(#[from] kanidm::Error),
    #[error(transparent)]
    Staff(#[from] auth::staff::Error),
}

pub async fn execute(command: Command, now: jiff::Timestamp) -> Result<ExitCode, Error> {
    match command {
        Command::Entrypoint(service) => return Ok(entrypoint::run(service)),
        Command::Migrate { database_url } => {
            let pool = db::connect(&database_url).await?;
            db::migrate(&pool).await?;
            pool.close().await;
            println!("migrations applied");
        }
        Command::Kanidm(command) => print!("{}", kanidm::execute(command)?),
        Command::Staff(StaffCommand::Add {
            database_url,
            sub,
            name,
            email,
            role,
        }) => {
            let pool = db::connect(&database_url).await?;
            let added = auth::staff::add(
                &pool,
                &auth::NewStaff {
                    oidc_sub: sub,
                    name,
                    email: email
                        .map(|email| email.trim().to_string())
                        .filter(|email| !email.is_empty()),
                    role,
                },
            )
            .await;
            pool.close().await;
            let added = added?;
            println!(
                "added {} as {} (id {})",
                added.name,
                added.role,
                added.id.get()
            );
        }
        Command::DevSecrets { dir } => {
            for path in dev_secrets::write(&dir, dev_secrets::random_password)? {
                println!("wrote {}", path.display());
            }
        }
        Command::Backup(command) => execute_backup(command, now).await?,
    }
    Ok(ExitCode::SUCCESS)
}

fn rustic_repo(args: &RepoArgs) -> backup::Repo<'_> {
    backup::Repo {
        location: &args.repo,
        password_file: &args.password_file,
    }
}

async fn execute_backup(command: BackupCommand, now: jiff::Timestamp) -> Result<(), Error> {
    let time_zone = domain::bakery_time_zone();
    match command {
        BackupCommand::List(args) => {
            let runs = backup::list(&rustic_repo(&args))?;
            if runs.is_empty() {
                println!("no backups in {}", args.repo);
            }
            for run in &runs {
                println!("{}", backup::retention::describe(run, &time_zone));
            }
        }
        BackupCommand::Prune(args) => {
            let policy = backup::Policy {
                hourly: args.keep_hourly,
                daily: args.keep_daily,
                weekly: args.keep_weekly,
            };
            let plan = backup::prune(&rustic_repo(&args.repo), policy, &time_zone, now)?;
            println!(
                "kept {} runs, forgot {} runs ({} snapshots) in {}",
                plan.keep.len(),
                plan.forget_runs.len(),
                plan.forget.len(),
                args.repo.repo
            );
        }
        BackupCommand::Restore(args) => {
            let run_id = backup::run_id(now)?;
            let restored = backup::restore(&backup::Restore {
                repo: &args.repo.repo,
                password_file: &args.repo.password_file,
                database_url: &args.database_url,
                run: Some(&args.run),
                scratch_database: &backup::scratch_database(&run_id),
                confirmed: args.yes_overwrite,
                now,
            })
            .await?;
            println!(
                "restored run {} into database {}: {} tables match the manifest, migrations applied\n\
                 the data from before is in database {}; drop it once you no longer need it",
                restored.run, restored.database, restored.tables, restored.previous
            );
        }
        BackupCommand::Run(args) => {
            let run_id = &backup::run_id(now)?;
            let kanidm = args.kanidm.as_deref().filter(|dir| {
                let present = backup::has_files(dir);
                if !present {
                    eprintln!(
                        "warning: no Kanidm backup in {} yet; this run has the database only",
                        dir.display()
                    );
                }
                present
            });
            backup::run(&backup::BackupRun {
                repos: &args.repos,
                password_file: &args.password_file,
                database_url: &args.database_url,
                uploads: args.uploads.as_deref(),
                kanidm,
                run_id,
            })
            .await?;
            println!(
                "backup run-{run_id} stored in {} repositories",
                args.repos.len()
            );
        }
        BackupCommand::RestoreTest(args) => {
            let run_id = &backup::run_id(now)?;
            let scratch_database = backup::scratch_database(run_id);
            let mut failed = 0;
            for repo in &args.repos {
                let result = backup::restore_test(&backup::RestoreTest {
                    repo,
                    password_file: &args.password_file,
                    admin_url: &args.database_url,
                    scratch_database: &scratch_database,
                    max_age: args.max_age,
                    now,
                })
                .await;
                match result {
                    Ok(verified) => {
                        println!(
                            "restore test passed for {repo}: run {} ({:#} old), {} tables match the manifest, migrations applied",
                            verified.run, verified.age, verified.tables
                        );
                    }
                    Err(error) => {
                        failed += 1;
                        eprintln!("error: restore test failed for {repo}: {error}");
                    }
                }
            }
            if failed > 0 {
                return Err(backup::Error::RestoreTestsFailed {
                    failed,
                    total: args.repos.len(),
                }
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_backup_run_with_several_repos() {
        let cli = Cli::try_parse_from([
            "cli",
            "backup",
            "run",
            "--repo",
            "/backup/local",
            "--repo",
            "opendal:sftp",
            "--password-file",
            "/run/secrets/rustic",
            "--database-url",
            "postgres://db/bakflow",
            "--uploads",
            "/data/uploads",
        ])
        .expect("valid arguments");
        let Command::Backup(BackupCommand::Run(args)) = cli.command else {
            panic!("expected backup run, got {cli:?}");
        };
        assert_eq!(args.repos, ["/backup/local", "opendal:sftp"]);
        assert_eq!(args.password_file, PathBuf::from("/run/secrets/rustic"));
        assert_eq!(args.database_url, "postgres://db/bakflow");
        assert_eq!(args.uploads, Some(PathBuf::from("/data/uploads")));
    }

    #[test]
    fn parses_restore_test() {
        let cli = Cli::try_parse_from([
            "cli",
            "backup",
            "restore-test",
            "--repo",
            "/backup/local",
            "--password-file",
            "/pw",
            "--database-url",
            "postgres://db/postgres",
        ])
        .expect("valid arguments");
        let Command::Backup(BackupCommand::RestoreTest(args)) = cli.command else {
            panic!("expected restore-test, got {cli:?}");
        };
        assert_eq!(args.repos, ["/backup/local"]);
        assert_eq!(args.max_age, jiff::SignedDuration::from_hours(26));
    }

    #[test]
    fn parses_restore_test_max_age() {
        let cli = Cli::try_parse_from([
            "cli",
            "backup",
            "restore-test",
            "--repo",
            "/backup/local",
            "--password-file",
            "/pw",
            "--database-url",
            "postgres://db/postgres",
            "--max-age",
            "2h 30m",
        ])
        .expect("valid arguments");
        let Command::Backup(BackupCommand::RestoreTest(args)) = cli.command else {
            panic!("expected restore-test, got {cli:?}");
        };
        assert_eq!(args.max_age, jiff::SignedDuration::from_mins(150));
    }

    #[test]
    fn parses_dev_secrets() {
        let cli =
            Cli::try_parse_from(["cli", "dev-secrets", "deploy/secrets"]).expect("valid arguments");
        let Command::DevSecrets { dir } = cli.command else {
            panic!("expected dev-secrets, got {cli:?}");
        };
        assert_eq!(dir, PathBuf::from("deploy/secrets"));
    }

    #[test]
    fn parses_staff_add_with_a_role() {
        let cli = Cli::try_parse_from([
            "cli",
            "staff",
            "add",
            "--database-url",
            "postgres://db/bakflow",
            "--sub",
            "0D2F1F9E-1C2B-4E3A-9F00-123456789ABC",
            "--name",
            "Bas",
            "--role",
            "baker",
        ])
        .expect("valid arguments");
        let Command::Staff(StaffCommand::Add {
            sub, role, email, ..
        }) = cli.command
        else {
            panic!("expected staff add, got {cli:?}");
        };
        assert_eq!(sub, "0d2f1f9e-1c2b-4e3a-9f00-123456789abc");
        assert_eq!(role, auth::Role::Baker);
        assert_eq!(email, None);

        let error = Cli::try_parse_from([
            "cli",
            "staff",
            "add",
            "--database-url",
            "postgres://db",
            "--sub",
            "0d2f1f9e-1c2b-4e3a-9f00-123456789abc",
            "--name",
            "X",
            "--role",
            "admin",
        ])
        .expect_err("unknown role");
        assert!(
            error.to_string().contains("owner, baker or counter"),
            "{error}"
        );
    }

    #[test]
    fn staff_add_refuses_a_sub_that_is_no_uuid_and_a_blank_name() {
        let parse = |sub: &str, name: &str| {
            Cli::try_parse_from([
                "cli",
                "staff",
                "add",
                "--database-url",
                "postgres://db",
                "--sub",
                sub,
                "--name",
                name,
                "--role",
                "baker",
            ])
        };
        for sub in [
            "bas",
            "",
            "0d2f1f9e-1c2b-4e3a-9f00-123456789ab",
            "0d2f1f9e1c2b4e3a9f00123456789abc",
        ] {
            let error = parse(sub, "Bas").expect_err(sub);
            assert!(error.to_string().contains("not a UUID"), "{error}");
        }
        let error = parse("0d2f1f9e-1c2b-4e3a-9f00-123456789abc", "   ").expect_err("blank");
        assert!(error.to_string().contains("must not be empty"), "{error}");
        let cli =
            parse(" 0d2f1f9e-1c2b-4e3a-9f00-123456789abc ", " Bas van 't Hof ").expect("valid");
        let Command::Staff(StaffCommand::Add { sub, name, .. }) = cli.command else {
            panic!("expected staff add");
        };
        assert_eq!(sub, "0d2f1f9e-1c2b-4e3a-9f00-123456789abc");
        assert_eq!(name, "Bas van 't Hof");
        let cli = Cli::try_parse_from([
            "cli",
            "staff",
            "add",
            "--database-url",
            "postgres://db",
            "--sub",
            "0d2f1f9e-1c2b-4e3a-9f00-123456789abc",
            "--name",
            "Bas",
            "--role",
            "baker",
            "--email",
            "",
        ])
        .expect("an empty email is allowed");
        assert!(matches!(
            cli.command,
            Command::Staff(StaffCommand::Add { .. })
        ));
    }

    #[test]
    fn parses_kanidm_oauth2_client() {
        let cli = Cli::try_parse_from([
            "cli",
            "kanidm",
            "oauth2-client",
            "--landing-url",
            "https://localhost:8443/staff",
            "--redirect-url",
            "https://localhost:8443/staff/oidc/callback",
        ])
        .expect("valid arguments");
        let Command::Kanidm(kanidm::KanidmCommand::Oauth2Client { url, client_id, .. }) =
            cli.command
        else {
            panic!("expected kanidm oauth2-client, got {cli:?}");
        };
        assert_eq!(url, kanidm::DEFAULT_URL);
        assert_eq!(client_id, "bvw-staff");
    }

    #[test]
    fn backup_run_requires_a_repo() {
        let error = Cli::try_parse_from([
            "cli",
            "backup",
            "run",
            "--password-file",
            "/pw",
            "--database-url",
            "postgres://db/bakflow",
        ])
        .expect_err("missing --repo");
        assert!(error.to_string().contains("--repo"), "{error}");
    }

    #[test]
    fn parses_backup_run_with_kanidm() {
        let cli = Cli::try_parse_from([
            "cli",
            "backup",
            "run",
            "--repo=/backup/repo",
            "--password-file=/pw",
            "--database-url=postgres://db/postgres",
            "--kanidm=/kanidm/backups",
        ])
        .expect("valid arguments");
        let Command::Backup(BackupCommand::Run(args)) = cli.command else {
            panic!("expected backup run, got {cli:?}");
        };
        assert_eq!(args.kanidm, Some(PathBuf::from("/kanidm/backups")));
        assert_eq!(args.uploads, None);
    }

    #[test]
    fn parses_backup_prune_with_zero_as_the_default() {
        let cli = Cli::try_parse_from([
            "cli",
            "backup",
            "prune",
            "--repo=/backup/repo",
            "--password-file=/pw",
            "--keep-hourly=24",
            "--keep-weekly=8",
        ])
        .expect("valid arguments");
        let Command::Backup(BackupCommand::Prune(args)) = cli.command else {
            panic!("expected backup prune, got {cli:?}");
        };
        assert_eq!(args.repo.repo, "/backup/repo");
        assert_eq!(
            (args.keep_hourly, args.keep_daily, args.keep_weekly),
            (24, 0, 8)
        );
    }

    #[test]
    fn backup_restore_defaults_to_latest_and_no_confirmation() {
        let parse = |extra: &[&str]| {
            let mut args = vec![
                "cli",
                "backup",
                "restore",
                "--repo=/backup/repo",
                "--password-file=/pw",
                "--database-url=postgres://db/postgres",
            ];
            args.extend(extra);
            let cli = Cli::try_parse_from(args).expect("valid arguments");
            let Command::Backup(BackupCommand::Restore(args)) = cli.command else {
                panic!("expected backup restore, got {cli:?}");
            };
            args
        };
        let plain = parse(&[]);
        assert_eq!(plain.run, "latest");
        assert!(!plain.yes_overwrite);
        let confirmed = parse(&["--run=1795640400000000000", "--yes-overwrite"]);
        assert_eq!(confirmed.run, "1795640400000000000");
        assert!(confirmed.yes_overwrite);
    }

    #[tokio::test]
    async fn backup_restore_refuses_without_yes_overwrite_before_touching_anything() {
        let cli = Cli::try_parse_from([
            "cli",
            "backup",
            "restore",
            "--repo=/does/not/exist",
            "--password-file=/does/not/exist",
            "--database-url=postgres://nowhere.invalid/postgres",
        ])
        .expect("valid arguments");
        let error = execute(cli.command, jiff::Timestamp::UNIX_EPOCH)
            .await
            .expect_err("not confirmed");
        assert_eq!(
            error.to_string(),
            "restoring replaces the whole database with the backup; pass --yes-overwrite to go ahead"
        );
    }
}
