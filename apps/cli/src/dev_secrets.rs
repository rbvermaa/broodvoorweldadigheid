use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const POSTGRES_PASSWORD_FILE: &str = "postgres-password";
pub const DATABASE_ENV_FILE: &str = "database.env";
pub const RUSTIC_PASSWORD_FILE: &str = "rustic-password";

const PASSWORD_BYTES: usize = 32;
const FILE_MODE: u32 = 0o644;
const DIR_MODE: u32 = 0o700;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} already exists; remove it first if you want new secrets")]
    Exists(PathBuf),
    #[error(
        "{path} has mode {mode:o}, so other users can read the secrets; run `chmod 700` on it first"
    )]
    OpenDirectory { path: PathBuf, mode: u32 },
    #[error(
        "cannot hard link {path}: {source}; the secrets directory must be on a filesystem that supports hard links"
    )]
    HardLink {
        path: PathBuf,
        source: std::io::Error,
    },
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

/// Files are world-readable because the containers read them as uid 1000; the directory
/// must be 0700, which keeps other host users out. Either all files are written or none.
pub fn write(
    dir: &Path,
    mut password: impl FnMut() -> std::io::Result<String>,
) -> Result<Vec<PathBuf>, Error> {
    let names = contents("", "").map(|(name, _)| dir.join(name));
    if let Some(existing) = names.iter().find(|path| path.exists()) {
        return Err(Error::Exists(existing.clone()));
    }
    private_dir(dir)?;
    let postgres_password = password().map_err(io(dir))?;
    let rustic_password = password().map_err(io(dir))?;
    let mut created = Created(Vec::new());
    let mut staged = Vec::new();
    for (name, text) in contents(&postgres_password, &rustic_password) {
        let temp = dir.join(format!(".{name}.tmp"));
        create_new(&temp, &mut created)?;
        std::fs::write(&temp, text).map_err(io(&temp))?;
        std::fs::set_permissions(&temp, Permissions::from_mode(FILE_MODE)).map_err(io(&temp))?;
        staged.push((temp, dir.join(name)));
    }
    for (temp, path) in &staged {
        std::fs::hard_link(temp, path).map_err(|source| link_error(path, source))?;
        created.0.push(path.clone());
    }
    for (temp, _) in &staged {
        std::fs::remove_file(temp).map_err(io(temp))?;
    }
    created.0.clear();
    Ok(staged.into_iter().map(|(_, path)| path).collect())
}

/// Removes every path it holds when dropped, so a failed `write` leaves nothing behind.
struct Created(Vec<PathBuf>);

impl Drop for Created {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn private_dir(dir: &Path) -> Result<(), Error> {
    match std::fs::metadata(dir) {
        Ok(metadata) => {
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                return Err(Error::OpenDirectory {
                    path: dir.to_path_buf(),
                    mode,
                });
            }
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::NotFound => DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(dir)
            .map_err(io(dir)),
        Err(error) => Err(io(dir)(error)),
    }
}

fn create_new(path: &Path, created: &mut Created) -> Result<(), Error> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| exists_or_io(path, source))?;
    created.0.push(path.to_path_buf());
    Ok(())
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io { path, source }
}

fn link_error(path: &Path, source: std::io::Error) -> Error {
    match source.kind() {
        ErrorKind::AlreadyExists => Error::Exists(path.to_path_buf()),
        _ => Error::HardLink {
            path: path.to_path_buf(),
            source,
        },
    }
}

fn exists_or_io(path: &Path, source: std::io::Error) -> Error {
    match source.kind() {
        ErrorKind::AlreadyExists => Error::Exists(path.to_path_buf()),
        _ => io(path)(source),
    }
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

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    fn private_dir(path: &Path) {
        fs::create_dir_all(path).expect("create dir");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("chmod");
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn every_file_is_readable_by_the_containers() {
        let dir = TempDir::new("all-modes");
        for path in write(&dir.0, counter()).expect("write") {
            assert_eq!(mode(&path), 0o644, "{}", path.display());
        }
        assert_eq!(
            entries(&dir.0),
            [
                DATABASE_ENV_FILE,
                POSTGRES_PASSWORD_FILE,
                RUSTIC_PASSWORD_FILE
            ]
        );
    }

    #[test]
    fn refuses_an_existing_directory_other_users_can_enter() {
        let dir = TempDir::new("open-dir");
        fs::create_dir_all(&dir.0).expect("create dir");
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).expect("chmod");
        let error = write(&dir.0, counter()).expect_err("must refuse");
        assert!(
            matches!(&error, Error::OpenDirectory { path, mode: 0o755 } if path == &dir.0),
            "{error}"
        );
        assert!(entries(&dir.0).is_empty());
    }

    #[test]
    fn hard_link_failure_names_the_filesystem() {
        let path = Path::new("/secrets/database.env");
        let error = link_error(path, std::io::Error::from(ErrorKind::Unsupported));
        let message = error.to_string();
        assert!(message.contains("hard link"), "{message}");
        assert!(message.contains("filesystem"), "{message}");
        assert!(message.contains("/secrets/database.env"), "{message}");
        assert!(matches!(
            link_error(path, std::io::Error::from(ErrorKind::AlreadyExists)),
            Error::Exists(_)
        ));
    }

    #[test]
    fn accepts_an_existing_private_directory() {
        let dir = TempDir::new("private-dir");
        private_dir(&dir.0);
        write(&dir.0, counter()).expect("write");
        assert_eq!(mode(&dir.0), 0o700);
    }

    #[test]
    fn a_failed_write_leaves_no_files_behind() {
        let dir = TempDir::new("partial");
        private_dir(&dir.0);
        let stale = dir.0.join(format!(".{DATABASE_ENV_FILE}.tmp"));
        fs::write(&stale, "stale").expect("write stale temp file");
        let error = write(&dir.0, counter()).expect_err("must fail");
        assert!(
            matches!(&error, Error::Exists(path) if path == &stale),
            "{error}"
        );
        assert_eq!(entries(&dir.0), [format!(".{DATABASE_ENV_FILE}.tmp")]);
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
