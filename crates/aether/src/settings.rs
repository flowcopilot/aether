//! Node settings in the configuration table. Each feature defines its own
//! settings next to its code; this module only reads and writes them.

use std::path::Path;

use aether_store::{NodeStore, StoreError};

/// A node setting in the configuration table, with the type of its value.
pub trait Setting {
    /// The configuration key.
    const KEY: &'static str;
    type Value;

    /// The value when the key is not stored.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when the setting has no default.
    fn absent() -> Result<Self::Value, StoreError> {
        Err(StoreError::NotFound)
    }

    /// The value of the stored text.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the text is not a value.
    fn decode(stored: String) -> Result<Self::Value, StoreError>;

    /// The text to store for `value`; `None` removes the key.
    ///
    /// # Errors
    ///
    /// A [`StoreError`] when the value has no text form.
    fn encode(value: &Self::Value) -> Result<Option<String>, StoreError>;
}

/// The stored value of the setting `S`.
///
/// # Errors
///
/// A [`StoreError`] when the store fails, or as [`Setting::absent`] and
/// [`Setting::decode`] return.
pub fn setting<S: Setting>(data_dir: &Path) -> Result<S::Value, StoreError> {
    match NodeStore::in_dir(data_dir)?.configuration(S::KEY) {
        Ok(stored) => S::decode(stored),
        Err(StoreError::NotFound) => S::absent(),
        Err(error) => Err(error),
    }
}

/// Keeps `value` for the setting `S`.
///
/// # Errors
///
/// A [`StoreError`] when the store fails or the value has no text form.
pub fn store_setting<S: Setting>(
    data_dir: &Path,
    value: &S::Value,
) -> Result<(), StoreError> {
    let store = NodeStore::in_dir(data_dir)?;
    match S::encode(value)? {
        Some(text) => store.put_configuration(S::KEY, &text),
        None => store.delete_configuration(S::KEY),
    }
}
