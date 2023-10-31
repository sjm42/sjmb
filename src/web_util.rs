// web_util.rs

use log::*;
use std::sync::Arc;
use url::Url;

pub async fn get_url_body<S>(url: S) -> anyhow::Result<Option<String>>
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

    // We want a normalized and valid url, IDN handled etc.
    let url_c = String::from(Url::parse(url.as_ref())?);
    info!("Fetching URL: {url_c:#?}");

    let mut tls_config = rustls::ClientConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_safe_default_protocol_versions()?
        .with_custom_certificate_verifier(Arc::new(danger::NoCertificateVerification {}))
        .with_no_client_auth();
    tls_config.key_log = Arc::new(rustls::KeyLogFile::new());

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_or_http()
        .enable_http1()
        .build();

    let client = hyper::Client::builder().build::<_, hyper::Body>(https);
    let resp = client.get(url_c.parse()?).await?;
    debug!("Got response:\n{resp:#?}");
    let status = resp.status();
    if let hyper::StatusCode::OK = status {
        if let Some(ct) = resp.headers().get("content-type") {
            let ct_s = std::str::from_utf8(ct.as_bytes())?;
            if ct_s == "text/html" {
                let body =
                    String::from_utf8(hyper::body::to_bytes(resp.into_body()).await?.to_vec())?;
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
