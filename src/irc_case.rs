// irc_case.rs

//! IRC identifier equality, as advertised by the server's CASEMAPPING token.

use crate::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseMapping {
    Ascii,
    StrictRfc1459,
    #[default]
    Rfc1459,
}

impl CaseMapping {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "ascii" => Ok(Self::Ascii),
            "strict-rfc1459" => Ok(Self::StrictRfc1459),
            "rfc1459" => Ok(Self::Rfc1459),
            _ => bail!("unsupported IRC CASEMAPPING: {value}"),
        }
    }

    fn fold_char(self, ch: char) -> char {
        match ch {
            '[' if self != Self::Ascii => '{',
            ']' if self != Self::Ascii => '}',
            '\\' if self != Self::Ascii => '|',
            '^' if self == Self::Rfc1459 => '~',
            _ => ch.to_ascii_lowercase(),
        }
    }

    pub fn fold(self, value: &str) -> String {
        value.chars().map(|ch| self.fold_char(ch)).collect()
    }

    pub fn eq(self, left: &str, right: &str) -> bool {
        left.chars()
            .map(|ch| self.fold_char(ch))
            .eq(right.chars().map(|ch| self.fold_char(ch)))
    }
}

/// Reject ambiguous configuration rather than selecting a random HashMap entry.
pub fn get_irc<'a, T>(map: &'a HashMap<String, T>, key: &str, mapping: CaseMapping) -> anyhow::Result<Option<&'a T>> {
    let mut matches = map.iter().filter(|(name, _)| mapping.eq(name, key));
    let value = matches.next().map(|(_, value)| value);
    if matches.next().is_some() {
        bail!("ambiguous IRC configuration keys matching {key}");
    }
    Ok(value)
}

pub fn get_irc_wild<'a, T>(
    map: &'a HashMap<String, T>,
    key: &str,
    mapping: CaseMapping,
) -> anyhow::Result<Option<&'a T>> {
    Ok(get_irc(map, key, mapping)?.or_else(|| map.get("*")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implements_all_three_irc_case_mappings() {
        assert!(CaseMapping::Ascii.eq("Alice", "ALICE"));
        assert!(!CaseMapping::Ascii.eq("Nick[", "nick{"));
        assert!(CaseMapping::StrictRfc1459.eq("Nick[\\]", "nick{|}"));
        assert!(!CaseMapping::StrictRfc1459.eq("Nick^", "nick~"));
        assert!(CaseMapping::Rfc1459.eq("Nick^", "nick~"));
    }

    #[test]
    fn channel_fallbacks_and_ambiguous_names_are_checked() {
        let mut map = HashMap::from([("#Test".into(), true), ("*".into(), false)]);
        assert_eq!(get_irc_wild(&map, "#TEST", CaseMapping::Ascii).unwrap(), Some(&true));
        assert_eq!(get_irc_wild(&map, "#other", CaseMapping::Ascii).unwrap(), Some(&false));
        map.insert("#test".into(), false);
        assert!(get_irc_wild(&map, "#TEST", CaseMapping::Ascii).is_err());
    }
}
// EOF
