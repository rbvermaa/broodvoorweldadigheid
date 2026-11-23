use std::ffi::OsString;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

pub const DEFAULT_PGDATA: &str = "/var/lib/postgresql/data";
pub const DEFAULT_KANIDM_TLS_CHAIN: &str = "/data/chain.pem";
pub const DEFAULT_KANIDM_TLS_KEY: &str = "/data/key.pem";
/// Written to PGDATA only after initdb and the pg_hba.conf update both succeed.
pub const INIT_DONE_MARKER: &str = ".bvw-init-done";
const REMOTE_SCRAM: [&str; 5] = ["host", "all", "all", "samenet", "scram-sha-256"];

#[derive(Debug, Clone, Copy, clap::Subcommand)]
pub enum Service {
    /// Initialise PGDATA on first start, then exec postgres
    Postgres,
    /// Generate a self-signed TLS cert on first start, then exec kanidmd server
    Kanidm,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Step {
    pub program: &'static str,
    pub args: Vec<OsString>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub init: Option<Step>,
    /// PGDATA to pass to [`finish_init`] after `init`, if any, succeeds.
    pub finish: Option<PathBuf>,
    pub exec: Step,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("password file {0} does not exist")]
    NoPasswordFile(PathBuf),
    #[error(
        "{0} has PG_VERSION but no {INIT_DONE_MARKER} and was never started (no postmaster.opts). \
         Either an earlier initdb was interrupted, or the volume is not a PostgreSQL data directory \
         this image created. To start fresh, remove only the postgres volume: \
         `docker compose stop postgres && docker compose rm -f postgres && docker volume rm <project>_pgdata`"
    )]
    InitUnfinished(PathBuf),
}

pub fn postgres_plan(lookup: impl Fn(&str) -> Option<String>) -> Result<Plan, PlanError> {
    let pgdata = PathBuf::from(non_empty(&lookup, "PGDATA").unwrap_or(DEFAULT_PGDATA.into()));
    let init = if pgdata.join(INIT_DONE_MARKER).exists() {
        None
    } else if pgdata.join("PG_VERSION").exists() {
        if !pgdata.join("postmaster.opts").exists() {
            return Err(PlanError::InitUnfinished(pgdata));
        }
        return Ok(Plan {
            init: None,
            finish: Some(pgdata.clone()),
            exec: postgres_exec(pgdata),
        });
    } else {
        let user =
            non_empty(&lookup, "POSTGRES_USER").ok_or(PlanError::Missing("POSTGRES_USER"))?;
        let password_file = PathBuf::from(
            non_empty(&lookup, "POSTGRES_PASSWORD_FILE")
                .ok_or(PlanError::Missing("POSTGRES_PASSWORD_FILE"))?,
        );
        if !password_file.is_file() {
            return Err(PlanError::NoPasswordFile(password_file));
        }
        Some(Step {
            program: "initdb",
            args: vec![
                flag("--pgdata=", &pgdata),
                format!("--username={user}").into(),
                flag("--pwfile=", &password_file),
                "--auth-local=trust".into(),
                "--auth-host=scram-sha-256".into(),
                "--encoding=UTF8".into(),
                "--locale=C".into(),
                "--no-instructions".into(),
            ],
        })
    };
    Ok(Plan {
        finish: init.as_ref().map(|_| pgdata.clone()),
        init,
        exec: postgres_exec(pgdata),
    })
}

fn postgres_exec(pgdata: PathBuf) -> Step {
    Step {
        program: "postgres",
        args: vec![
            "-D".into(),
            pgdata.into(),
            "-c".into(),
            "listen_addresses=*".into(),
        ],
    }
}

pub fn kanidm_plan(lookup: impl Fn(&str) -> Option<String>) -> Plan {
    let chain = non_empty(&lookup, "KANIDM_TLS_CHAIN").unwrap_or(DEFAULT_KANIDM_TLS_CHAIN.into());
    let key = non_empty(&lookup, "KANIDM_TLS_KEY").unwrap_or(DEFAULT_KANIDM_TLS_KEY.into());
    let has_cert = Path::new(&chain).is_file() && Path::new(&key).is_file();
    Plan {
        init: (!has_cert).then(|| Step {
            program: "kanidmd",
            args: vec!["cert-generate".into()],
        }),
        finish: None,
        exec: Step {
            program: "kanidmd",
            args: vec!["server".into()],
        },
    }
}

