//! The files of a node or control data directory.
//!
//! Every file name in a data directory is written here once. A caller asks
//! for the file it needs (a key, a store, a socket) and never joins a name
//! itself.

use std::{
    io,
    path::{Path, PathBuf},
};

use aether_store::{NodeStore, StoreError};
use ed25519_dalek::SigningKey;

pub use crate::key_file::KeyFileError;

/// The agent socket a connected agent attaches to. The agent-socket
/// contract in `protocol/agent-socket` names the same file.
pub const AGENT_SOCKET: &str = "agent.sock";

/// The login file in a Codex home.
const CODEX_LOGIN: &str = "auth.json";

/// A node or control data directory.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Creates the directory when it does not exist.
    ///
    /// # Errors
    ///
    /// The I/O error of the creation.
    pub fn create(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }

    /// The node store, in a directory created when absent.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the directory or the store cannot open.
    pub fn store(&self) -> Result<NodeStore, StoreError> {
        self.create()?;
        NodeStore::in_dir(&self.root)
    }

    /// The node's iroh endpoint key, created on first use.
    ///
    /// # Errors
    ///
    /// A [`KeyFileError`] when the key cannot load or be created.
    pub fn endpoint_key(&self) -> Result<iroh::SecretKey, KeyFileError> {
        let bytes =
            self.key("endpoint.key", || Ok(iroh::SecretKey::generate().to_bytes()))?;
        Ok(iroh::SecretKey::from_bytes(&bytes))
    }

    /// The key that signs this node's invitations, directory records and
    /// Agent Cards, created on first use.
    ///
    /// # Errors
    ///
    /// A [`KeyFileError`] when the key cannot load or be created.
    pub fn discovery_key(&self) -> Result<SigningKey, KeyFileError> {
        self.signing_key("discovery.key")
    }

    /// The key that signs the grants this node gives, created on first use.
    ///
    /// # Errors
    ///
    /// A [`KeyFileError`] when the key cannot load or be created.
    pub fn issuer_key(&self) -> Result<SigningKey, KeyFileError> {
        self.signing_key("issuer.key")
    }

    /// The Codex home aether owns for this node.
    #[must_use]
    pub fn codex_home(&self) -> PathBuf {
        self.root.join("codex-home")
    }

    #[must_use]
    pub fn agent_socket(&self) -> PathBuf {
        self.root.join(AGENT_SOCKET)
    }

    #[must_use]
    pub fn admin_socket(&self) -> PathBuf {
        self.root.join("admin.sock")
    }

    /// The task folder root of a node created without a policy file: a
    /// `tasks` folder beside the data directory.
    #[must_use]
    pub fn default_tasks_root(&self) -> PathBuf {
        self.root.parent().unwrap_or(&self.root).join("tasks")
    }

    /// The self-signed certificate and key of the admin page, made on first
    /// use. Returns `(certificate, key)`.
    ///
    /// # Errors
    ///
    /// The I/O error of reading or writing either file.
    pub fn admin_tls(&self) -> io::Result<(Vec<u8>, Vec<u8>)> {
        crate::key_file::self_signed_cert(
            &self.root,
            "admin-cert.der",
            "admin-key.der",
            &["localhost", "127.0.0.1"],
        )
    }

    #[must_use]
    pub fn control_store(&self) -> PathBuf {
        self.root.join("control.sqlite")
    }

    /// The certificate a directory client pins to reach this control
    /// service.
    #[must_use]
    pub fn control_certificate(&self) -> PathBuf {
        self.root.join("control-cert.der")
    }

    /// The self-signed certificate and key of the control service, made on
    /// first use. Returns `(certificate, key)`.
    ///
    /// # Errors
    ///
    /// The I/O error of reading or writing either file.
    pub fn control_tls(&self) -> io::Result<(Vec<u8>, Vec<u8>)> {
        crate::key_file::self_signed_cert(
            &self.root,
            "control-cert.der",
            "control-key.der",
            &["localhost", "127.0.0.1", "control"],
        )
    }

    fn signing_key(&self, name: &str) -> Result<SigningKey, KeyFileError> {
        let bytes = self.key(name, || {
            let mut bytes = [0; 32];
            getrandom::fill(&mut bytes).map_err(io::Error::other)?;
            Ok(bytes)
        })?;
        Ok(SigningKey::from_bytes(&bytes))
    }

    fn key(
        &self,
        name: &str,
        generate: impl FnMut() -> io::Result<[u8; 32]>,
    ) -> Result<[u8; 32], KeyFileError> {
        self.create().map_err(|_| KeyFileError::Io)?;
        crate::key_file::load_or_create(&self.root.join(name), generate)
    }
}

