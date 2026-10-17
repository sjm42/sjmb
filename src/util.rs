// util.rs

use crate::*;

const TS_FMT_LONG: &str = "%Y-%m-%d %H:%M:%S";
const TS_FMT_SHORT: &str = "%b %d %H:%M";
const TS_FMT_SHORT_YEAR: &str = "%Y %b %d %H:%M";
const TS_NONE: &str = "(none)";

pub fn ts_fmt(fmt: &str, ts: i64) -> String {
    if ts == 0 {
        TS_NONE.to_string()
    } else {
        DateTime::from_timestamp(ts, 0).map_or_else(
            || TS_NONE.to_string(),
            |ts| {
                let mut result = String::new();
                if write!(&mut result, "{}", ts.format(fmt)).is_err() {
                    return TS_NONE.to_string();
                }
                result
            },
        )
    }
}

pub trait TimeStampFormats {
    fn ts_long(self) -> String;
    fn ts_short(self) -> String;
    fn ts_short_y(self) -> String;
}

impl TimeStampFormats for i64 {
    fn ts_long(self) -> String {
        ts_fmt(TS_FMT_LONG, self)
    }

    fn ts_short(self) -> String {
        ts_fmt(TS_FMT_SHORT, self)
    }

    fn ts_short_y(self) -> String {
        ts_fmt(TS_FMT_SHORT_YEAR, self)
    }
}

pub trait CollapseWhiteSpace {
    fn ws_collapse(self) -> String;
}

impl CollapseWhiteSpace for &str {
    fn ws_collapse(self) -> String {
        self.split_whitespace().collect::<Vec<&str>>().join(" ")
    }
}

pub fn get_wild<'a, T>(map: &'a HashMap<String, T>, key: &str) -> Option<&'a T> {
    map.get(key).or_else(|| map.get("*"))
}

#[derive(Debug, Clone)]
pub struct ReAcl {
    rules: Vec<Regex>,
}

impl ReAcl {
    pub fn new(list: &[String]) -> anyhow::Result<Self> {
        let rules = list.iter().map(|s| Regex::new(s)).collect::<Result<_, _>>()?;
        Ok(Self { rules })
    }

    pub fn patterns(&self) -> impl Iterator<Item = &str> {
        self.rules.iter().map(Regex::as_str)
    }

    pub fn re_match(&self, text: &str) -> Option<(usize, String)> {
        self.rules
            .iter()
            .enumerate()
            .find(|(_, re)| re.is_match(text))
            .map(|(i, re)| (i, re.as_str().to_owned()))
    }
}

#[derive(Debug, Clone)]
pub struct ReMut {
    rules: Vec<(Regex, String)>,
}

impl ReMut {
    pub fn new(list: &[(String, String)]) -> anyhow::Result<Self> {
        let rules = list
            .iter()
            .map(|(pattern, replacement)| Ok((Regex::new(pattern)?, replacement.clone())))
            .collect::<anyhow::Result<_>>()?;
        Ok(Self { rules })
    }

    pub fn re_mut(&self, text: &str) -> Option<(usize, String)> {
        self.rules
            .iter()
            .enumerate()
            .find(|(_, (re, _))| re.is_match(text))
            .map(|(i, (re, replacement))| (i, re.replace(text, replacement).into_owned()))
    }
}

/// Whole-string wildcard masks: `*` matches any string; `?` matches one character.
pub fn wildcard_regex(mask: &str) -> anyhow::Result<Regex> {
    let mut pattern = String::from("\\A(?:");
    for ch in mask.chars() {
        match ch {
            '*' => pattern.push_str(".*"),
            '?' => pattern.push('.'),
            _ => pattern.push_str(&regex::escape(&ch.to_string())),
        }
    }
    pattern.push_str(")\\z");
    Ok(Regex::new(&pattern)?)
}

/// One bounded IRC payload, without embedded protocol/control characters.
pub fn irc_text(text: &str, max_bytes: usize) -> String {
    let mut result = String::new();
    let mut space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            space = !result.is_empty();
        } else if !ch.is_control() {
            if result.len() + ch.len_utf8() + usize::from(space) > max_bytes {
                break;
            }
            if space {
                result.push(' ');
                space = false;
            }
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_masks_are_anchored_and_escape_regex_characters() {
        let re = wildcard_regex("~?ser@*.example.com").unwrap();
        assert!(re.is_match("~user@host.example.com"));
        assert!(!re.is_match("~user@host.example.com.evil"));
        assert!(!wildcard_regex("u@a.b").unwrap().is_match("u@axb"));
    }

    #[test]
    fn output_bounds_unicode_and_removes_embedded_commands() {
        assert_eq!(irc_text("a\r\nPRIVMSG x :oops\u{1} ", 100), "a PRIVMSG x :oops");
        assert_eq!(irc_text("ééé", 5), "éé");
    }

    #[test]
    fn invalid_timestamp_formats_return_a_placeholder() {
        assert_eq!(ts_fmt("%", 1), "(none)");
        assert_eq!(ts_fmt("%Y", i64::MAX), "(none)");
    }
}
