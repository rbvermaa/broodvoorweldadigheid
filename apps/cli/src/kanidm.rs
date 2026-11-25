//! Local Kanidm set-up for staff login (F13). Runs inside the kanidm container through
//! `make kanidm-setup` and `make kanidm-person`, which need the `kanidm` client and
//! `kanidmd` on PATH and the server's admin socket.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const DEFAULT_URL: &str = "https://idm.localhost:8443";
pub const DEFAULT_DATA_DIR: &str = "/data";
const ADMIN: &str = "idm_admin";
const CA_COPY: &str = "/tmp/bvw-kanidm-ca.pem";
const TOKEN_CACHE: &str = "/tmp/bvw-kanidm-tokens";
/// The image user's home is the read-only /var/empty, and the kanidm client creates
/// `~/.cache` even with `KANIDM_TOKEN_CACHE_PATH` set.
const HOME: &str = "/tmp/bvw-kanidm-home";
/// Only members of this group may log in to the shop; their role lives in `staff_user`.
pub const STAFF_GROUP: &str = "bvw_staff";
const SCOPES: [&str; 2] = ["openid", "email"];

#[derive(Debug, clap::Subcommand)]
pub enum KanidmCommand {
    #[command(about = "Print Kanidm's CA certificate (PEM), for the server's OIDC_CA_FILE")]
    Ca {
        #[arg(long, default_value = DEFAULT_DATA_DIR)]
        data_dir: PathBuf,
    },
    #[command(about = "Register the staff OAuth2 client if needed and print its client secret")]
    Oauth2Client {
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
        #[arg(long, default_value = "bvw-staff")]
        client_id: String,
        #[arg(long, help = "where the shop lives, e.g. https://localhost:8443/staff")]
        landing_url: String,
        #[arg(long, help = "e.g. https://localhost:8443/staff/oidc/callback")]
        redirect_url: String,
        #[arg(long, default_value = DEFAULT_DATA_DIR)]
        data_dir: PathBuf,
    },
    #[command(
        about = "Create a person in the bvw_staff group and print their subject and a passkey set-up link"
    )]
    Person {
        #[arg(long, default_value = DEFAULT_URL)]
        url: String,
        #[arg(long, env = "PERSON_NAME", help = "login name, e.g. bas")]
        name: String,
        #[arg(long, env = "PERSON_DISPLAY_NAME")]
        display_name: String,
        #[arg(long, env = "PERSON_MAIL")]
        mail: Option<String>,
        #[arg(long, default_value = DEFAULT_DATA_DIR)]
        data_dir: PathBuf,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no certificate in {0}")]
    NoCertificate(PathBuf),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot run {program}: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("`{command}` failed: {stderr}")]
    Failed { command: String, stderr: String },
    #[error("unexpected output from `{command}`: {output}")]
    Parse { command: String, output: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub program: &'static str,
    pub args: Vec<String>,
}

fn kanidm(args: &[&str]) -> Step {
    Step {
        program: "kanidm",
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
    }
}

/// `ca.pem` from `kanidmd cert-generate`, or else the last certificate in `chain.pem`.
///
/// # Errors
/// When neither file holds a certificate.
pub fn ca_pem(data_dir: &Path) -> Result<String, Error> {
    let ca = data_dir.join("ca.pem");
    if let Ok(pem) = std::fs::read_to_string(&ca)
        && let Some(cert) = last_certificate(&pem)
    {
        return Ok(cert);
    }
    let chain = data_dir.join("chain.pem");
    let pem = std::fs::read_to_string(&chain).map_err(|source| Error::Io {
        path: chain.clone(),
        source,
    })?;
    last_certificate(&pem).ok_or(Error::NoCertificate(chain))
}

pub fn last_certificate(pem: &str) -> Option<String> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";
    let start = pem.rfind(BEGIN)?;
    let end = pem[start..].find(END)? + start + END.len();
    Some(format!("{}\n", &pem[start..end]))
}

/// `kanidmd recover-account` logs the new password as a `new_password` field.
pub fn recovered_password(output: &str) -> Option<String> {
    let after = &output[output.find("new_password")? + "new_password".len()..];
    let value = after.trim_start_matches(['"', ':', '=', ' ']);
    let password: String = value
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '"' && *c != ',' && *c != '}')
        .collect();
    (!password.is_empty()).then_some(password)
}

pub fn oauth2_client_steps(client_id: &str, landing_url: &str, redirect_url: &str) -> Vec<Step> {
    let mut scope_map = vec![
        "system",
        "oauth2",
        "update-scope-map",
        client_id,
        STAFF_GROUP,
    ];
    scope_map.extend(SCOPES);
    vec![
        kanidm(&[
            "system",
            "oauth2",
            "add-redirect-url",
            client_id,
            redirect_url,
        ]),
        kanidm(&["system", "oauth2", "enable-strict-redirect-url", client_id]),
        kanidm(&scope_map),
        kanidm(&["system", "oauth2", "prefer-short-username", client_id]),
        kanidm(&[
            "system",
            "oauth2",
            "set-landing-url",
            client_id,
            landing_url,
        ]),
    ]
}

