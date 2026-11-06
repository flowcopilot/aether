use std::fmt;

use super::protocol::name_number_digits;

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FullName {
    name: String,
    number: u16,
}

impl FullName {
    #[must_use]
    pub fn parse(full_name: &str) -> Option<Self> {
        let (name, number) = full_name.split_once('#')?;
        if number.len() != name_number_digits()
            || !number.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let number = number.parse::<u16>().ok()?;
        if number == 0 || !valid_name_part(name) {
            return None;
        }
        Some(Self { name: name.to_owned(), number })
    }

    #[must_use]
    pub fn name_part(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn number(&self) -> u16 {
        self.number
    }

    #[must_use]
    pub fn path_under(&self, base: &str) -> String {
        format!("{base}/{}/{:04}", self.name, self.number)
    }
}

impl fmt::Display for FullName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}#{:04}", self.name, self.number)
    }
}

#[must_use]
pub fn valid_name_part(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=32).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte)
        })
}

#[must_use]
pub fn valid_full_name(full_name: &str) -> bool {
    FullName::parse(full_name).is_some()
}

#[must_use]
pub fn split_full_name(full_name: &str) -> Option<(&str, u16)> {
    let (name, number) = full_name.split_once('#')?;
    if number.len() != name_number_digits()
        || !number.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let number = number.parse::<u16>().ok()?;
    (number != 0 && valid_name_part(name)).then_some((name, number))
}

#[cfg(test)]
mod tests {
    use super::{FullName, split_full_name, valid_full_name, valid_name_part};

    #[test]
    fn a_full_name_splits_into_name_and_number() {
        assert_eq!(split_full_name("ana#0042"), Some(("ana", 42)));
        assert_eq!(split_full_name("a.b-c#9999"), Some(("a.b-c", 9999)));
        for bad in
            ["ana", "ana#42", "ana#00042", "ana#0000", "Ana#0042", "ana#00a2", "#0042"]
        {
            assert_eq!(split_full_name(bad), None, "{bad}");
            assert!(!valid_full_name(bad));
        }
    }

    #[test]
    fn a_name_part_is_short_lowercase_and_starts_with_a_letter_or_digit() {
        assert!(valid_name_part("bob"));
        assert!(valid_name_part("0x.b_o-b"));
        for name in ["", "Bob", "-bob", "bob smith", "bob#0042", &"b".repeat(33)] {
            assert!(!valid_name_part(name), "{name}");
        }
    }

    #[test]
    fn display_round_trips() {
        let parsed = FullName::parse("law#0042").expect("valid");
        assert_eq!(parsed.to_string(), "law#0042");
    }
}
