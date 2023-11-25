// web_util.rs

use anyhow::anyhow;
use log::*;
use std::time::Duration;
use url::Url;

const CONN_TIMEOUT: u64 = 5;
const REQW_TIMEOUT: u64 = 10;

pub async fn get_text_body<S>(url_s: S) -> anyhow::Result<Option<(String, String)>>
where
    S: AsRef<str>,
{
    let (body, ct) = get_body(url_s.as_ref()).await?;

    if ct.starts_with("text/") {
        Ok(Some((body, ct)))
    } else {
        debug!("Content-type ignored: {ct:?}");
        Ok(None)
    }
}

pub async fn get_body<S>(url_s: S) -> anyhow::Result<(String, String)>
where
    S: AsRef<str>,
{
    // We want a normalized and valid url, IDN handled etc.
    let url = Url::parse(url_s.as_ref())?;

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

// EOF