/// The person's UUID, which is the `sub` claim in Kanidm's ID tokens.
pub fn person_uuid(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let value = line.trim().strip_prefix("uuid:")?.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

/// Runs `kanidm` as idm_admin against `url`, trusting Kanidm's own CA.
struct Admin {
    url: String,
}

impl Admin {
    fn login(url: &str, data_dir: &Path) -> Result<Self, Error> {
        std::fs::create_dir_all(HOME).map_err(|source| Error::Io {
            path: HOME.into(),
            source,
        })?;
        std::fs::write(CA_COPY, ca_pem(data_dir)?).map_err(|source| Error::Io {
            path: CA_COPY.into(),
            source,
        })?;
        let recover = Step {
            program: "kanidmd",
            args: vec!["recover-account".to_string(), ADMIN.to_string()],
        };
        let output = run(&recover, &[])?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let password = recovered_password(&text).ok_or_else(|| Error::Parse {
            command: describe(&recover),
            output: "no new_password field".to_string(),
        })?;
        let admin = Self {
            url: url.to_string(),
        };
        admin.run_with(
            &kanidm(&["login"]),
            &[("KANIDM_PASSWORD", password.as_str())],
        )?;
        Ok(admin)
    }

    fn run(&self, step: &Step) -> Result<String, Error> {
        self.run_with(step, &[])
    }

    fn run_with(&self, step: &Step, extra: &[(&str, &str)]) -> Result<String, Error> {
        let mut env = vec![
            ("KANIDM_URL", self.url.as_str()),
            ("KANIDM_CA_PATH", CA_COPY),
            ("KANIDM_NAME", ADMIN),
            ("KANIDM_TOKEN_CACHE_PATH", TOKEN_CACHE),
            ("HOME", HOME),
        ];
        env.extend_from_slice(extra);
        let output = run(step, &env)?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// `kanidm ... get` exits 0 also when there is no such entry. A failed lookup counts as
    /// absent; the `create` that follows then reports the real error.
    fn found(&self, step: &Step) -> bool {
        self.run(step).is_ok_and(|output| entry_found(&output))
    }
}

pub fn entry_found(output: &str) -> bool {
    !output.trim().is_empty() && !output.contains("No matching entries")
}

fn describe(step: &Step) -> String {
    format!("{} {}", step.program, step.args.join(" "))
}

fn run(step: &Step, env: &[(&str, &str)]) -> Result<Output, Error> {
    let output = Command::new(step.program)
        .args(&step.args)
        .envs(env.iter().copied())
        .output()
        .map_err(|source| Error::Spawn {
            program: step.program.to_string(),
            source,
        })?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(Error::Failed {
            command: describe(step),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// Runs `command` and returns what to print on stdout.
///
/// # Errors
/// When Kanidm cannot be reached or refuses a step.
pub fn execute(command: KanidmCommand) -> Result<String, Error> {
    match command {
        KanidmCommand::Ca { data_dir } => ca_pem(&data_dir),
        KanidmCommand::Oauth2Client {
            url,
            client_id,
            landing_url,
            redirect_url,
            data_dir,
        } => {
            let admin = Admin::login(&url, &data_dir)?;
            if !admin.found(&kanidm(&["group", "get", STAFF_GROUP])) {
                admin.run(&kanidm(&["group", "create", STAFF_GROUP]))?;
            }
            let exists = admin.found(&kanidm(&["system", "oauth2", "get", &client_id]));
            if !exists {
                admin.run(&kanidm(&[
                    "system",
                    "oauth2",
                    "create",
                    &client_id,
                    "Brood voor Weldadigheid",
                    &landing_url,
                ]))?;
            }
            for step in oauth2_client_steps(&client_id, &landing_url, &redirect_url) {
                admin.run(&step)?;
            }
            let show = kanidm(&["system", "oauth2", "show-basic-secret", &client_id]);
            let output = admin.run(&show)?;
            let secret = output
                .lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .ok_or_else(|| Error::Parse {
                    command: describe(&show),
                    output: String::new(),
                })?;
            Ok(format!("{secret}\n"))
        }
        KanidmCommand::Person {
            url,
            name,
            display_name,
            mail,
            data_dir,
        } => {
            let admin = Admin::login(&url, &data_dir)?;
            if !admin.found(&kanidm(&["person", "get", &name])) {
                admin.run(&kanidm(&["person", "create", &name, &display_name]))?;
            }
            if let Some(mail) = mail.as_deref().filter(|mail| !mail.is_empty()) {
                admin.run(&kanidm(&["person", "update", &name, "--mail", mail]))?;
            }
            admin.run(&kanidm(&["group", "add-members", STAFF_GROUP, &name]))?;
            let get = kanidm(&["person", "get", &name]);
            let details = admin.run(&get)?;
            let uuid = person_uuid(&details).ok_or_else(|| Error::Parse {
                command: describe(&get),
                output: details.clone(),
            })?;
            let reset = admin.run(&kanidm(&[
                "person",
                "credential",
                "create-reset-token",
                &name,
            ]))?;
            Ok(format!("sub: {uuid}\n{reset}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAF: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----";
    const CA: &str = "-----BEGIN CERTIFICATE-----\nBBBB\n-----END CERTIFICATE-----";

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("cli-kanidm-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create dir");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_ca_is_the_last_certificate_of_the_chain() {
        assert_eq!(
            last_certificate(&format!("{LEAF}\n{CA}\n")),
            Some(format!("{CA}\n"))
        );
        assert_eq!(last_certificate(LEAF), Some(format!("{LEAF}\n")));
        assert_eq!(last_certificate("no pem here"), None);
        assert_eq!(last_certificate("-----BEGIN CERTIFICATE-----\nAAAA"), None);
    }

    #[test]
    fn ca_pem_prefers_ca_pem_over_the_chain() {
        let dir = TempDir::new("ca");
        std::fs::write(dir.0.join("chain.pem"), format!("{LEAF}\n{CA}\n")).expect("chain");
        assert_eq!(ca_pem(&dir.0).expect("from chain"), format!("{CA}\n"));
        let own = "-----BEGIN CERTIFICATE-----\nCCCC\n-----END CERTIFICATE-----";
        std::fs::write(dir.0.join("ca.pem"), own).expect("ca");
        assert_eq!(ca_pem(&dir.0).expect("from ca.pem"), format!("{own}\n"));
    }

    #[test]
    fn ca_pem_fails_without_a_certificate() {
        let dir = TempDir::new("none");
        assert!(matches!(ca_pem(&dir.0), Err(Error::Io { .. })));
        std::fs::write(dir.0.join("chain.pem"), "junk").expect("chain");
        assert!(matches!(ca_pem(&dir.0), Err(Error::NoCertificate(_))));
    }

    #[test]
    fn the_recovered_password_is_read_from_text_or_json_logs() {
        for output in [
            "INFO new_password: \"Xy12abc\"\n",
            "new_password=Xy12abc other=1",
            "{\"new_password\":\"Xy12abc\",\"level\":\"INFO\"}",
            "INFO 00000000 [info]: | new_password: Xy12abc\n",
        ] {
            assert_eq!(
                recovered_password(output).as_deref(),
                Some("Xy12abc"),
                "{output}"
            );
        }
        assert_eq!(recovered_password("nothing"), None);
        assert_eq!(recovered_password("new_password: \"\""), None);
    }

    #[test]
    fn the_client_gets_the_redirect_scopes_and_landing_url() {
        let steps = oauth2_client_steps(
            "bvw-staff",
            "https://localhost:8443/staff",
            "https://localhost:8443/staff/oidc/callback",
        );
        let lines: Vec<String> = steps.iter().map(describe).collect();
        assert_eq!(
            lines,
            [
                "kanidm system oauth2 add-redirect-url bvw-staff https://localhost:8443/staff/oidc/callback",
                "kanidm system oauth2 enable-strict-redirect-url bvw-staff",
                "kanidm system oauth2 update-scope-map bvw-staff bvw_staff openid email",
                "kanidm system oauth2 prefer-short-username bvw-staff",
                "kanidm system oauth2 set-landing-url bvw-staff https://localhost:8443/staff",
            ]
        );
    }

    #[test]
    fn no_matching_entries_is_not_an_entry() {
        assert!(!entry_found("No matching entries\n"));
        assert!(!entry_found(""));
        assert!(entry_found("---\nclass: group\nname: bvw_staff\n"));
    }

    #[test]
    fn the_person_uuid_comes_from_person_get() {
        let output = "---\nclass: person\nname: bas\nuuid: 0d2f1f9e-1111-2222-3333-444455556666\n";
        assert_eq!(
            person_uuid(output).as_deref(),
            Some("0d2f1f9e-1111-2222-3333-444455556666")
        );
        assert_eq!(person_uuid("name: bas\n"), None);
    }

    #[test]
    fn a_missing_program_is_reported() {
        let step = Step {
            program: "bvw-no-such-program",
            args: vec![],
        };
        assert!(matches!(run(&step, &[]), Err(Error::Spawn { .. })));
    }
}
