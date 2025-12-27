// re_acl.rs

use crate::*;

#[derive(Debug, Clone)]
pub struct ReAcl {
    pub acl_str: Vec<String>,
    pub acl_re: Vec<Regex>,
}

impl ReAcl {
    pub fn new(list: &Vec<String>) -> anyhow::Result<Self> {
        info!("Got {} entries.", list.len());
        debug!("New ReAcl:\n{list:#?}");

        // precompile every regex and save them
        let mut acl_str = Vec::with_capacity(list.len());
        let mut acl_re = Vec::with_capacity(list.len());
        for s in list {
            acl_str.push(s.to_owned());
            acl_re.push(Regex::new(s)?);
        }
        Ok(Self { acl_str, acl_re })
    }
    pub fn re_match(&self, text: &str) -> Option<(usize, String)> {
        for (i, re) in self.acl_re.iter().enumerate() {
            if re.is_match(text) {
                // return index of match along with the matched regex string
                return Some((i, self.acl_str[i].to_string()));
            }
        }
        None
    }
}
// EOF
