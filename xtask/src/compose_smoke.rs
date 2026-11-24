use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

pub const USAGE: &str = "compose-smoke [--project NAME] [--main-project NAME] [--tag TAG] [--http-port N] [--https-port N] [--subnet CIDR] [--keep-on-failure]";

pub const IMAGES: [&str; 5] = [
    "bvw-server",
    "bvw-postgres",
    "bvw-kanidm",
    "bvw-caddy",
    "bvw-backup",
];

const CADDY_ROOT_CA: &str = "caddy:/data/caddy/pki/authorities/local/root.crt";
const BACKUP_REPO: &str = "--repo=/backup/repo";
const RUSTIC_PASSWORD: &str = "--password-file=/run/secrets/rustic_password";
const SEED_SQL: &str = "CREATE TABLE IF NOT EXISTS smoke_check (id integer PRIMARY KEY); \
                        INSERT INTO smoke_check VALUES (1) ON CONFLICT DO NOTHING;";
const ATTEMPTS: u32 = 60;
const RETRY_DELAY: Duration = Duration::from_secs(2);
const DEFAULT_SUBNET: &str = "10.89.1.0/24";
/// Caddy's fixed address is this many addresses into the compose subnet (deploy/compose.yaml).
const CADDY_OFFSET: u32 = 10;

#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    pub project: String,
    /// The `make up` project, which the smoke test must never use: it deletes its project's volumes.
    pub main_project: String,
    pub tag: String,
    pub http_port: u16,
    pub https_port: u16,
    pub subnet: String,
    pub caddy_ip: Ipv4Addr,
    /// Leave the stack running after a failure so it can be inspected.
    pub keep_on_failure: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            project: "bvw-smoke".into(),
            main_project: "bvw".into(),
            tag: "dev".into(),
            http_port: 18080,
            https_port: 18443,
            subnet: DEFAULT_SUBNET.into(),
            caddy_ip: Ipv4Addr::new(10, 89, 1, 10),
            keep_on_failure: false,
        }
    }
}

pub fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        let port = |name: &str, text: String| {
            text.parse::<u16>()
                .map_err(|_| format!("{name} {text:?} is not a port number"))
        };
        match arg.as_str() {
            "--project" => options.project = value(arg)?,
            "--main-project" => options.main_project = value(arg)?,
            "--tag" => options.tag = value(arg)?,
            "--http-port" => options.http_port = port(arg, value(arg)?)?,
            "--https-port" => options.https_port = port(arg, value(arg)?)?,
            "--subnet" => {
                options.subnet = value(arg)?;
                options.caddy_ip = caddy_ip(&options.subnet)?;
            }
            "--keep-on-failure" => options.keep_on_failure = true,
            other => return Err(format!("unknown argument {other:?}; usage: {USAGE}")),
        }
    }
    if options.project == options.main_project {
        return Err(format!(
            "--project {} is the `make up` stack; the smoke test would delete its volumes",
            options.project
        ));
    }
    Ok(options)
}

/// `subnet` is an IPv4 network in CIDR form, such as `10.89.1.0/24`.
pub fn caddy_ip(subnet: &str) -> Result<Ipv4Addr, String> {
    let bad = |why: &str| format!("--subnet {subnet:?}: {why}");
    let (address, prefix) = subnet
        .split_once('/')
        .ok_or_else(|| bad("expected an address/prefix such as 10.89.1.0/24"))?;
    let address: Ipv4Addr = address.parse().map_err(|_| bad("not an IPv4 address"))?;
    let prefix: u32 = prefix.parse().map_err(|_| bad("not a prefix length"))?;
    if prefix > 28 {
        return Err(bad("the prefix must be /28 or shorter"));
    }
    let host_bits = u32::MAX >> prefix;
    if u32::from(address) & host_bits != 0 {
        return Err(bad("not a network address"));
    }
    Ok(Ipv4Addr::from(u32::from(address) + CADDY_OFFSET))
}

pub fn health_ok(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body).is_ok_and(|health| health["status"] == "ok")
}

