use crate::model::TlsInfo;
use anyhow::{Context, Result};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    DigitallySignedStruct, SignatureScheme,
};
use std::sync::Arc;
use tokio::net::TcpStream;
use x509_parser::prelude::*;

#[derive(Debug)]
pub struct ObserveCertificate;
impl ServerCertVerifier for ObserveCertificate {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            m,
            c,
            d,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            m,
            c,
            d,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub async fn inspect(ip: &str, port: u16, host: &str, insecure: bool) -> Result<TlsInfo> {
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::from_iter(
            webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
        ))
        .with_no_client_auth();
    if insecure {
        config
            .dangerous()
            .set_certificate_verifier(Arc::new(ObserveCertificate));
    }
    let name = ServerName::try_from(if host.is_empty() {
        ip.to_owned()
    } else {
        host.to_owned()
    })?;
    let stream = TcpStream::connect((ip, port)).await?;
    let peer_ip = stream.peer_addr()?.ip().to_string();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, stream)
        .await?;
    let (_, session) = tls.get_ref();
    let certs = session.peer_certificates().context("証明書なし")?;
    let leaf = certs.first().context("証明書なし")?;
    let (_, cert) = X509Certificate::from_der(leaf.as_ref()).map_err(|e| anyhow::anyhow!("{e}"))?;
    let san = cert
        .subject_alternative_name()?
        .map(|s| {
            s.value
                .general_names
                .iter()
                .map(|n| match n {
                    x509_parser::extensions::GeneralName::DNSName(s) => s.to_string(),
                    x509_parser::extensions::GeneralName::IPAddress(b) if b.len() == 4 => {
                        format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
                    }
                    other => format!("{other:?}"),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(TlsInfo {
        peer_ip,
        version: format!(
            "{:?}",
            session.protocol_version().context("TLS versionなし")?
        ),
        cipher: format!(
            "{:?}",
            session
                .negotiated_cipher_suite()
                .context("cipherなし")?
                .suite()
        ),
        subject: cert.subject().to_string(),
        issuer: cert.issuer().to_string(),
        serial: cert.raw_serial_as_string(),
        san,
        self_signed: cert.subject() == cert.issuer() && cert.verify_signature(None).is_ok(),
        not_before: cert.validity().not_before.to_string(),
        not_after: cert.validity().not_after.to_string(),
        certificate_sha256: crate::scan::sha256(leaf.as_ref()),
        chain_sha256: certs
            .iter()
            .map(|c| crate::scan::sha256(c.as_ref()))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn collects_self_signed_and_rejects_in_strict_mode() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["fixture.test".into()]).unwrap();
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der()).into(),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let _ = acceptor.accept(stream).await;
            }
        });
        let info = inspect("127.0.0.1", port, "fixture.test", true)
            .await
            .unwrap();
        assert!(info.self_signed);
        assert!(info.san.contains(&"fixture.test".into()));
        assert!(info.version.starts_with("TLS"));
        assert!(!info.cipher.is_empty());
        assert_eq!(info.certificate_sha256.len(), 64);
        assert!(inspect("127.0.0.1", port, "fixture.test", false)
            .await
            .is_err());
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }
}