pub fn run(service: Service) -> ExitCode {
    let lookup = |key: &str| std::env::var(key).ok();
    let plan = match service {
        Service::Postgres => postgres_plan(lookup),
        Service::Kanidm => Ok(kanidm_plan(lookup)),
    };
    let plan = match plan {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("entrypoint: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(init) = plan.init {
        match Command::new(init.program).args(&init.args).status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!("entrypoint: {} failed: {status}", init.program);
                return ExitCode::FAILURE;
            }
            Err(error) => {
                eprintln!("entrypoint: cannot run {}: {error}", init.program);
                return ExitCode::FAILURE;
            }
        }
    }
    if let Some(pgdata) = plan.finish
        && let Err(error) = finish_init(&pgdata)
    {
        eprintln!(
            "entrypoint: cannot finish init of {}: {error}",
            pgdata.display()
        );
        return ExitCode::FAILURE;
    }
    let error = Command::new(plan.exec.program).args(&plan.exec.args).exec();
    eprintln!("entrypoint: cannot exec {}: {error}", plan.exec.program);
    ExitCode::FAILURE
}

/// initdb's `--auth-host` only covers loopback; other containers connect over the network.
pub fn finish_init(pgdata: &Path) -> std::io::Result<()> {
    let path = pgdata.join("pg_hba.conf");
    let current = std::fs::read_to_string(&path)?;
    let has_line = current
        .lines()
        .any(|line| line.split_whitespace().eq(REMOTE_SCRAM));
    if !has_line {
        let mut hba = std::fs::OpenOptions::new().append(true).open(&path)?;
        if !current.is_empty() && !current.ends_with('\n') {
            hba.write_all(b"\n")?;
        }
        hba.write_all(format!("{}\n", REMOTE_SCRAM.join(" ")).as_bytes())?;
        hba.sync_all()?;
    }
    std::fs::File::create(pgdata.join(INIT_DONE_MARKER))?.sync_all()?;
    std::fs::File::open(pgdata)?.sync_all()
}

fn non_empty(lookup: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    lookup(key).filter(|value| !value.is_empty())
}