/// The home page answered 200 and stored a session: the server saves the session before it
/// responds, so a missing `http_session` table would give a 500.
pub fn session_ok(stdout: &str) -> bool {
    parse_curl(stdout).is_some_and(|(status, headers)| {
        status == 200
            && headers.lines().any(|line| {
                let line = line.trim_end();
                line.to_ascii_lowercase()
                    .starts_with("set-cookie: bvw_session=")
                    && line.split("; ").any(|part| part == "HttpOnly")
                    && line.split("; ").any(|part| part == "Secure")
            })
    })
}

/// `written` is curl's `--write-out "%{http_code} %{redirect_url}"`.
pub fn redirect_ok(written: &str, location: &str) -> bool {
    written.split_once(' ').is_some_and(|(status, url)| {
        matches!(status, "301" | "302" | "307" | "308") && url == location
    })
}

#[derive(Debug, PartialEq, Eq)]
pub struct Step {
    pub program: &'static str,
    pub args: Vec<String>,
}

pub struct Stack {
    pub options: Options,
    pub compose_file: PathBuf,
    pub work_dir: PathBuf,
}

impl Stack {
    pub fn secrets_dir(&self) -> PathBuf {
        self.work_dir.join("secrets")
    }

    pub fn ca_file(&self) -> PathBuf {
        self.work_dir.join("caddy-root.crt")
    }

    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("BVW_SECRETS_DIR", self.secrets_dir().display().to_string()),
            ("BVW_BIND_ADDRESS", "127.0.0.1".into()),
            ("BVW_HTTP_PORT", self.options.http_port.to_string()),
            ("BVW_HTTPS_PORT", self.options.https_port.to_string()),
            ("BVW_IMAGE_TAG", self.options.tag.clone()),
            ("BVW_SUBNET", self.options.subnet.clone()),
            ("BVW_CADDY_IP", self.options.caddy_ip.to_string()),
        ]
    }

    pub fn compose(&self, args: &[&str]) -> Step {
        let mut all = vec![
            "compose".to_string(),
            "--file".into(),
            self.compose_file.display().to_string(),
            "--project-name".into(),
            self.options.project.clone(),
        ];
        all.extend(args.iter().map(|arg| arg.to_string()));
        Step {
            program: "docker",
            args: all,
        }
    }

    pub fn up(&self) -> Step {
        self.compose(&["up", "--detach", "--wait", "--wait-timeout", "600"])
    }

    pub fn copy_ca(&self) -> Step {
        self.compose(&["cp", CADDY_ROOT_CA, &self.ca_file().display().to_string()])
    }

    pub fn seed(&self) -> Step {
        self.compose(&[
            "exec",
            "-T",
            "postgres",
            "psql",
            "--host=/run/postgresql",
            "--username=postgres",
            "--dbname=postgres",
            "--set=ON_ERROR_STOP=1",
            &format!("--command={SEED_SQL}"),
        ])
    }

    pub fn backup_run(&self) -> Step {
        self.compose(&[
            "run",
            "--rm",
            "backup",
            "backup",
            "run",
            BACKUP_REPO,
            RUSTIC_PASSWORD,
        ])
    }

    pub fn restore_test(&self) -> Step {
        self.compose(&[
            "run",
            "--rm",
            "backup",
            "backup",
            "restore-test",
            BACKUP_REPO,
            RUSTIC_PASSWORD,
        ])
    }

    pub fn logs(&self) -> Step {
        self.compose(&["logs", "--no-color", "--tail=200"])
    }

    /// The profile makes `down` also remove the on-demand backup service.
    pub fn down(&self) -> Step {
        self.compose(&[
            "--profile",
            "backup",
            "down",
            "--volumes",
            "--remove-orphans",
        ])
    }

    /// `--resolve` pins the host to 127.0.0.1, since not every resolver maps *.localhost.
    pub fn curl(&self, host: &str, path: &str) -> Step {
        let port = self.options.https_port;
        Step {
            program: "curl",
            args: vec![
                "--silent".into(),
                "--show-error".into(),
                "--max-time".into(),
                "10".into(),
                "--cacert".into(),
                self.ca_file().display().to_string(),
                "--resolve".into(),
                format!("{host}:{port}:127.0.0.1"),
                "--write-out".into(),
                "\n%{http_code}".into(),
                format!("https://{host}:{port}{path}"),
            ],
        }
    }

    /// Prints the response headers, then the status code on its own last line.
    pub fn curl_headers(&self, host: &str, path: &str) -> Step {
        let port = self.options.https_port;
        Step {
            program: "curl",
            args: vec![
                "--silent".into(),
                "--show-error".into(),
                "--max-time".into(),
                "10".into(),
                "--cacert".into(),
                self.ca_file().display().to_string(),
                "--resolve".into(),
                format!("{host}:{port}:127.0.0.1"),
                "--dump-header".into(),
                "-".into(),
                "--output".into(),
                "/dev/null".into(),
                "--write-out".into(),
                "\n%{http_code}".into(),
                format!("https://{host}:{port}{path}"),
            ],
        }
    }

    pub fn curl_redirect(&self, host: &str, path: &str) -> Step {
        let port = self.options.http_port;
        Step {
            program: "curl",
            args: vec![
                "--silent".into(),
                "--show-error".into(),
                "--max-time".into(),
                "10".into(),
                "--output".into(),
                "/dev/null".into(),
                "--resolve".into(),
                format!("{host}:{port}:127.0.0.1"),
                "--write-out".into(),
                "%{http_code} %{redirect_url}".into(),
                format!("http://{host}:{port}{path}"),
            ],
        }
    }
}

