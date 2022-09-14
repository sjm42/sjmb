// re_acl.rs

use log::*;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{fs::File, io::BufReader};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ReAcl {
    pub acl: Vec<String>,
    #[serde(skip)]
    pub acl_re: Option<Vec<Regex>>,
}
impl ReAcl {
    pub fn new(file: &str) -> anyhow::Result<Self> {
        info!("Reading json acl file {file}");
        let mut acl: Self = serde_json::from_reader(BufReader::new(File::open(file)?))?;
        info!("Got {} entries.", acl.acl.len());
        debug!("New ReAcl:\n{acl:#?}");

        // precompile every regex and save them
        let mut re_vec = Vec::with_capacity(acl.acl.len());
        for s in &acl.acl {
            re_vec.push(Regex::new(s)?);
        }
        acl.acl_re = Some(re_vec);

        Ok(acl)
    }
    pub fn re_match<S>(&self, text: S) -> Option<(usize, String)>
    where
        S: AsRef<str>,
    {
        for (i, re) in self.acl_re.as_ref().unwrap().iter().enumerate() {
            if re.is_match(text.as_ref()) {
                // return index of match along with the matched regex string
                return Some((i, self.acl[i].to_string()));
            }
        }
        None
    }
}

// EOF
