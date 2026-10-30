//! Verified TLS and bounded TCP connection set-up shared by IMAP and SMTP.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use native_tls::{Certificate, Protocol, TlsConnector};

use super::MailError;

/// TLS 1.2 or later with normal certificate and hostname verification. An
/// optional extra trust root is added; verification is never disabled.
pub(super) fn tls_connector(ca_certificate: Option<&str>) -> Result<TlsConnector, MailError> {
    let mut builder = TlsConnector::builder();
    builder.min_protocol_version(Some(Protocol::Tlsv12));
    if let Some(pem) = ca_certificate {
        builder.add_root_certificate(Certificate::from_pem(pem.as_bytes()).map_err(|_| {
            MailError::InvalidConfig("the additional CA certificate is not valid PEM".into())
        })?);
    }
    builder.build().map_err(|_| MailError::Tls)
}

pub(super) fn tcp_connect(host: &str, port: u16) -> Result<TcpStream, MailError> {
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|_| MailError::Connection)?;
    let mut connected = None;
    for address in addresses.take(4) {
        if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_secs(8)) {
            connected = Some(stream);
            break;
        }
    }
    let stream = connected.ok_or(MailError::Connection)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .map_err(|_| MailError::Connection)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(20)))
        .map_err(|_| MailError::Connection)?;
    Ok(stream)
}
