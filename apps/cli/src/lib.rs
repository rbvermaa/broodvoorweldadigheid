#![forbid(unsafe_code)]

pub mod backup;
pub mod entrypoint;

use std::path::PathBuf;

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
    #[command(
        subcommand,
        about = "Container entrypoints: first-start setup, then exec the service"
    )]
    Entrypoint(entrypoint::Service),
}

#[derive(Debug, Subcommand)]
pub enum BackupCommand {
    #[command(about = "Back up the database (and uploads) to every repository")]
    Run(RunArgs),
    #[command(about = "Restore the latest backup into a scratch database and verify it")]
    RestoreTest(RestoreTestArgs),
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

pub async fn execute(command: BackupCommand, now: jiff::Timestamp) -> Result<(), backup::Error> {
    let run_id = &backup::run_id(now)?;
    match command {
        BackupCommand::Run(args) => {
            backup::run(&backup::BackupRun {
                repos: &args.repos,
                password_file: &args.password_file,
                database_url: &args.database_url,
                uploads: args.uploads.as_deref(),
                run_id,
            })
            .await?;
            println!(
                "backup run-{run_id} stored in {} repositories",
                args.repos.len()
            );
        }
        BackupCommand::RestoreTest(args) => {
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
                        for skipped in &verified.skipped {
                            eprintln!("warning: {repo}: {skipped}");
                        }
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
                });
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
}
