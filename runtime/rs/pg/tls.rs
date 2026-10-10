//! TLS for the connection to the journal database, with rustls (the same
//! library the model and tool calls use).
//!
//! `sslmode` in the URL says what to do, as libpq does, except that Calyx
//! always checks the server's certificate when it uses TLS:
//!
//! - none, `disable`: no TLS (a database on a private network or behind a
//!   tunnel).
//! - `prefer`, `allow`: TLS if the server offers it, checked; plain text if
//!   it does not.
//! - `require`, `verify-full`: TLS or no connection; the certificate must
//!   chain to a trusted root and name the host.
//! - `verify-ca`: the same, without checking the host name (a server
//!   reached by an address its certificate does not name).
//!
//! The trusted roots are the system's, plus the CA in `sslrootcert=FILE`
//! (PEM) when it is given: a server whose certificate comes from your own
//! CA.

use std::sync::Arc;

use postgres::config::SslMode;
use postgres::{Client, Config, NoTls};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::Verifier;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    Disable,
    Prefer,
    Full,
    Ca,
}

/// The URL without `sslmode` and `sslrootcert` (which this module reads),
/// and their values.
fn split(url: &str) -> Result<(String, Mode, Option<String>), String> {
    let mut mode = Mode::Disable;
    let mut root = None;
    let mut take = |k: &str, v: &str| -> Result<bool, String> {
        match k {
            "sslmode" => {
                mode = match v {
                    "disable" => Mode::Disable,
                    "allow" | "prefer" => Mode::Prefer,
                    "require" | "verify-full" => Mode::Full,
                    "verify-ca" => Mode::Ca,
                    _ => return Err(format!("unknown sslmode `{v}`")),
                };
                Ok(true)
            }
            "sslrootcert" => {
                root = Some(v.to_owned());
                Ok(true)
            }
            _ => Ok(false),
        }
    };
    let rest = if let Some((base, query)) = url.split_once('?').filter(|_| url.contains("://")) {
        let mut kept = Vec::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            if !take(k, &decode(v))? {
                kept.push(pair);
            }
        }
        if kept.is_empty() {
            base.to_owned()
        } else {
            format!("{base}?{}", kept.join("&"))
        }
    } else if url.contains("://") {
        url.to_owned()
    } else {
        // `host=... sslmode=...`
        let mut kept = Vec::new();
        for pair in url.split_whitespace() {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            if !take(k, v.trim_matches('\''))? {
                kept.push(pair);
            }
        }
        kept.join(" ")
    };
    Ok((rest, mode, root))
}

/// `%2F` and the like, in a URL's query.
fn decode(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (
            b[i],
            b.get(i + 1).copied().and_then(hex),
            b.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An error with its causes: "error performing TLS handshake: invalid
/// peer certificate: UnknownIssuer", not only the first part.
fn why(e: &postgres::Error) -> String {
    let mut s = e.to_string();
    let mut cause = std::error::Error::source(e);
    while let Some(c) = cause {
        s = format!("{s}: {c}");
        cause = c.source();
    }
    s
}

/// Connects as `url` says, with TLS when its `sslmode` asks for it.
pub fn connect(url: &str) -> Result<Client, String> {
    let (rest, mode, root) = split(url)?;
    let mut config: Config = rest.parse().map_err(|e| format!("{e}"))?;
    if mode == Mode::Disable {
        config.ssl_mode(SslMode::Disable);
        return config.connect(NoTls).map_err(|e| why(&e));
    }
    config.ssl_mode(if mode == Mode::Prefer {
        SslMode::Prefer
    } else {
        SslMode::Require
    });
    let tls = tokio_postgres_rustls::MakeRustlsConnect::new(client_config(mode, root.as_deref())?);
    config.connect(tls).map_err(|e| why(&e))
}

fn client_config(mode: Mode, root: Option<&str>) -> Result<ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let extra: Vec<CertificateDer<'static>> = match root {
        Some(file) => CertificateDer::pem_file_iter(file)
            .and_then(|certs| certs.collect::<Result<_, _>>())
            .map_err(|e| format!("cannot read sslrootcert {file}: {e}"))?,
        None => Vec::new(),
    };
    let verifier = Verifier::new_with_extra_roots(extra, provider.clone())
        .map_err(|e| format!("cannot load the trusted roots: {e}"))?;
    let verifier: Arc<dyn ServerCertVerifier> = if mode == Mode::Ca {
        Arc::new(AnyName(verifier))
    } else {
        Arc::new(verifier)
    };
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

/// `verify-ca`: the chain is checked, the host name is not.
#[derive(Debug)]
struct AnyName(Verifier);

impl ServerCertVerifier for AnyName {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self
            .0
            .verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
        {
            Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
            )) => Ok(ServerCertVerified::assertion()),
            other => other,
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.0.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_removes_the_tls_options() {
        let (rest, mode, root) = split(
            "postgresql://u@h:5432/db?sslmode=verify-ca&application_name=x&sslrootcert=%2Fca.pem",
        )
        .unwrap();
        assert_eq!(rest, "postgresql://u@h:5432/db?application_name=x");
        assert_eq!(mode, Mode::Ca);
        assert_eq!(root.as_deref(), Some("/ca.pem"));
        let (rest, mode, _) = split("postgresql://u@h/db?sslmode=require").unwrap();
        assert_eq!((rest.as_str(), mode), ("postgresql://u@h/db", Mode::Full));
        let (rest, mode, _) = split("host=h user=u sslmode=prefer").unwrap();
        assert_eq!((rest.as_str(), mode), ("host=h user=u", Mode::Prefer));
        assert_eq!(split("postgresql://u@h/db").unwrap().1, Mode::Disable);
        assert!(split("postgresql://h/db?sslmode=bogus").is_err());
    }
}
