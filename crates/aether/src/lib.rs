use std::{
    fs,
    path::{Path, PathBuf},
};

mod access_groups;
pub mod access_requests;
pub mod admin;
mod admin_components;
pub mod admin_socket;
pub mod admin_ui;
pub mod agent;
pub mod agent_card;
// Registry components ship whole (`topcoat ui add`); a page may use only
// part of one, such as a table without its caption. They are vendored code:
// `topcoat ui` compares them by hash, so they are kept as the registry
// ships them rather than rewritten for this project's stricter lint groups.
#[allow(dead_code, clippy::pedantic, clippy::nursery)]
mod components;
pub mod control_service;
pub mod data_dir;
pub mod direct;
pub mod directory_client;
pub mod directory_publication;
pub mod discovery;
pub mod execution;
pub mod fixture;
mod https;
pub mod hub_client;
mod hub_membership;
mod hub_sync;
pub mod identity;
mod key_file;
pub mod listing;
pub mod managed_client;
pub mod mcp;
pub mod node_config;
pub mod node_service;
pub mod outbox;
pub mod pact;
mod pinned_https;
pub mod proof;
pub mod transport;
pub mod untrusted;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Command {
    Node,
    Control,
    Mcp,
}

impl Command {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Control => "control",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Config {
    pub data_dir: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct UserError {
    message: String,
}

impl UserError {
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for UserError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UserError {}

pub fn load_config(path: Option<&Path>, command: Command) -> Result<Config, UserError> {
    let default_data_dir = PathBuf::from(format!("./.aether/{}", command.name()));
    let Some(path) = path else {
        return Ok(Config { data_dir: default_data_dir });
    };

    let source = fs::read_to_string(path).map_err(|_| UserError {
        message: "configuration file cannot be read".to_owned(),
    })?;
    let file: ConfigFile = toml::from_str(&source).map_err(|_| UserError {
        message: "configuration must contain data_dir = \"path\"".to_owned(),
    })?;
    if file.data_dir.is_empty() || file.data_dir.contains('\0') {
        return Err(UserError {
            message: "configuration data_dir must be a non-empty path".to_owned(),
        });
    }

    Ok(Config { data_dir: PathBuf::from(file.data_dir) })
}

/// The configuration file. `data_dir` is its only setting.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    data_dir: String,
}

#[must_use]
pub fn startup_log(command: Command, _config: &Config) -> String {
    serde_json::json!({
        "level": "info",
        "event": "process_ready",
        "command": command.name(),
        "protocol": aether_core::PROTOCOL_FAMILY,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::{Command, Config, load_config, startup_log};
    use std::{fs, path::PathBuf};

    #[test]
    fn rejects_an_unreadable_configuration_file_without_its_path() {
        let error = load_config(
            Some(std::path::Path::new("missing-aether.toml")),
            Command::Node,
        )
        .expect_err("the missing file must fail");
        assert_eq!(error.message(), "configuration file cannot be read");
    }

    #[test]
    fn reads_the_setting_as_toml() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join("aether.toml");
        fs::write(&path, "# node state\ndata_dir='state dir'  # local\n")
            .expect("the test config must be written");
        let config =
            load_config(Some(&path), Command::Node).expect("the config must load");
        assert_eq!(config.data_dir, PathBuf::from("state dir"));
    }

    #[test]
    fn refuses_an_unknown_setting_and_an_empty_path() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = directory.path().join("aether.toml");
        for (source, message) in [
            (
                "data_dir = \"state\"\nport = 1",
                "configuration must contain data_dir = \"path\"",
            ),
            ("port = 1", "configuration must contain data_dir = \"path\""),
            ("data_dir = \"\"", "configuration data_dir must be a non-empty path"),
        ] {
            fs::write(&path, source).expect("the test config must be written");
            let error = load_config(Some(&path), Command::Node)
                .expect_err("the config must be refused");
            assert_eq!(error.message(), message, "{source}");
        }
    }

    #[test]
    fn startup_log_does_not_include_a_data_directory() {
        let config = Config { data_dir: PathBuf::from("private-workspace") };
        let log = startup_log(Command::Node, &config);
        assert!(log.contains("\"command\":\"node\""));
        assert!(!log.contains("private-workspace"));
    }
}
