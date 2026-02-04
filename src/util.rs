// util.rs

use url::Url;

use crate::*;

const TS_FMT_LONG: &str = "%Y-%m-%d %H:%M:%S";
const TS_FMT_SHORT: &str = "%b %d %H:%M";
const TS_FMT_SHORT_YEAR: &str = "%Y %b %d %H:%M";
const TS_NONE: &str = "(none)";

pub fn ts_fmt(fmt: &str, ts: i64) -> String {
    if ts == 0 {
        TS_NONE.to_string()
    } else {
        DateTime::from_timestamp(ts, 0).map_or_else(|| TS_NONE.to_string(), |ts| ts.format(fmt).to_string())
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

const CONN_TIMEOUT: u64 = 5;
const REQW_TIMEOUT: u64 = 10;

pub async fn get_text_body(url_s: &str) -> anyhow::Result<Option<(String, String)>> {
    let (body, ct) = get_body(url_s).await?;

    if ct.starts_with("text/") {
        Ok(Some((body, ct)))
    } else {
        debug!("Content-type ignored: {ct:?}");
        Ok(None)
    }
}

pub async fn get_body(url_s: &str) -> anyhow::Result<(String, String)> {
    // We want a normalized and valid url, IDN handled etc.
    let url = Url::parse(url_s)?;

    let c = reqwest::ClientBuilder::new()
        .connect_timeout(Duration::from_secs(CONN_TIMEOUT))
        .timeout(Duration::from_secs(REQW_TIMEOUT))
        .user_agent(format!(
            "Rust/hyper/{} v{}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        ))
        .use_rustls_tls()
        .danger_accept_invalid_certs(true)
        .build()?;

    let resp = c.get(url).send().await?.error_for_status()?;
    let ct = String::from_utf8_lossy(
        resp.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .ok_or(anyhow!("No content-type in response"))?
            .as_bytes(),
    )
        .to_string();

    let body = resp.text().await?;
    Ok((body, ct))
}

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
    pub fn re_mut(&self, text: &str) -> Option<(usize, String)> {
        for (i, re) in self.re_vec.iter().enumerate() {
            if re.is_match(text) {
                // return index of match along with the mutated string
                return Some((i, re.replace(text, &self.re_str[i].1).to_string()));
            }
        }
        None
    }
}

// EOF
