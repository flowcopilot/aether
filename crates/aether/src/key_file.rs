//! Files that hold private keys.

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

/// Why a 32-byte key file could not be loaded or created. Each caller maps
/// these to its own error.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum KeyFileError {
    /// The file exists but cannot be opened, for example because it is a
    /// symbolic link.
    Unreadable,
    /// The file does not hold exactly 32 bytes.
    WrongSize,
    /// Reading, writing, restricting or generating the key failed.
    Io,
}

/// Loads the 32-byte key at `path`, or creates it owner-only with
/// `generate`. A symbolic link is refused. When another process creates the
/// file first, this loads that key instead of overwriting it.
pub fn load_or_create(
    path: &Path,
    mut generate: impl FnMut() -> io::Result<[u8; 32]>,
) -> Result<[u8; 32], KeyFileError> {
    for _ in 0..2 {
        match open_existing(path) {
            Ok(file) => return read_key(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let key = generate().map_err(|_| KeyFileError::Io)?;
                match create_owner_only(path) {
                    Ok(mut file) => {
                        file.write_all(&key).map_err(|_| KeyFileError::Io)?;
                        file.sync_all().map_err(|_| KeyFileError::Io)?;
                        return Ok(key);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err(KeyFileError::Io),
                }
            }
            Err(_) => return Err(KeyFileError::Unreadable),
        }
    }
    Err(KeyFileError::Unreadable)
}

fn read_key(mut file: fs::File) -> Result<[u8; 32], KeyFileError> {
    set_owner_only(&file).map_err(|_| KeyFileError::Io)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|_| KeyFileError::Io)?;
    bytes.try_into().map_err(|_| KeyFileError::WrongSize)
}

#[cfg(unix)]
fn open_existing(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
}

#[cfg(not(unix))]
fn open_existing(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().read(true).open(path)
}

#[cfg(unix)]
fn set_owner_only(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

/// Replaces `path` with `bytes`, readable by the owner only from the moment
/// the file exists. The bytes go to a new sibling file created with mode
/// `0600`, which is then renamed over `path`. A write followed by `chmod`
/// would leave the key readable under the umask until the `chmod`.
pub fn write_owner_only(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = temporary_path(path)?;
    match fs::remove_file(&temporary) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut file = create_owner_only(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
}

fn temporary_path(path: &Path) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?
        .to_os_string();
    name.push(".tmp");
    Ok(path.with_file_name(name))
}

/// Loads the DER certificate `cert_name` and the PKCS#8 DER key `key_name`
/// from `dir`. When one of the two files does not exist, this makes a new
/// self-signed certificate for `sans` and writes both files; the key is
/// readable by the owner only. Returns `(certificate, key)`.
pub fn self_signed_cert(
    dir: &Path,
    cert_name: &str,
    key_name: &str,
    sans: &[&str],
) -> io::Result<(Vec<u8>, Vec<u8>)> {
    fs::create_dir_all(dir)?;
    let certificate_path = dir.join(cert_name);
    let key_path = dir.join(key_name);
    if certificate_path.exists() && key_path.exists() {
        return Ok((fs::read(certificate_path)?, fs::read(key_path)?));
    }
    let params = rcgen::CertificateParams::new(
        sans.iter().map(|&san| san.to_owned()).collect::<Vec<_>>(),
    )
    .map_err(io::Error::other)?;
    let key = rcgen::KeyPair::generate().map_err(io::Error::other)?;
    let certificate = params.self_signed(&key).map_err(io::Error::other)?;
    let certificate_bytes = certificate.der().to_vec();
    let key_bytes = key.serialize_der();
    fs::write(&certificate_path, &certificate_bytes)?;
    write_owner_only(&key_path, &key_bytes)?;
    Ok((certificate_bytes, key_bytes))
}

/// `create_new` also refuses a symbolic link at `path`.
#[cfg(unix)]
fn create_owner_only(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

#[cfg(not(unix))]
fn create_owner_only(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{KeyFileError, load_or_create, write_owner_only};

    #[cfg(unix)]
    fn mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        fs::metadata(path).expect("file must exist").permissions().mode() & 0o777
    }

    #[test]
    fn creates_a_key_once_and_loads_it_after() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join("key");
        let created = load_or_create(&path, || Ok([7; 32])).expect("key must create");
        assert_eq!(created, [7; 32]);
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o600);
        let loaded = load_or_create(&path, || Ok([9; 32])).expect("key must load");
        assert_eq!(loaded, [7; 32]);
    }

    #[test]
    fn refuses_a_key_of_the_wrong_size() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join("key");
        fs::write(&path, [1; 31]).expect("short key must write");
        assert_eq!(load_or_create(&path, || Ok([7; 32])), Err(KeyFileError::WrongSize));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symbolic_link() {
        let directory = tempfile::tempdir().expect("directory must create");
        let target = directory.path().join("target");
        fs::write(&target, [1; 32]).expect("target must write");
        let path = directory.path().join("key");
        std::os::unix::fs::symlink(&target, &path).expect("link must create");
        assert_eq!(
            load_or_create(&path, || Ok([7; 32])),
            Err(KeyFileError::Unreadable)
        );
    }

    #[test]
    fn replaces_a_readable_key_and_a_stale_temporary_file() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join("key.der");
        fs::write(&path, b"old").expect("old key must write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
                .expect("old key mode must set");
        }
        fs::write(directory.path().join("key.der.tmp"), b"stale")
            .expect("stale file must write");
        write_owner_only(&path, b"new").expect("key must replace");
        assert_eq!(fs::read(&path).expect("key must read"), b"new");
        #[cfg(unix)]
        assert_eq!(mode(&path), 0o600);
        assert!(!directory.path().join("key.der.tmp").exists());
    }
}