pub fn image_inspect(tag: &str) -> Step {
    let mut args = vec![
        "image".to_string(),
        "inspect".into(),
        "--format={{.Id}}".into(),
    ];
    args.extend(IMAGES.iter().map(|image| format!("{image}:{tag}")));
    Step {
        program: "docker",
        args,
    }
}

pub fn dev_secrets(dir: &Path) -> Step {
    Step {
        program: "cargo",
        args: vec![
            "run".into(),
            "--quiet".into(),
            "--package".into(),
            "cli".into(),
            "--".into(),
            "dev-secrets".into(),
            dir.display().to_string(),
        ],
    }
}

/// Splits curl output written with `--write-out "\n%{http_code}"` into status and body.
pub fn parse_curl(stdout: &str) -> Option<(u16, &str)> {
    let (body, status) = stdout.rsplit_once('\n')?;
    Some((status.trim().parse().ok()?, body))
}

pub fn run(args: &[String]) -> Result<Vec<String>, String> {
    let options = parse(args)?;
    let root = workspace_root()?;
    let work_dir = std::env::temp_dir().join(format!("bvw-compose-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir)
        .map_err(|error| format!("cannot create {}: {error}", work_dir.display()))?;
    let stack = Stack {
        options,
        compose_file: root.join("deploy/compose.yaml"),
        work_dir,
    };

    let result = smoke(&stack);
    if result.is_err() {
        match execute(&stack, &stack.logs()) {
            Ok(output) => eprint!("{}", String::from_utf8_lossy(&output.stdout)),
            Err(error) => eprintln!("compose-smoke: {error}"),
        }
    }
    if result.is_err() && stack.options.keep_on_failure {
        eprintln!(
            "compose-smoke: stack {} left running; secrets in {}",
            stack.options.project,
            stack.secrets_dir().display()
        );
    } else {
        let down = execute(&stack, &stack.down());
        let _ = std::fs::remove_dir_all(&stack.work_dir);
        down?;
    }
    result.map(|()| Vec::new())
}

fn smoke(stack: &Stack) -> Result<(), String> {
    execute(stack, &image_inspect(&stack.options.tag))
        .map_err(|error| format!("{error}\nload the images first with `make images`"))?;
    execute(stack, &dev_secrets(&stack.secrets_dir()))?;
    execute(stack, &stack.up())?;
    retry("Caddy root CA", || {
        execute(stack, &stack.copy_ca()).map(drop)
    })?;
    retry("https://localhost/health", || {
        expect(stack, "localhost", "/health", health_ok)
    })?;
    retry("https://idm.localhost/status", || {
        expect(stack, "idm.localhost", "/status", |body| {
            body.trim() == "true"
        })
    })?;
    retry("https://idm.localhost/ui/login", || {
        expect(stack, "idm.localhost", "/ui/login", |body| {
            !body.trim().is_empty()
        })
    })?;
    expect_redirect(stack, "localhost", "/health")?;
    expect_redirect(stack, "idm.localhost", "/status")?;
    expect_session(stack)?;
    execute(stack, &stack.seed())?;
    execute(stack, &stack.backup_run())?;
    execute(stack, &stack.restore_test())?;
    println!("compose-smoke: health, Kanidm, redirects, session, backup and restore test passed");
    Ok(())
}

fn expect(stack: &Stack, host: &str, path: &str, body_ok: fn(&str) -> bool) -> Result<(), String> {
    let output = execute(stack, &stack.curl(host, path))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_curl(&stdout) {
        Some((200, body)) if body_ok(body) => {
            println!("compose-smoke: https://{host}{path} -> 200");
            Ok(())
        }
        Some((status, body)) => Err(format!("https://{host}{path} -> {status}: {body}")),
        None => Err(format!(
            "https://{host}{path}: unexpected curl output {stdout:?}"
        )),
    }
}

fn expect_session(stack: &Stack) -> Result<(), String> {
    let output = execute(stack, &stack.curl_headers("localhost", "/"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if session_ok(&stdout) {
        println!("compose-smoke: https://localhost/ -> 200 with a session cookie");
        Ok(())
    } else {
        Err(format!(
            "https://localhost/: expected 200 and a session cookie, got {stdout:?}"
        ))
    }
}

fn expect_redirect(stack: &Stack, host: &str, path: &str) -> Result<(), String> {
    let location = format!("https://{host}:{}{path}", stack.options.https_port);
    let output = execute(stack, &stack.curl_redirect(host, path))?;
    let written = String::from_utf8_lossy(&output.stdout);
    if redirect_ok(&written, &location) {
        println!("compose-smoke: http://{host}{path} -> {location}");
        Ok(())
    } else {
        Err(format!(
            "http://{host}{path}: expected a redirect to {location}, got {written:?}"
        ))
    }
}

fn retry(what: &str, mut attempt: impl FnMut() -> Result<(), String>) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..ATTEMPTS {
        match attempt() {
            Ok(()) => return Ok(()),
            Err(error) => last = error,
        }
        std::thread::sleep(RETRY_DELAY);
    }
    Err(format!(
        "{what} not ready after {ATTEMPTS} attempts: {last}"
    ))
}

fn execute(stack: &Stack, step: &Step) -> Result<Output, String> {
    let output = Command::new(step.program)
        .args(&step.args)
        .envs(stack.env())
        .output()
        .map_err(|error| format!("cannot run {}: {error}", step.program))?;
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "{} {} failed ({}): {}",
            step.program,
            step.args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stdout).trim()
        ))
    }
}

