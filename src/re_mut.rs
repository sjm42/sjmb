// re_acl.rs

use log::*;
use regex::Regex;

#[derive(Debug, Clone)]
pub struct ReMut {
    pub re_str: Vec<(String, String)>,
    pub re_vec: Vec<Regex>,
}
impl ReMut {
    pub fn new(list: &Vec<(String, String)>) -> anyhow::Result<Self> {
        info!("Got {} entries.", list.len());
        debug!("New ReMut:\n{list:#?}");

        // precompile every regex and save them
        let mut re_str = Vec::with_capacity(list.len());
        let mut re_vec = Vec::with_capacity(list.len());
        for (s, r) in list {
            re_str.push((s.to_owned(), r.to_owned()));
            re_vec.push(Regex::new(s)?);
        }
        Ok(Self { re_str, re_vec })
    }
    pub fn re_match<S>(&self, text: S) -> Option<(usize, String)>
    where
        S: AsRef<str>,
    {
        for (i, re) in self.re_vec.iter().enumerate() {
            if re.is_match(text.as_ref()) {
                // return index of match along with the mutated string
                return Some((i, re.replace(text.as_ref(), &self.re_str[i].1).to_string()));
            }
        }
        None
    }
}

// EOF