impl From<&Path> for DataDir {
    fn from(root: &Path) -> Self {
        Self::new(root)
    }
}

/// The node data directory when `--data-dir` is not given: trimmed
/// `$AETHER_DATA_DIR` when it is not blank, else `home/.aether/host`.
#[must_use]
pub fn default_node_data_dir(
    explicit: Option<PathBuf>,
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
    home: Option<PathBuf>,
) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }
    if let Some(value) =
        env("AETHER_DATA_DIR").and_then(|value| value.into_string().ok())
    {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Some(home) = home {
        home.join(".aether").join("host")
    } else {
        PathBuf::from(".aether/host")
    }
}

/// The login file in the Codex home `codex_home`.
#[must_use]
pub fn codex_login(codex_home: &Path) -> PathBuf {
    codex_home.join(CODEX_LOGIN)
}

#[cfg(test)]
mod tests {
    use super::{DataDir, default_node_data_dir};
    use std::path::PathBuf;

    #[test]
    fn default_node_data_dir_follows_the_agent_socket_contract() {
        let home = PathBuf::from("/home/ana");
        let env = |key: &str| match key {
            "AETHER_DATA_DIR" => Some(std::ffi::OsString::from("/srv/aether")),
            _ => None,
        };
        assert_eq!(
            default_node_data_dir(None, env, Some(home.clone())),
            PathBuf::from("/srv/aether")
        );
        let env = |key: &str| match key {
            "AETHER_DATA_DIR" => Some(std::ffi::OsString::from("  /srv/aether  ")),
            _ => None,
        };
        assert_eq!(
            default_node_data_dir(None, env, Some(home.clone())),
            PathBuf::from("/srv/aether")
        );
        let env = |_key: &str| None;
        assert_eq!(
            default_node_data_dir(None, env, Some(home.clone())),
            PathBuf::from("/home/ana/.aether/host")
        );
        let env = |key: &str| match key {
            "AETHER_DATA_DIR" => Some(std::ffi::OsString::from("   ")),
            _ => None,
        };
        assert_eq!(
            default_node_data_dir(None, env, Some(home)),
            PathBuf::from("/home/ana/.aether/host")
        );
        assert_eq!(
            default_node_data_dir(
                Some(PathBuf::from("/explicit")),
                |_key: &str| None,
                None
            ),
            PathBuf::from("/explicit")
        );
    }

    #[test]
    fn each_key_is_its_own_file_and_is_kept() {
        let directory = tempfile::tempdir().expect("directory must create");
        let data_dir = DataDir::new(directory.path().join("node"));
        let discovery = data_dir.discovery_key().expect("discovery key must create");
        let issuer = data_dir.issuer_key().expect("issuer key must create");
        let endpoint = data_dir.endpoint_key().expect("endpoint key must create");
        assert_ne!(discovery.to_bytes(), issuer.to_bytes());
        assert_eq!(
            data_dir.discovery_key().expect("discovery key must load").to_bytes(),
            discovery.to_bytes()
        );
        assert_eq!(
            data_dir.issuer_key().expect("issuer key must load").to_bytes(),
            issuer.to_bytes()
        );
        assert_eq!(
            data_dir.endpoint_key().expect("endpoint key must load").public(),
            endpoint.public()
        );
    }

    #[test]
    fn control_tls_replaces_a_key_left_without_its_certificate() {
        let directory = tempfile::tempdir().expect("directory must create");
        let key_path = directory.path().join("control-key.der");
        std::fs::write(&key_path, b"orphaned").expect("orphaned key must write");
        let data_dir = DataDir::new(directory.path());
        let (certificate, key) =
            data_dir.control_tls().expect("TLS material must generate");
        assert_ne!(key, b"orphaned");
        assert_eq!(std::fs::read(&key_path).expect("key must read"), key);
        assert_eq!(
            std::fs::read(data_dir.control_certificate())
                .expect("certificate must read"),
            certificate
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = std::fs::metadata(&key_path)
                .expect("key must exist")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
