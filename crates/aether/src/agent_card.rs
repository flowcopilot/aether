//! The A2A Agent Card of this node's agent.
//!
//! The card is what a caller's agent knows of this agent. The owner writes
//! the text; the node writes the rest. The card names no MCP server, tool,
//! model, sandbox, folder or setting of the computer, and no provider or
//! build. It holds the endpoint ID, so it goes only into an invite, never
//! into a public listing.

use std::{collections::BTreeSet, path::Path};

use aether_store::NodeStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The most characters of the card description.
pub const MAX_DESCRIPTION: usize = 500;
/// The most characters of the skill name.
pub const MAX_SKILL_NAME: usize = 64;
/// The most characters of the skill description.
pub const MAX_SKILL_DESCRIPTION: usize = 300;
/// The most tags of the skill.
pub const MAX_TAGS: usize = 5;
/// The most characters of one tag, from `[a-z0-9-]`.
pub const MAX_TAG: usize = 24;
/// The most bytes of a card.
pub const MAX_CARD_BYTES: usize = 2048;
/// The A2A binding of an Aether node over Iroh.
pub const IROH_BINDING: &str = "urn:aether:a2a-binding:iroh:0";

const SETTINGS_KEY: &str = "node.agent_card";
const SECURITY_DESCRIPTION: &str = "Connect with the key that your invite names. Send the signed claim from the invite with each task.";
/// The longest agent name: 64 characters and a hub number.
const LONGEST_NAME: usize = 69;
const TOP_LEVEL_KEYS: [&str; 10] = [
    "capabilities",
    "defaultInputModes",
    "defaultOutputModes",
    "description",
    "name",
    "securityRequirements",
    "securitySchemes",
    "skills",
    "supportedInterfaces",
    "version",
];

#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum CardError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("the card settings cannot be stored")]
    Store,
}

/// The text of the card that the owner writes, and the card version.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CardSettings {
    pub description: String,
    pub skill_name: String,
    pub skill_description: String,
    pub tags: Vec<String>,
    pub version: u64,
}

impl Default for CardSettings {
    fn default() -> Self {
        Self {
            description: "An agent that does tasks for people its owner invites.".to_owned(),
            skill_name: "General tasks".to_owned(),
            skill_description:
                "Does one task that you describe in plain text and returns a plain-text answer."
                    .to_owned(),
            tags: vec![crate::execution::GENERAL_SKILL.to_owned()],
            version: 1,
        }
    }
}

/// The text that the owner sends to change the card.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CardInput {
    pub description: String,
    pub skill_name: String,
    pub skill_description: String,
    pub tags: Vec<String>,
}

/// The fields of a valid card that a page or a tool can show.
#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardView {
    pub name: String,
    pub description: String,
    pub skill_name: String,
    pub skill_description: String,
    pub tags: Vec<String>,
    pub version: String,
}

/// The card settings of `data_dir`, or the defaults.
#[must_use]
pub fn settings(data_dir: &Path) -> CardSettings {
    NodeStore::in_dir(data_dir)
        .ok()
        .and_then(|store| store.configuration(SETTINGS_KEY).ok())
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default()
}

/// Checks and keeps `input`, with the next card version.
pub fn save_settings(
    data_dir: &Path,
    input: &CardInput,
) -> Result<CardSettings, CardError> {
    let current = settings(data_dir);
    let next = CardSettings {
        description: input.description.trim().to_owned(),
        skill_name: input.skill_name.trim().to_owned(),
        skill_description: input.skill_description.trim().to_owned(),
        tags: input.tags.iter().map(|tag| tag.trim().to_owned()).collect(),
        version: current.version.saturating_add(1),
    };
    check_text(&next)?;
    // The longest name must still fit, so that every invite can hold it.
    let probe = compose(&next, &"a".repeat(LONGEST_NAME), &"0".repeat(64));
    if probe.len() > MAX_CARD_BYTES {
        return Err(CardError::Invalid("the card is too long"));
    }
    let value = serde_json::to_string(&next).map_err(|_| CardError::Store)?;
    NodeStore::in_dir(data_dir)
        .and_then(|store| store.put_configuration(SETTINGS_KEY, &value))
        .map_err(|_| CardError::Store)?;
    Ok(next)
}

/// The hub's text rule for a listing, so that the card text can be listed:
/// no control, bidirectional or zero-width characters.
fn valid_text(text: &str, max_chars: usize) -> bool {
    !text.is_empty()
        && text.chars().count() <= max_chars
        && !text.chars().any(|character| {
            character.is_control() || crate::untrusted::hidden(character)
        })
}

fn valid_tag(tag: &str) -> bool {
    (1..=MAX_TAG).contains(&tag.len())
        && tag.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
        })
}