fn workspace_root() -> Result<PathBuf, String> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no parent", manifest_dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn stack() -> Stack {
        Stack {
            options: Options {
                project: "bvw-fix-b".into(),
                main_project: "bvw-main".into(),
                tag: "s9".into(),
                http_port: 28080,
                https_port: 28443,
                subnet: "10.89.7.0/24".into(),
                caddy_ip: Ipv4Addr::new(10, 89, 7, 10),
                keep_on_failure: false,
            },
            compose_file: PathBuf::from("/repo/deploy/compose.yaml"),
            work_dir: PathBuf::from("/tmp/smoke"),
        }
    }

    fn compose_prefix() -> Vec<String> {
        strings(&[
            "compose",
            "--file",
            "/repo/deploy/compose.yaml",
            "--project-name",
            "bvw-fix-b",
        ])
    }

    fn with_prefix(rest: &[&str]) -> Vec<String> {
        let mut all = compose_prefix();
        all.extend(strings(rest));
        all
    }

    #[test]
    fn parse_defaults() {
        assert_eq!(parse(&[]), Ok(Options::default()));
        assert_eq!(Options::default().project, "bvw-smoke");
    }

    #[test]
    fn defaults_do_not_clash_with_make_up() {
        let options = Options::default();
        assert_ne!(options.project, options.main_project);
        assert_eq!(options.main_project, "bvw");
        assert_eq!((options.http_port, options.https_port), (18080, 18443));
        assert_eq!(options.subnet, "10.89.1.0/24");
        assert_eq!(options.caddy_ip, Ipv4Addr::new(10, 89, 1, 10));
    }

    #[test]
    fn parse_refuses_the_main_project() {
        let error = parse(&strings(&["--project", "bvw"])).expect_err("must refuse");
        assert!(error.contains("bvw"), "{error}");
    }

    #[test]
    fn parse_takes_the_main_project_from_make() {
        let error = parse(&strings(&["--project", "mine", "--main-project", "mine"]))
            .expect_err("must refuse");
        assert!(error.contains("mine"), "{error}");
        assert!(parse(&strings(&["--main-project", "mine", "--project", "bvw"])).is_ok());
        assert!(parse(&strings(&["--main-project"])).is_err());
    }

    #[test]
    fn caddy_gets_the_tenth_address_of_the_subnet() {
        assert_eq!(caddy_ip("10.89.0.0/24"), Ok(Ipv4Addr::new(10, 89, 0, 10)));
        assert_eq!(
            caddy_ip("172.30.16.0/20"),
            Ok(Ipv4Addr::new(172, 30, 16, 10))
        );
        assert_eq!(caddy_ip("10.0.0.16/28"), Ok(Ipv4Addr::new(10, 0, 0, 26)));
        for bad in [
            "10.89.0.0",
            "10.89.0.0/29",
            "10.89.0.0/33",
            "10.89.0.5/24",
            "nope/24",
            "10.89.0.0/x",
        ] {
            assert!(caddy_ip(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn session_needs_200_and_a_secure_http_only_cookie() {
        let ok = "HTTP/2 200\r\nset-cookie: bvw_session=abc; HttpOnly; SameSite=Lax; Secure; Path=/\r\n\r\n\n200";
        assert!(session_ok(ok));
        assert!(!session_ok(&ok.replace("\n200", "\n500")));
        assert!(!session_ok(&ok.replace("; Secure", "")));
        assert!(!session_ok(&ok.replace("bvw_session", "other")));
        assert!(!session_ok("HTTP/2 200\r\n\r\n\n200"));
    }

    #[test]
    fn health_must_report_ok() {
        assert!(health_ok(r#"{"status":"ok","db":"ok"}"#));
        assert!(!health_ok(r#"{"status":"degraded","db":"down"}"#));
        assert!(!health_ok(r#"{"db":"ok"}"#));
        assert!(!health_ok("ok"));
    }

    #[test]
    fn http_redirects_to_the_published_https_port() {
        let step = stack().curl_redirect("localhost", "/health");
        assert_eq!(step.program, "curl");
        assert_eq!(
            step.args,
            strings(&[
                "--silent",
                "--show-error",
                "--max-time",
                "10",
                "--output",
                "/dev/null",
                "--resolve",
                "localhost:28080:127.0.0.1",
                "--write-out",
                "%{http_code} %{redirect_url}",
                "http://localhost:28080/health",
            ])
        );
        assert!(redirect_ok(
            "302 https://localhost:28443/health",
            "https://localhost:28443/health"
        ));
        assert!(!redirect_ok(
            "308 https://localhost/health",
            "https://localhost:28443/health"
        ));
        assert!(!redirect_ok("200 ", "https://localhost:28443/health"));
    }

    #[test]
    fn parse_all_flags() {
        let options = parse(&strings(&[
            "--project",
            "bvw-fix-b",
            "--main-project",
            "bvw-main",
            "--tag",
            "s9",
            "--http-port",
            "28080",
            "--https-port",
            "28443",
            "--subnet",
            "10.89.7.0/24",
            "--keep-on-failure",
        ]))
        .expect("valid");
        assert_eq!(
            options,
            Options {
                keep_on_failure: true,
                ..stack().options
            }
        );
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert!(parse(&strings(&["--https-port", "x"])).is_err());
        assert!(parse(&strings(&["--https-port", "70000"])).is_err());
        assert!(parse(&strings(&["--project"])).is_err());
        assert!(parse(&strings(&["--bogus"])).is_err());
    }

    #[test]
    fn env_points_compose_at_the_secrets_and_ports() {
        assert_eq!(
            stack().env(),
            [
                ("BVW_SECRETS_DIR", "/tmp/smoke/secrets".to_string()),
                ("BVW_BIND_ADDRESS", "127.0.0.1".to_string()),
                ("BVW_HTTP_PORT", "28080".to_string()),
                ("BVW_HTTPS_PORT", "28443".to_string()),
                ("BVW_IMAGE_TAG", "s9".to_string()),
                ("BVW_SUBNET", "10.89.7.0/24".to_string()),
                ("BVW_CADDY_IP", "10.89.7.10".to_string()),
            ]
        );
    }

    #[test]
    fn up_waits_for_healthchecks() {
        let step = stack().up();
        assert_eq!(step.program, "docker");
        assert_eq!(
            step.args,
            with_prefix(&["up", "--detach", "--wait", "--wait-timeout", "600"])
        );
    }

    #[test]
    fn down_removes_volumes() {
        assert_eq!(
            stack().down().args,
            with_prefix(&[
                "--profile",
                "backup",
                "down",
                "--volumes",
                "--remove-orphans"
            ])
        );
    }

    #[test]
    fn copy_ca_reads_from_the_caddy_volume() {
        assert_eq!(
            stack().copy_ca().args,
            with_prefix(&[
                "cp",
                "caddy:/data/caddy/pki/authorities/local/root.crt",
                "/tmp/smoke/caddy-root.crt"
            ])
        );
    }

    #[test]
    fn backup_and_restore_test_use_the_backup_volume() {
        let stack = stack();
        assert_eq!(
            stack.backup_run().args,
            with_prefix(&[
                "run",
                "--rm",
                "backup",
                "backup",
                "run",
                "--repo=/backup/repo",
                "--password-file=/run/secrets/rustic_password",
            ])
        );
        assert_eq!(
            stack.restore_test().args,
            with_prefix(&[
                "run",
                "--rm",
                "backup",
                "backup",
                "restore-test",
                "--repo=/backup/repo",
                "--password-file=/run/secrets/rustic_password",
            ])
        );
    }

    #[test]
    fn seed_uses_the_local_socket() {
        let args = stack().seed().args;
        assert_eq!(
            args[..9],
            with_prefix(&["exec", "-T", "postgres", "psql"])[..]
        );
        assert!(
            args.contains(&"--host=/run/postgresql".to_string()),
            "{args:?}"
        );
        assert!(
            args.contains(&"--set=ON_ERROR_STOP=1".to_string()),
            "{args:?}"
        );
        assert!(
            args.last().is_some_and(|sql| sql.contains("CREATE TABLE")),
            "{args:?}"
        );
    }

    #[test]
    fn curl_trusts_only_the_caddy_ca() {
        let step = stack().curl("idm.localhost", "/status");
        assert_eq!(step.program, "curl");
        assert_eq!(
            step.args,
            strings(&[
                "--silent",
                "--show-error",
                "--max-time",
                "10",
                "--cacert",
                "/tmp/smoke/caddy-root.crt",
                "--resolve",
                "idm.localhost:28443:127.0.0.1",
                "--write-out",
                "\n%{http_code}",
                "https://idm.localhost:28443/status",
            ])
        );
        assert!(!step.args.contains(&"--insecure".to_string()));
    }

    #[test]
    fn image_inspect_checks_every_image() {
        let step = image_inspect("s9");
        assert_eq!(step.program, "docker");
        assert_eq!(&step.args[..3], ["image", "inspect", "--format={{.Id}}"]);
        assert_eq!(
            &step.args[3..],
            [
                "bvw-server:s9",
                "bvw-postgres:s9",
                "bvw-kanidm:s9",
                "bvw-caddy:s9",
                "bvw-backup:s9"
            ]
        );
    }

    #[test]
    fn dev_secrets_runs_the_cli() {
        assert_eq!(
            dev_secrets(Path::new("/tmp/smoke/secrets")).args,
            strings(&[
                "run",
                "--quiet",
                "--package",
                "cli",
                "--",
                "dev-secrets",
                "/tmp/smoke/secrets"
            ])
        );
    }

    #[test]
    fn parse_curl_splits_status_from_body() {
        assert_eq!(parse_curl("true\n200"), Some((200, "true")));
        assert_eq!(parse_curl("\n502"), Some((502, "")));
        assert_eq!(parse_curl("200"), None);
        assert_eq!(parse_curl("x\nabc"), None);
    }
}
