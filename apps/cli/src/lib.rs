#![forbid(unsafe_code)]

pub mod backup;
pub mod dev_secrets;

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
    #[command(about = "Write random secrets for deploy/compose.yaml into DIR; never overwrites")]
    DevSecrets { dir: PathBuf },
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
    #[arg(long, env = "DATABASE_URL")]
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
        help = "connection with the right to create and drop databases"
    )]
    pub database_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Backup(#[from] backup::Error),
    #[error(transparent)]
    DevSecrets(#[from] dev_secrets::Error),
}

pub async fn execute(cli: Cli, run_id: &str) -> Result<(), Error> {
    match cli.command {
        Command::DevSecrets { dir } => {
            for path in dev_secrets::write(&dir, dev_secrets::random_password)? {
                println!("wrote {}", path.display());
            }
        }
        Command::Backup(BackupCommand::Run(args)) => {
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
        Command::Backup(BackupCommand::RestoreTest(args)) => {
            let scratch_database = format!("restore_test_{run_id}");
            for repo in &args.repos {
                let tables = backup::restore_test(&backup::RestoreTest {
                    repo,
                    password_file: &args.password_file,
                    admin_url: &args.database_url,
                    scratch_database: &scratch_database,
                })
                .await?;
                println!(
                    "restore test passed for {repo}: {tables} tables match the manifest, migrations applied"
                );
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
