// web_util.rs

use hyper::StatusCode;
use log::*;
use std::{sync::Arc, time::Duration};
use url::Url;

const MAX_REDIR_COUNT: i32 = 3;
const HTTP_TIMEOUT: u64 = 5;

pub async fn get_http_body<S>(url_s: S) -> anyhow::Result<Option<String>>
where
    S: AsRef<str>,
{
    mod danger {
        use rustls::client::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
        use rustls::{Certificate, DigitallySignedStruct, Error, ServerName};
        use std::time::SystemTime;

        pub struct NoCertificateVerification {}
        impl ServerCertVerifier for NoCertificateVerification {
            fn verify_server_cert(
                &self,
                _end_entity: &Certificate,
                _intermediates: &[Certificate],
                _server_name: &ServerName,
                _scts: &mut dyn Iterator<Item = &[u8]>,
                _oscp_response: &[u8],
                _now: SystemTime,
            ) -> Result<ServerCertVerified, Error> {
                Ok(ServerCertVerified::assertion())
            }

            fn verify_tls12_signature(
                &self,
                _message: &[u8],
                _cert: &Certificate,
                _dss: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, Error> {
                Ok(HandshakeSignatureValid::assertion())
            }

            fn verify_tls13_signature(
                &self,
                _message: &[u8],
                _cert: &Certificate,
                _dss: &DigitallySignedStruct,
            ) -> Result<HandshakeSignatureValid, Error> {
                Ok(HandshakeSignatureValid::assertion())
            }
        }
    }

    let tls_config = rustls::ClientConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_safe_default_protocol_versions()?
        .with_custom_certificate_verifier(Arc::new(danger::NoCertificateVerification {}))
        .with_no_client_auth();

    // for snooping TLS we can use:
    // tls_config.key_log = Arc::new(rustls::KeyLogFile::new());

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_or_http()
        .enable_http1()
        .build();
    let client = hyper::Client::builder()
        .set_host(true)
        .build::<_, hyper::Body>(https);

    // We want a normalized and valid url, IDN handled etc.
    let mut url = Url::parse(url_s.as_ref())?;
    let mut redir_count = 0;
    let mut status;
    let resp = loop {
        let url_c = url.to_string();
        info!("Fetching URL: {url_c:#?}");

        let req = hyper::Request::builder()
            .uri(url_c)
            // .header(hyper::header::HOST, url.host_str().unwrap_or("none"))
            .header(
                hyper::header::USER_AGENT,
                format!(
                    "Rust/hyper/{} v{}",
                    env!("CARGO_PKG_NAME"),
                    env!("CARGO_PKG_VERSION")
                ),
            )
            .header(hyper::header::CONNECTION, "close")
            .body(hyper::Body::empty())?;

        let resp =
            tokio::time::timeout(Duration::from_secs(HTTP_TIMEOUT), client.request(req)).await??;
        debug!("Got response:\n{resp:#?}");
        status = resp.status();
        // handle HTTP redirects gracefully, start over with new url
        match status {
            StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT => {
                if redir_count < MAX_REDIR_COUNT {
                    if let Some(new_url) = resp.headers().get("location") {
                        let new_url_s = new_url.to_str()?;
                        url = Url::parse(new_url_s)?;
                        redir_count += 1;
                        info!("Redirect #{redir_count} --> {new_url_s}");
                        continue;
                    }
                } else {
                    return Err(anyhow::anyhow!("HTTP redirect count exceeded"));
                }
            }
            _ => {
                break resp;
            }
        }
    };

    if let hyper::StatusCode::OK = status {
        if let Some(ct) = resp.headers().get("content-type") {
            let ct_s = String::from_utf8_lossy(ct.as_bytes());
            if ct_s.starts_with("text/html") {
                let body = String::from_utf8_lossy(
                    hyper::body::to_bytes(resp.into_body())
                        .await?
                        .to_vec()
                        .as_ref(),
                )
                .to_string();
                Ok(Some(body))
            } else {
                debug!("Content-type ignored: {ct_s:?}");
                Ok(None)
            }
        } else {
            error!("No content-type!");
            Ok(None)
        }
    } else {
        Err(anyhow::anyhow!("HTTP status: {status:?}"))
    }
}

// EOF
