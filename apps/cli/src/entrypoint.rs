use std::ffi::OsString;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

pub const DEFAULT_PGDATA: &str = "/var/lib/postgresql/data";
pub const DEFAULT_KANIDM_TLS_CHAIN: &str = "/data/chain.pem";
pub const DEFAULT_KANIDM_TLS_KEY: &str = "/data/key.pem";

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
    /// pg_hba.conf that gets a remote scram rule after `init` succeeds.
    pub pg_hba: Option<PathBuf>,
    pub exec: Step,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("password file {0} does not exist")]
    NoPasswordFile(PathBuf),
}

pub fn postgres_plan(lookup: impl Fn(&str) -> Option<String>) -> Result<Plan, PlanError> {
    let pgdata = PathBuf::from(non_empty(&lookup, "PGDATA").unwrap_or(DEFAULT_PGDATA.into()));
    let init = if pgdata.join("PG_VERSION").exists() {
        None
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
        pg_hba: init.as_ref().map(|_| pgdata.join("pg_hba.conf")),
        init,
        exec: Step {
            program: "postgres",
            args: vec![
                "-D".into(),
                pgdata.into(),
                "-c".into(),
                "listen_addresses=*".into(),
            ],
        },
    })
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
        pg_hba: None,
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
    if let Some(pg_hba) = plan.pg_hba
        && let Err(error) = allow_remote_scram(&pg_hba)
    {
        eprintln!("entrypoint: cannot update {}: {error}", pg_hba.display());
        return ExitCode::FAILURE;
    }
    let error = Command::new(plan.exec.program).args(&plan.exec.args).exec();
    eprintln!("entrypoint: cannot exec {}: {error}", plan.exec.program);
    ExitCode::FAILURE
}

/// initdb's `--auth-host` only covers loopback; other containers connect over the network.
pub fn allow_remote_scram(pg_hba: &Path) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new().append(true).open(pg_hba)?;
    file.write_all(b"host all all all scram-sha-256\n")
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
        assert_eq!(
            plan.pg_hba,
            Some(PathBuf::from(&pgdata).join("pg_hba.conf"))
        );
    }

    #[test]
    fn postgres_skips_initdb_when_pg_version_exists() {
        let dir = TempDir::new("pg-ready");
        dir.file("PG_VERSION");
        let plan = postgres_plan(lookup(vec![("PGDATA", dir.path(""))])).expect("plan");
        assert_eq!(plan.init, None);
        assert_eq!(plan.pg_hba, None);
        assert_eq!(plan.exec.program, "postgres");
    }

    #[test]
    fn remote_scram_line_is_appended_to_pg_hba() {
        let dir = TempDir::new("pg-hba");
        let hba = dir.0.join("pg_hba.conf");
        fs::write(&hba, "local all all trust\n").expect("write pg_hba.conf");
        allow_remote_scram(&hba).expect("append");
        assert_eq!(
            fs::read_to_string(&hba).expect("read pg_hba.conf"),
            "local all all trust\nhost all all all scram-sha-256\n"
        );
    }

    #[test]
    fn remote_scram_needs_an_existing_pg_hba() {
        let dir = TempDir::new("pg-hba-missing");
        assert!(allow_remote_scram(&dir.0.join("pg_hba.conf")).is_err());
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
                pg_hba: None,
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