fn flag(name: &str, path: &Path) -> OsString {
    let mut arg = OsString::from(name);
    arg.push(path);
    arg
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("cli-entrypoint-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create temp dir");
            Self(path)
        }

        fn file(&self, name: &str) -> String {
            let path = self.0.join(name);
            fs::write(&path, "x").expect("write file");
            path.to_str().expect("utf-8 path").to_string()
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_str().expect("utf-8 path").to_string()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn lookup(vars: Vec<(&'static str, String)>) -> impl Fn(&str) -> Option<String> {
        move |key| {
            vars.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
        }
    }

    fn args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn postgres_runs_initdb_when_pgdata_has_no_pg_version() {
        let dir = TempDir::new("pg-empty");
        let pgdata = dir.path("data");
        let pwfile = dir.file("pw");
        let plan = postgres_plan(lookup(vec![
            ("PGDATA", pgdata.clone()),
            ("POSTGRES_USER", "bakery".into()),
            ("POSTGRES_PASSWORD_FILE", pwfile.clone()),
        ]))
        .expect("plan");
        assert_eq!(
            plan.init,
            Some(Step {
                program: "initdb",
                args: args(&[
                    &format!("--pgdata={pgdata}"),
                    "--username=bakery",
                    &format!("--pwfile={pwfile}"),
                    "--auth-local=trust",
                    "--auth-host=scram-sha-256",
                    "--encoding=UTF8",
                    "--locale=C",
                    "--no-instructions",
                ]),
            })
        );
        assert_eq!(
            plan.exec,
            Step {
                program: "postgres",
                args: args(&["-D", &pgdata, "-c", "listen_addresses=*"]),
            }
        );
        assert_eq!(plan.finish, Some(PathBuf::from(&pgdata)));
    }

    #[test]
    fn postgres_skips_initdb_when_init_finished() {
        let dir = TempDir::new("pg-ready");
        dir.file("PG_VERSION");
        dir.file(INIT_DONE_MARKER);
        let plan = postgres_plan(lookup(vec![("PGDATA", dir.path(""))])).expect("plan");
        assert_eq!(plan.init, None);
        assert_eq!(plan.finish, None);
        assert_eq!(plan.exec.program, "postgres");
    }

    fn complete_cluster(dir: &TempDir, hba: &str) {
        dir.file("PG_VERSION");
        fs::create_dir_all(dir.0.join("global")).expect("create global");
        dir.file("global/pg_filenode.map");
        dir.file("postmaster.opts");
        fs::write(dir.0.join("pg_hba.conf"), hba).expect("write pg_hba.conf");
    }

    #[test]
    fn postgres_adopts_a_complete_cluster_from_before_the_marker() {
        let dir = TempDir::new("pg-adopt");
        let hba = "local all all trust\nhost  all all  samenet  scram-sha-256\n";
        complete_cluster(&dir, hba);
        let plan = postgres_plan(lookup(vec![("PGDATA", dir.path(""))])).expect("plan");
        assert_eq!(plan.init, None);
        assert_eq!(plan.finish, Some(dir.0.clone()));
        finish_init(&dir.0).expect("finish");
        assert_eq!(
            fs::read_to_string(dir.0.join("pg_hba.conf")).expect("read pg_hba.conf"),
            hba
        );
        assert!(dir.0.join(INIT_DONE_MARKER).is_file());
    }

    #[test]
    fn postgres_adopts_a_complete_cluster_and_adds_the_missing_hba_line() {
        let dir = TempDir::new("pg-adopt-hba");
        complete_cluster(
            &dir,
            "local all all trust\n# host all all samenet scram-sha-256\n",
        );
        let plan = postgres_plan(lookup(vec![("PGDATA", dir.path(""))])).expect("plan");
        assert_eq!(plan.init, None);
        assert_eq!(plan.finish, Some(dir.0.clone()));
        finish_init(&dir.0).expect("finish");
        assert_eq!(
            fs::read_to_string(dir.0.join("pg_hba.conf")).expect("read pg_hba.conf"),
            "local all all trust\n# host all all samenet scram-sha-256\nhost all all samenet scram-sha-256\n"
        );
        assert!(dir.0.join(INIT_DONE_MARKER).is_file());
    }

    #[test]
    fn postgres_refuses_a_cluster_killed_after_bootstrap() {
        let dir = TempDir::new("pg-bootstrap-only");
        dir.file("PG_VERSION");
        fs::create_dir_all(dir.0.join("global")).expect("create global");
        dir.file("global/pg_filenode.map");
        let pgdata = dir.path("");
        let error =
            postgres_plan(lookup(vec![("PGDATA", pgdata.clone())])).expect_err("must refuse");
        assert_eq!(error, PlanError::InitUnfinished(PathBuf::from(&pgdata)));
    }

    #[test]
    fn postgres_refuses_an_incomplete_cluster() {
        let dir = TempDir::new("pg-half");
        dir.file("PG_VERSION");
        let pgdata = dir.path("");
        let error = postgres_plan(lookup(vec![
            ("PGDATA", pgdata.clone()),
            ("POSTGRES_USER", "bakery".into()),
            ("POSTGRES_PASSWORD_FILE", dir.file("pw")),
        ]))
        .expect_err("must refuse");
        assert_eq!(error, PlanError::InitUnfinished(PathBuf::from(&pgdata)));
        let message = error.to_string();
        assert!(message.contains(INIT_DONE_MARKER), "{message}");
        assert!(message.contains("postmaster.opts"), "{message}");
        assert!(message.contains("initdb was interrupted"), "{message}");
        assert!(
            message.contains("not a PostgreSQL data directory"),
            "{message}"
        );
        assert!(message.contains("docker volume rm"), "{message}");
        assert!(fs::exists(dir.0.join("PG_VERSION")).expect("exists"));
    }

    #[test]
    fn finish_init_allows_remote_scram_then_writes_the_marker() {
        let dir = TempDir::new("pg-finish");
        let hba = dir.0.join("pg_hba.conf");
        fs::write(&hba, "local all all trust\n").expect("write pg_hba.conf");
        finish_init(&dir.0).expect("finish");
        assert_eq!(
            fs::read_to_string(&hba).expect("read pg_hba.conf"),
            "local all all trust\nhost all all samenet scram-sha-256\n"
        );
        assert!(dir.0.join(INIT_DONE_MARKER).is_file());
        let plan = postgres_plan(lookup(vec![("PGDATA", dir.path(""))])).expect("plan");
        assert_eq!(plan.init, None);
    }

    #[test]
    fn finish_init_starts_the_hba_line_on_a_new_line() {
        let dir = TempDir::new("pg-hba-no-newline");
        let hba = dir.0.join("pg_hba.conf");
        fs::write(&hba, "local all all trust").expect("write pg_hba.conf");
        finish_init(&dir.0).expect("finish");
        assert_eq!(
            fs::read_to_string(&hba).expect("read pg_hba.conf"),
            "local all all trust\nhost all all samenet scram-sha-256\n"
        );
    }

    #[test]
    fn finish_init_without_pg_hba_fails_and_writes_no_marker() {
        let dir = TempDir::new("pg-hba-missing");
        assert!(finish_init(&dir.0).is_err());
        assert!(!dir.0.join(INIT_DONE_MARKER).exists());
    }
    #[test]
    fn postgres_defaults_pgdata() {
        if Path::new(DEFAULT_PGDATA).join("PG_VERSION").exists() {
            return;
        }
        let dir = TempDir::new("pg-default");
        let plan = postgres_plan(lookup(vec![
            ("POSTGRES_USER", "bakery".into()),
            ("POSTGRES_PASSWORD_FILE", dir.file("pw")),
        ]))
        .expect("plan");
        assert_eq!(
            plan.exec.args,
            args(&["-D", DEFAULT_PGDATA, "-c", "listen_addresses=*"])
        );
    }

    #[test]
    fn postgres_init_needs_user_and_password_file() {
        let dir = TempDir::new("pg-missing");
        let pgdata = dir.path("data");
        assert_eq!(
            postgres_plan(lookup(vec![("PGDATA", pgdata.clone())])),
            Err(PlanError::Missing("POSTGRES_USER"))
        );
        assert_eq!(
            postgres_plan(lookup(vec![
                ("PGDATA", pgdata.clone()),
                ("POSTGRES_USER", "bakery".into()),
            ])),
            Err(PlanError::Missing("POSTGRES_PASSWORD_FILE"))
        );
        let missing = dir.path("no-such-pw");
        assert_eq!(
            postgres_plan(lookup(vec![
                ("PGDATA", pgdata),
                ("POSTGRES_USER", "bakery".into()),
                ("POSTGRES_PASSWORD_FILE", missing.clone()),
            ])),
            Err(PlanError::NoPasswordFile(missing.into()))
        );
    }

    #[test]
    fn kanidm_generates_cert_when_chain_or_key_is_missing() {
        let dir = TempDir::new("kanidm-missing");
        let chain = dir.file("chain.pem");
        let plan = kanidm_plan(lookup(vec![
            ("KANIDM_TLS_CHAIN", chain),
            ("KANIDM_TLS_KEY", dir.path("key.pem")),
        ]));
        assert_eq!(
            plan,
            Plan {
                init: Some(Step {
                    program: "kanidmd",
                    args: args(&["cert-generate"]),
                }),
                finish: None,
                exec: Step {
                    program: "kanidmd",
                    args: args(&["server"]),
                },
            }
        );
    }

    #[test]
    fn kanidm_keeps_an_existing_cert() {
        let dir = TempDir::new("kanidm-ready");
        let plan = kanidm_plan(lookup(vec![
            ("KANIDM_TLS_CHAIN", dir.file("chain.pem")),
            ("KANIDM_TLS_KEY", dir.file("key.pem")),
        ]));
        assert_eq!(plan.init, None);
        assert_eq!(plan.exec.args, args(&["server"]));
    }

    #[test]
    fn kanidm_defaults_to_cert_paths_under_data() {
        if Path::new(DEFAULT_KANIDM_TLS_CHAIN).exists() {
            return;
        }
        assert!(kanidm_plan(lookup(vec![])).init.is_some());
    }
}
