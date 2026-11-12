//! The relay a node listens through: the owner's saved choice and the one
//! the running node uses.

use std::path::Path;

use aether_store::StoreError;

use super::TransportError;
use crate::settings::{Setting, store_setting};

const DEFAULT_RELAY_URL: &str = "https://aether-relay.tudorandrei.xyz";

/// The value that keeps a node on the local network, in the settings and
/// in the owner's input.
pub const RELAY_OFF: &str = "off";

/// Whether `url` is a relay URL an owner may choose: an `http` or `https`
/// URL with a host, in the form iroh reads.
#[must_use]
pub fn valid_relay_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|parsed| {
        matches!(parsed.scheme(), "https" | "http") && parsed.has_host()
    }) && url.parse::<iroh::RelayUrl>().is_ok()
}

/// Waits for `switch`, which moves a running node to `relay`, and then
/// keeps `relay` as the saved and the active relay.
///
/// # Errors
///
/// The error of `switch`, or [`TransportError::Io`] when the store fails.
pub async fn apply_relay(
    data_dir: &Path,
    relay: &Option<String>,
    switch: impl Future<Output = Result<(), TransportError>>,
) -> Result<(), TransportError> {
    switch.await?;
    store_setting::<Relay>(data_dir, relay)
        .and_then(|()| store_setting::<ActiveRelay>(data_dir, relay))
        .map_err(|_| TransportError::Io)
}

/// The owner's relay choice, kept for the next start. `None` keeps the
/// node on the local network. A new node has the deployed relay.
pub struct Relay;

impl Setting for Relay {
    const KEY: &'static str = "node.relay_url";
    type Value = Option<String>;

    fn absent() -> Result<Self::Value, StoreError> {
        Ok(Some(DEFAULT_RELAY_URL.to_owned()))
    }

    fn decode(stored: String) -> Result<Self::Value, StoreError> {
        Ok((stored != RELAY_OFF).then_some(stored))
    }

    fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
        Ok(Some(value.clone().unwrap_or_else(|| RELAY_OFF.to_owned())))
    }
}

/// The relay this running node uses for connect-code invites. The saved
/// [`Relay`] can differ when a command-line override or an MCP edit is
/// present.
pub struct ActiveRelay;

impl Setting for ActiveRelay {
    const KEY: &'static str = "node.runtime.relay_url";
    type Value = Option<String>;

    fn decode(stored: String) -> Result<Self::Value, StoreError> {
        Relay::decode(stored)
    }

    fn encode(value: &Self::Value) -> Result<Option<String>, StoreError> {
        Relay::encode(value)
    }
}

#[cfg(test)]
mod tests {
    use super::Relay;
    use crate::settings::{setting, store_setting};

    #[test]
    fn relay_uses_the_deployed_default_until_the_owner_changes_it() {
        let directory = tempfile::tempdir().expect("directory must create");
        assert_eq!(
            setting::<Relay>(directory.path()).ok().flatten().as_deref(),
            Some("https://aether-relay.tudorandrei.xyz")
        );

        store_setting::<Relay>(
            directory.path(),
            &Some("https://other.example.com".to_owned()),
        )
        .expect("relay choice must save");
        assert_eq!(
            setting::<Relay>(directory.path()).ok().flatten().as_deref(),
            Some("https://other.example.com")
        );

        store_setting::<Relay>(directory.path(), &None)
            .expect("local choice must save");
        assert_eq!(setting::<Relay>(directory.path()).ok().flatten(), None);
    }
}