fn check_text(settings: &CardSettings) -> Result<(), CardError> {
    if !valid_text(&settings.description, MAX_DESCRIPTION) {
        return Err(CardError::Invalid(
            "the description must have 1 to 500 characters",
        ));
    }
    if !valid_text(&settings.skill_name, MAX_SKILL_NAME) {
        return Err(CardError::Invalid("the skill name must have 1 to 64 characters"));
    }
    if !valid_text(&settings.skill_description, MAX_SKILL_DESCRIPTION) {
        return Err(CardError::Invalid(
            "the skill description must have 1 to 300 characters",
        ));
    }
    let unique: BTreeSet<&String> = settings.tags.iter().collect();
    if settings.tags.is_empty()
        || settings.tags.len() > MAX_TAGS
        || unique.len() != settings.tags.len()
        || !settings.tags.iter().all(|tag| valid_tag(tag))
    {
        return Err(CardError::Invalid(
            "use 1 to 5 different tags of lowercase letters, digits and dashes",
        ));
    }
    Ok(())
}

fn interface(endpoint_id: &str) -> Value {
    json!([{
        "url": format!("iroh://{endpoint_id}"),
        "protocolBinding": IROH_BINDING,
        "protocolVersion": "1.0",
    }])
}

fn capabilities() -> Value {
    json!({ "streaming": false, "pushNotifications": false, "extendedAgentCard": false })
}

fn security_schemes() -> Value {
    json!({ "aetherInvite": { "mtlsSecurityScheme": { "description": SECURITY_DESCRIPTION } } })
}

/// The security requirement in the shape of the A2A specification.
fn security_requirements() -> Value {
    json!([{ "schemes": { "aetherInvite": { "list": [] } } }])
}

/// The card of the agent `agent_name` at `endpoint_id`, as JSON bytes.
#[must_use]
pub fn compose(
    settings: &CardSettings,
    agent_name: &str,
    endpoint_id: &str,
) -> Vec<u8> {
    let card = json!({
        "name": agent_name,
        "description": settings.description,
        "version": settings.version.to_string(),
        "supportedInterfaces": interface(endpoint_id),
        "capabilities": capabilities(),
        "securitySchemes": security_schemes(),
        "securityRequirements": security_requirements(),
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [{
            "id": crate::execution::GENERAL_SKILL,
            "name": settings.skill_name,
            "description": settings.skill_description,
            "tags": settings.tags,
        }],
    });
    serde_json::to_vec(&card).unwrap_or_default()
}

