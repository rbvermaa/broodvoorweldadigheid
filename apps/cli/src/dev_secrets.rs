use std::fs::{DirBuilder, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const POSTGRES_PASSWORD_FILE: &str = "postgres-password";
pub const DATABASE_ENV_FILE: &str = "database.env";
pub const RUSTIC_PASSWORD_FILE: &str = "rustic-password";

const PASSWORD_BYTES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} already exists; remove it first if you want new secrets")]
    Exists(PathBuf),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// The DATABASE_URL points at the `postgres` service of deploy/compose.yaml.
pub fn contents(postgres_password: &str, rustic_password: &str) -> [(&'static str, String); 3] {
    [
        (POSTGRES_PASSWORD_FILE, postgres_password.to_string()),
        (
            DATABASE_ENV_FILE,
            format!(
                "DATABASE_URL=postgres://postgres:{postgres_password}@postgres:5432/postgres\n"
            ),
        ),
        (RUSTIC_PASSWORD_FILE, rustic_password.to_string()),
    ]
}

/// Files are world-readable because the containers read them as uid 1000; a directory
/// this creates is 0700, which keeps other host users out.
pub fn write(
    dir: &Path,
    mut password: impl FnMut() -> std::io::Result<String>,
) -> Result<Vec<PathBuf>, Error> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| Error::Io { path, source }
    };
    let names = contents("", "").map(|(name, _)| dir.join(name));
    if let Some(existing) = names.iter().find(|path| path.exists()) {
        return Err(Error::Exists(existing.clone()));
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(io(dir))?;
    let postgres_password = password().map_err(io(dir))?;
    let rustic_password = password().map_err(io(dir))?;
    let mut written = Vec::new();
    for (name, text) in contents(&postgres_password, &rustic_password) {
        let path = dir.join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&path)
            .map_err(|source| match source.kind() {
                std::io::ErrorKind::AlreadyExists => Error::Exists(path.clone()),
                _ => Error::Io {
                    path: path.clone(),
                    source,
                },
            })?;
        file.write_all(text.as_bytes()).map_err(io(&path))?;
        written.push(path);
    }
    Ok(written)
}

pub fn random_password() -> std::io::Result<String> {
    let mut bytes = [0u8; PASSWORD_BYTES];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("cli-dev-secrets-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn counter() -> impl FnMut() -> std::io::Result<String> {
        let mut next = 0;
        move || {
            next += 1;
            Ok(format!("pw{next}"))
        }
    }

    fn read(dir: &Path, name: &str) -> String {
        fs::read_to_string(dir.join(name)).expect("read secret")
    }

    #[test]
    fn writes_the_three_secret_files() {
        let dir = TempDir::new("writes");
        let written = write(&dir.0, counter()).expect("write");
        assert_eq!(
            written,
            [
                dir.0.join(POSTGRES_PASSWORD_FILE),
                dir.0.join(DATABASE_ENV_FILE),
                dir.0.join(RUSTIC_PASSWORD_FILE),
            ]
        );
        assert_eq!(read(&dir.0, POSTGRES_PASSWORD_FILE), "pw1");
        assert_eq!(
            read(&dir.0, DATABASE_ENV_FILE),
            "DATABASE_URL=postgres://postgres:pw1@postgres:5432/postgres\n"
        );
        assert_eq!(read(&dir.0, RUSTIC_PASSWORD_FILE), "pw2");
    }

    #[test]
    fn creates_a_private_directory_with_readable_files() {
        let dir = TempDir::new("modes");
        let nested = dir.0.join("secrets");
        write(&nested, counter()).expect("write");
        let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(&nested), 0o700);
        assert_eq!(mode(&nested.join(RUSTIC_PASSWORD_FILE)), 0o644);
    }

    #[test]
    fn refuses_to_overwrite_and_writes_nothing() {
        let dir = TempDir::new("exists");
        fs::create_dir_all(&dir.0).expect("create dir");
        fs::write(dir.0.join(RUSTIC_PASSWORD_FILE), "keep me").expect("write existing");
        let error = write(&dir.0, counter()).expect_err("must refuse");
        assert!(
            matches!(&error, Error::Exists(path) if path == &dir.0.join(RUSTIC_PASSWORD_FILE)),
            "{error}"
        );
        assert!(!dir.0.join(POSTGRES_PASSWORD_FILE).exists());
        assert_eq!(read(&dir.0, RUSTIC_PASSWORD_FILE), "keep me");
    }

    #[test]
    fn random_passwords_are_long_hex_and_differ() {
        let first = random_password().expect("random");
        let second = random_password().expect("random");
        assert_eq!(first.len(), PASSWORD_BYTES * 2);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()), "{first}");
        assert_ne!(first, second);
    }
}