/// Checks `card` for the agent `agent_name` at `endpoint_id` and returns
/// the fields that can be shown. Only the fields of [`compose`] pass.
pub fn validate(
    card: &[u8],
    agent_name: &str,
    endpoint_id: &str,
) -> Result<CardView, CardError> {
    let invalid = CardError::Invalid;
    if card.len() > MAX_CARD_BYTES {
        return Err(invalid("the card is too long"));
    }
    // A card that A2A tools cannot read is not an A2A card.
    serde_json::from_slice::<a2a::AgentCard>(card)
        .map_err(|_| invalid("not an A2A card"))?;
    let value: Value = serde_json::from_slice(card).map_err(|_| invalid("not JSON"))?;
    let object = value.as_object().ok_or(CardError::Invalid("not a JSON object"))?;
    let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    if keys != TOP_LEVEL_KEYS.into_iter().collect() {
        return Err(invalid("the card has other fields"));
    }
    let text = |key: &str| object.get(key).and_then(Value::as_str).unwrap_or_default();
    if text("name") != agent_name {
        return Err(invalid("the card names another agent"));
    }
    if object.get("supportedInterfaces") != Some(&interface(endpoint_id))
        || object.get("capabilities") != Some(&capabilities())
        || object.get("securitySchemes") != Some(&security_schemes())
        || object.get("securityRequirements") != Some(&security_requirements())
        || object.get("defaultInputModes") != Some(&json!(["text/plain"]))
        || object.get("defaultOutputModes") != Some(&json!(["text/plain"]))
    {
        return Err(invalid("the card has other protocol values"));
    }
    let version = text("version");
    if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("the card version is not a number"));
    }
    let [skill] = object
        .get("skills")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    else {
        return Err(invalid("the card must have one skill"));
    };
    let skill =
        skill.as_object().ok_or(CardError::Invalid("the skill is not an object"))?;
    let skill_keys: BTreeSet<&str> = skill.keys().map(String::as_str).collect();
    if skill_keys != ["description", "id", "name", "tags"].into_iter().collect()
        || skill.get("id").and_then(Value::as_str)
            != Some(crate::execution::GENERAL_SKILL)
    {
        return Err(invalid("the skill has other fields"));
    }
    let skill_text =
        |key: &str| skill.get(key).and_then(Value::as_str).unwrap_or_default();
    let tags: Vec<String> = skill
        .get("tags")
        .and_then(Value::as_array)
        .map(|tags| tags.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let settings = CardSettings {
        description: text("description").to_owned(),
        skill_name: skill_text("name").to_owned(),
        skill_description: skill_text("description").to_owned(),
        tags,
        version: 0,
    };
    check_text(&settings)?;
    Ok(CardView {
        name: agent_name.to_owned(),
        description: crate::untrusted::multi_line(
            &settings.description,
            MAX_DESCRIPTION * 4,
        ),
        skill_name: crate::untrusted::single_line(&settings.skill_name, MAX_SKILL_NAME),
        skill_description: crate::untrusted::multi_line(
            &settings.skill_description,
            MAX_SKILL_DESCRIPTION * 4,
        ),
        tags: settings.tags,
        version: version.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        CardInput, CardSettings, MAX_CARD_BYTES, compose, save_settings, settings,
        validate,
    };

    fn endpoint() -> String {
        iroh::SecretKey::from_bytes(&[7; 32]).public().to_string()
    }

    fn edited(card: &[u8], edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
        let mut value: serde_json::Value =
            serde_json::from_slice(card).expect("card is JSON");
        edit(&mut value);
        serde_json::to_vec(&value).expect("card must encode")
    }

    #[test]
    fn the_default_card_is_a_valid_a2a_card_with_one_general_skill() {
        let card = compose(&CardSettings::default(), "tudor#0042", &endpoint());
        assert!(card.len() <= MAX_CARD_BYTES);
        let parsed: a2a::AgentCard = serde_json::from_slice(&card).expect("A2A card");
        assert_eq!(parsed.skills.len(), 1);
        let view =
            validate(&card, "tudor#0042", &endpoint()).expect("the card must pass");
        assert_eq!(view.name, "tudor#0042");
        assert_eq!(view.skill_name, "General tasks");
        assert_eq!(view.tags, ["general"]);
        assert_eq!(view.version, "1");
    }

    #[test]
    fn validation_refuses_fields_outside_the_allowlist() {
        let card = compose(&CardSettings::default(), "tudor#0042", &endpoint());
        let refused = [
            edited(&card, |card| {
                card["provider"] = serde_json::json!({"organization": "Aether"});
            }),
            edited(&card, |card| {
                card["skills"][0]["examples"] = serde_json::json!(["x"]);
            }),
            edited(&card, |card| {
                card["capabilities"]["extensions"] = serde_json::json!([{"uri": "x"}]);
            }),
            edited(&card, |card| {
                card["defaultOutputModes"] = serde_json::json!(["text/html"]);
            }),
            edited(&card, |card| {
                let skill = card["skills"][0].clone();
                card["skills"] = serde_json::json!([skill.clone(), skill]);
            }),
        ];
        for card in refused {
            assert!(validate(&card, "tudor#0042", &endpoint()).is_err());
        }
        assert!(validate(&card, "mallory#0001", &endpoint()).is_err());
        let other = iroh::SecretKey::from_bytes(&[8; 32]).public().to_string();
        assert!(validate(&card, "tudor#0042", &other).is_err());
    }

    #[test]
    fn the_card_keeps_the_spec_security_requirement_shape() {
        let card = compose(&CardSettings::default(), "tudor#0042", &endpoint());
        let value: serde_json::Value =
            serde_json::from_slice(&card).expect("card is JSON");
        assert_eq!(
            value["securityRequirements"],
            serde_json::json!([{ "schemes": { "aetherInvite": { "list": [] } } }])
        );
        assert_eq!(
            value["securitySchemes"]["aetherInvite"]["mtlsSecurityScheme"]["description"]
                .as_str()
                .map(|text| text.starts_with("Connect with the key")),
            Some(true)
        );
    }

    fn input(description: &str, tags: &[&str]) -> CardInput {
        CardInput {
            description: description.to_owned(),
            skill_name: "Legal questions".to_owned(),
            skill_description: "Answers one legal question in plain text.".to_owned(),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        }
    }

    #[test]
    fn saving_the_card_raises_its_version() {
        let directory = tempfile::tempdir().expect("directory must create");
        assert_eq!(settings(directory.path()).version, 1);
        let saved =
            save_settings(directory.path(), &input("  Legal help.  ", &["law"]))
                .expect("the card must save");
        assert_eq!((saved.version, saved.description.as_str()), (2, "Legal help."));
        let again = save_settings(
            directory.path(),
            &input("Legal help.", &["law", "contracts"]),
        )
        .expect("the card must save");
        assert_eq!(again.version, 3);
        assert_eq!(settings(directory.path()), again);
    }

    #[test]
    fn saving_refuses_empty_or_too_long_text() {
        let directory = tempfile::tempdir().expect("directory must create");
        let refused = [
            input("", &["law"]),
            input(&"a".repeat(501), &["law"]),
            input("bell \u{0007}", &["law"]),
            input("Legal help.", &[]),
            input("Legal help.", &["Bad Tag"]),
            input("Legal help.", &["a", "b", "c", "d", "e", "f"]),
            input("Legal help.", &["law", "law"]),
        ];
        for card in refused {
            assert!(save_settings(directory.path(), &card).is_err());
        }
        assert_eq!(settings(directory.path()).version, 1);
    }
}
