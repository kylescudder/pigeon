//! SMTP submission with an explicit point of no return.
//!
//! The end-of-data marker is what commits a message. Any failure before it is
//! written means the server must discard the message, so retrying is safe.
//! Once it is written, a lost connection or missing reply leaves the outcome
//! unknown. The caller's `commit` hook runs just before the marker so it can
//! durably record that boundary.
//!
//! Server reply text is never shown: it can echo addresses or message data.
//! Errors carry the reply code and our own description only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{MailError, TransportSecurity};
use crate::secrets::Secret;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub security: TransportSecurity,
    pub username: String,
}

pub struct SmtpServer<'a> {
    pub config: &'a SmtpConfig,
    /// Optional additional trust root shared with the account's IMAP server.
    pub ca_certificate: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub from: String,
    pub recipients: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    /// The server accepted responsibility for delivery.
    Accepted,
    /// The server did not take the message. `retry` is true when the cause is
    /// temporary, such as being offline or a 4xx reply.
    NotSent { retry: bool, reason: String },
    /// The message may have been accepted.
    Unknown(String),
}

/// The SMTP seam. Production uses [`SmtpClient`]; tests can script outcomes.
pub trait SmtpTransport: Send + Sync {
    /// Connects and signs in without sending anything.
    fn verify(&self, server: &SmtpServer<'_>, password: &Secret) -> Result<(), MailError>;
    fn submit(
        &self,
        server: &SmtpServer<'_>,
        password: &Secret,
        envelope: &Envelope,
        message: &[u8],
        commit: &mut dyn FnMut() -> Result<(), MailError>,
    ) -> Submission;
}

pub struct SmtpClient {
    /// How long to wait for the reply to the end-of-data marker. Longer waits
    /// turn fewer slow servers into unknown outcomes.
    final_reply_timeout: Duration,
}

impl Default for SmtpClient {
    fn default() -> Self {
        Self {
            final_reply_timeout: Duration::from_secs(120),
        }
    }
}

impl SmtpClient {
    pub fn with_final_reply_timeout(timeout: Duration) -> Self {
        Self {
            final_reply_timeout: timeout,
        }
    }

    fn open(&self, server: &SmtpServer<'_>, password: &Secret) -> Result<Conn, MailError> {
        let config = server.config;
        let tls = super::net::tls_connector(server.ca_certificate)?;
        let stream = super::net::tcp_connect(&config.host, config.port)?;
        let mut conn = match config.security {
            TransportSecurity::Tls => {
                let stream = tls
                    .connect(&config.host, stream)
                    .map_err(|_| MailError::Tls)?;
                let mut conn = Conn::new(Box::new(stream));
                conn.greeting()?;
                conn
            }
            TransportSecurity::StartTls => {
                let plain = starttls(stream)?;
                let stream = tls
                    .connect(&config.host, plain)
                    .map_err(|_| MailError::Tls)?;
                Conn::new(Box::new(stream))
            }
        };
        let capabilities = conn.ehlo()?;
        conn.authenticate(&capabilities, &config.username, password)?;
        Ok(conn)
    }
}

impl SmtpTransport for SmtpClient {
    fn verify(&self, server: &SmtpServer<'_>, password: &Secret) -> Result<(), MailError> {
        let mut conn = self.open(server, password)?;
        conn.quit();
        Ok(())
    }

    fn submit(
        &self,
        server: &SmtpServer<'_>,
        password: &Secret,
        envelope: &Envelope,
        message: &[u8],
        commit: &mut dyn FnMut() -> Result<(), MailError>,
    ) -> Submission {
        match self.open(server, password) {
            Ok(mut conn) => conn.send(envelope, message, commit, self.final_reply_timeout),
            Err(error) => not_sent(error),
        }
    }
}

/// Classifies a failure that happened before any message data was sent.
fn not_sent(error: MailError) -> Submission {
    let retry = match &error {
        MailError::Connection => true,
        MailError::Smtp { temporary, .. } => *temporary,
        _ => false,
    };
    Submission::NotSent {
        retry,
        reason: error.to_string(),
    }
}

trait Stream: Read + Write + Send {
    fn set_read_timeout(&self, timeout: Duration);
}
impl Stream for TcpStream {
    fn set_read_timeout(&self, timeout: Duration) {
        let _ = TcpStream::set_read_timeout(self, Some(timeout));
    }
}
impl Stream for native_tls::TlsStream<TcpStream> {
    fn set_read_timeout(&self, timeout: Duration) {
        let _ = self.get_ref().set_read_timeout(Some(timeout));
    }
}

struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Reply {
    /// The reply code and any enhanced status code, never the free text.
    fn describe(&self) -> String {
        let enhanced = self.lines.first().and_then(|line| {
            let token = line.split_whitespace().next()?;
            let parts: Vec<&str> = token.split('.').collect();
            (parts.len() == 3
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())))
            .then(|| token.to_owned())
        });
        match enhanced {
            Some(enhanced) => format!("{} {enhanced}", self.code),
            None => self.code.to_string(),
        }
    }

    fn error(&self, operation: &'static str) -> MailError {
        MailError::Smtp {
            operation,
            temporary: (400..500).contains(&self.code),
            reason: format!("the server replied {}", self.describe()),
        }
    }
}

fn read_reply(reader: &mut impl BufRead) -> std::io::Result<Reply> {
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed SMTP reply");
    let mut lines = Vec::new();
    let mut code = None;
    loop {
        let mut line = Vec::new();
        let read = reader.by_ref().take(4096).read_until(b'\n', &mut line)?;
        if read == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        let line = String::from_utf8_lossy(&line);
        let line = line.trim_end_matches(['\r', '\n']);
        if line.len() < 3 || !line.as_bytes()[..3].iter().all(u8::is_ascii_digit) {
            return Err(invalid());
        }
        let this: u16 = line[..3].parse().map_err(|_| invalid())?;
        if *code.get_or_insert(this) != this {
            return Err(invalid());
        }
        let last = match line.as_bytes().get(3) {
            None | Some(b' ') => true,
            Some(b'-') => false,
            Some(_) => return Err(invalid()),
        };
        lines.push(line.get(4..).unwrap_or_default().to_owned());
        if last {
            return Ok(Reply { code: this, lines });
        }
        if lines.len() > 256 {
            return Err(invalid());
        }
    }
}

fn io_error(operation: &'static str, error: &std::io::Error) -> MailError {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::InvalidData => MailError::Smtp {
            operation,
            temporary: false,
            reason: "the server sent a malformed reply".into(),
        },
        _ => MailError::Connection,
    }
}

/// Plain-text greeting and EHLO, then an upgrade that refuses to continue
/// without STARTTLS. Returns the socket ready for the TLS handshake.
fn starttls(stream: TcpStream) -> Result<TcpStream, MailError> {
    let mut reader = BufReader::new(stream);
    let greeting = read_reply(&mut reader).map_err(|e| io_error("reading the greeting", &e))?;
    if greeting.code != 220 {
        return Err(greeting.error("reading the greeting"));
    }
    let capabilities = command(&mut reader, "EHLO [127.0.0.1]", "introducing Pigeon")?;
    if capabilities.code != 250 {
        return Err(capabilities.error("introducing Pigeon"));
    }
    if !capabilities
        .lines
        .iter()
        .any(|line| line.eq_ignore_ascii_case("STARTTLS"))
    {
        return Err(MailError::Tls);
    }
    let reply = command(&mut reader, "STARTTLS", "starting encryption")?;
    if reply.code != 220 {
        return Err(MailError::Tls);
    }
    // Anything already buffered was sent before encryption and could have
    // been injected; refuse rather than treat it as part of the TLS session.
    if !reader.buffer().is_empty() {
        return Err(MailError::Tls);
    }
    Ok(reader.into_inner())
}

fn command(
    reader: &mut BufReader<impl Read + Write>,
    line: &str,
    operation: &'static str,
) -> Result<Reply, MailError> {
    let stream = reader.get_mut();
    stream
        .write_all(format!("{line}\r\n").as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| io_error(operation, &e))?;
    read_reply(reader).map_err(|e| io_error(operation, &e))
}

struct Conn {
    reader: BufReader<Box<dyn Stream>>,
}

impl Conn {
    fn new(stream: Box<dyn Stream>) -> Self {
        Self {
            reader: BufReader::new(stream),
        }
    }

    fn greeting(&mut self) -> Result<(), MailError> {
        let reply =
            read_reply(&mut self.reader).map_err(|e| io_error("reading the greeting", &e))?;
        if reply.code != 220 {
            return Err(reply.error("reading the greeting"));
        }
        Ok(())
    }

    fn command(&mut self, line: &str, operation: &'static str) -> Result<Reply, MailError> {
        command(&mut self.reader, line, operation)
    }

    /// Identifies with an address literal so the local hostname stays private.
    fn ehlo(&mut self) -> Result<Vec<String>, MailError> {
        let reply = self.command("EHLO [127.0.0.1]", "introducing Pigeon")?;
        if reply.code != 250 {
            return Err(reply.error("introducing Pigeon"));
        }
        Ok(reply.lines)
    }

    fn authenticate(
        &mut self,
        capabilities: &[String],
        username: &str,
        password: &Secret,
    ) -> Result<(), MailError> {
        let password = std::str::from_utf8(password).map_err(|_| {
            MailError::InvalidConfig("the account password is not valid text".into())
        })?;
        let mechanisms: Vec<String> = capabilities
            .iter()
            .filter_map(|line| {
                let upper = line.to_ascii_uppercase();
                upper
                    .strip_prefix("AUTH ")
                    .or_else(|| upper.strip_prefix("AUTH="))
                    .map(str::to_owned)
            })
            .flat_map(|list| {
                list.split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        let operation = "signing in";
        let reply = if mechanisms.iter().any(|m| m == "PLAIN") {
            let token = Zeroizing::new(format!("\0{username}\0{password}"));
            let line = Zeroizing::new(format!("AUTH PLAIN {}", BASE64.encode(token.as_bytes())));
            self.command(&line, operation)?
        } else if mechanisms.iter().any(|m| m == "LOGIN") {
            let reply = self.command("AUTH LOGIN", operation)?;
            if reply.code != 334 {
                return Err(auth_error(&reply));
            }
            let reply = self.command(&BASE64.encode(username), operation)?;
            if reply.code != 334 {
                return Err(auth_error(&reply));
            }
            let line = Zeroizing::new(BASE64.encode(password));
            self.command(&line, operation)?
        } else {
            return Err(MailError::Smtp {
                operation,
                temporary: false,
                reason: "the server offers no supported sign-in method".into(),
            });
        };
        if reply.code != 235 {
            return Err(auth_error(&reply));
        }
        Ok(())
    }

    fn send(
        &mut self,
        envelope: &Envelope,
        message: &[u8],
        commit: &mut dyn FnMut() -> Result<(), MailError>,
        final_reply_timeout: Duration,
    ) -> Submission {
        if let Err(error) = self.envelope(envelope) {
            self.quit();
            return not_sent(error);
        }
        if let Err(error) = self.write_data(message) {
            return not_sent(error);
        }
        // Nothing past this line may be retried automatically.
        if commit().is_err() {
            // Dropping the connection without the marker makes the server
            // discard the message.
            return Submission::NotSent {
                retry: true,
                reason: "Pigeon could not record sending progress, so it stopped before sending"
                    .into(),
            };
        }
        let stream = self.reader.get_mut();
        if stream
            .write_all(b".\r\n")
            .and_then(|()| stream.flush())
            .is_err()
        {
            return unknown();
        }
        self.reader.get_ref().set_read_timeout(final_reply_timeout);
        let outcome = match read_reply(&mut self.reader) {
            Ok(reply) if (200..300).contains(&reply.code) => Submission::Accepted,
            Ok(reply) if (400..600).contains(&reply.code) => Submission::NotSent {
                retry: reply.code < 500,
                reason: format!("the server refused the message ({})", reply.describe()),
            },
            _ => unknown(),
        };
        self.quit();
        outcome
    }

    fn envelope(&mut self, envelope: &Envelope) -> Result<(), MailError> {
        let reply = self.command(
            &format!("MAIL FROM:<{}>", envelope.from),
            "starting the message",
        )?;
        if reply.code != 250 {
            return Err(reply.error("starting the message"));
        }
        for recipient in &envelope.recipients {
            let reply = self.command(&format!("RCPT TO:<{recipient}>"), "adding a recipient")?;
            if reply.code != 250 && reply.code != 251 {
                return Err(MailError::Smtp {
                    operation: "adding a recipient",
                    temporary: (400..500).contains(&reply.code),
                    reason: format!("the server refused {recipient} ({})", reply.describe()),
                });
            }
        }
        let reply = self.command("DATA", "starting the message body")?;
        if reply.code != 354 {
            return Err(reply.error("starting the message body"));
        }
        Ok(())
    }

    /// Writes the message with CRLF line endings and leading dots doubled,
    /// stopping just short of the end-of-data marker.
    fn write_data(&mut self, message: &[u8]) -> Result<(), MailError> {
        let mut data = Vec::with_capacity(message.len() + message.len() / 50 + 2);
        let body = message.strip_suffix(b"\n").unwrap_or(message);
        for line in body.split(|byte| *byte == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.first() == Some(&b'.') {
                data.push(b'.');
            }
            data.extend_from_slice(line);
            data.extend_from_slice(b"\r\n");
        }
        let stream = self.reader.get_mut();
        stream
            .write_all(&data)
            .and_then(|()| stream.flush())
            .map_err(|_| MailError::Connection)
    }

    fn quit(&mut self) {
        let _ = self.command("QUIT", "closing the connection");
    }
}

fn unknown() -> Submission {
    Submission::Unknown(
        "the connection ended after the message was sent, before the server confirmed it; it may have been delivered".into(),
    )
}

fn auth_error(reply: &Reply) -> MailError {
    if (400..500).contains(&reply.code) {
        reply.error("signing in")
    } else {
        MailError::Authentication
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone, Copy)]
    enum Fault {
        None,
        RejectRecipient,
        FinalReply(&'static str),
        DropAfterData,
        StallAfterData,
        Advertise(&'static str),
        InjectAfterStartTls,
    }

    #[derive(Default)]
    struct Received {
        data: Vec<u8>,
        terminated: bool,
    }

    fn server(fault: Fault) -> (String, std::thread::JoinHandle<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"220 test ESMTP\r\n").unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut received = Received::default();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return received;
                }
                let upper = line.to_ascii_uppercase();
                let reply: &[u8] = if upper.starts_with("EHLO") {
                    match fault {
                        Fault::Advertise(lines) => lines.as_bytes(),
                        _ => b"250-test\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n",
                    }
                } else if upper.starts_with("STARTTLS") {
                    match fault {
                        Fault::InjectAfterStartTls => b"220 go ahead\r\n250 injected\r\n",
                        _ => b"220 go ahead\r\n",
                    }
                } else if let Some(token) = line.strip_prefix("AUTH PLAIN ") {
                    let decoded = BASE64.decode(token.trim()).unwrap();
                    if decoded == b"\0user\0secret" {
                        b"235 2.7.0 ok\r\n"
                    } else {
                        b"535 5.7.8 credentials for user rejected\r\n"
                    }
                } else if upper.starts_with("MAIL") {
                    b"250 2.1.0 ok\r\n"
                } else if upper.starts_with("RCPT") {
                    if matches!(fault, Fault::RejectRecipient) && upper.contains("NOBODY") {
                        b"550 5.1.1 private detail: no such user\r\n"
                    } else {
                        b"250 2.1.5 ok\r\n"
                    }
                } else if upper.starts_with("DATA") {
                    stream.write_all(b"354 go\r\n").unwrap();
                    loop {
                        let mut data = Vec::new();
                        if reader.read_until(b'\n', &mut data).unwrap_or(0) == 0 {
                            return received;
                        }
                        if data == b".\r\n" {
                            received.terminated = true;
                            break;
                        }
                        received.data.extend_from_slice(&data);
                    }
                    match fault {
                        Fault::FinalReply(reply) => reply.as_bytes(),
                        Fault::DropAfterData => return received,
                        Fault::StallAfterData => {
                            std::thread::sleep(Duration::from_millis(800));
                            return received;
                        }
                        _ => b"250 2.0.0 queued\r\n",
                    }
                } else if upper.starts_with("QUIT") {
                    let _ = stream.write_all(b"221 bye\r\n");
                    return received;
                } else {
                    b"500 unknown\r\n"
                };
                stream.write_all(reply).unwrap();
            }
        });
        (address, handle)
    }

    fn connect(address: &str) -> Conn {
        let mut conn = Conn::new(Box::new(TcpStream::connect(address).unwrap()));
        conn.greeting().unwrap();
        let capabilities = conn.ehlo().unwrap();
        conn.authenticate(&capabilities, "user", &Secret::new(b"secret".to_vec()))
            .unwrap();
        conn
    }

    fn envelope(recipients: &[&str]) -> Envelope {
        Envelope {
            from: "me@pigeon.test".into(),
            recipients: recipients.iter().map(|r| (*r).to_owned()).collect(),
        }
    }

    fn send(fault: Fault, recipients: &[&str], message: &[u8]) -> (Submission, usize, Received) {
        let (address, server) = server(fault);
        let mut conn = connect(&address);
        let commits = AtomicUsize::new(0);
        let outcome = conn.send(
            &envelope(recipients),
            message,
            &mut || {
                commits.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            Duration::from_millis(300),
        );
        drop(conn);
        (outcome, commits.into_inner(), server.join().unwrap())
    }

    #[test]
    fn accepted_message_is_dot_stuffed_with_crlf_and_committed_once() {
        let (outcome, commits, received) = send(
            Fault::None,
            &["you@pigeon.test"],
            b"Subject: hi\r\n\r\n.hidden line\nplain\r\n",
        );
        assert_eq!(outcome, Submission::Accepted);
        assert_eq!(commits, 1);
        assert!(received.terminated);
        assert_eq!(
            received.data,
            b"Subject: hi\r\n\r\n..hidden line\r\nplain\r\n"
        );
    }

    #[test]
    fn refused_recipient_stops_before_data_without_echoing_server_text() {
        let (outcome, commits, received) = send(
            Fault::RejectRecipient,
            &["you@pigeon.test", "nobody@pigeon.test"],
            b"Subject: hi\r\n\r\nbody\r\n",
        );
        let Submission::NotSent { retry, reason } = outcome else {
            panic!("expected not sent");
        };
        assert!(!retry);
        assert!(reason.contains("nobody@pigeon.test") && reason.contains("550 5.1.1"));
        assert!(!reason.contains("private detail"));
        assert_eq!(commits, 0);
        assert!(received.data.is_empty() && !received.terminated);
    }

    #[test]
    fn an_explicit_refusal_after_data_is_not_sent_and_says_whether_to_retry() {
        for (reply, expected_retry) in [
            ("451 4.3.0 try later\r\n", true),
            ("554 5.6.0 rejected\r\n", false),
        ] {
            let (outcome, commits, _) =
                send(Fault::FinalReply(reply), &["you@pigeon.test"], b"body\r\n");
            assert_eq!(commits, 1);
            assert!(matches!(
                outcome,
                Submission::NotSent { retry, .. } if retry == expected_retry
            ));
        }
    }

    #[test]
    fn a_lost_or_missing_final_reply_is_unknown() {
        for fault in [Fault::DropAfterData, Fault::StallAfterData] {
            let (outcome, commits, received) = send(fault, &["you@pigeon.test"], b"body\r\n");
            assert!(matches!(outcome, Submission::Unknown(_)));
            assert_eq!(commits, 1);
            assert!(received.terminated);
        }
    }

    #[test]
    fn a_failed_commit_never_sends_the_end_of_data_marker() {
        let (address, server) = server(Fault::None);
        let mut conn = connect(&address);
        let outcome = conn.send(
            &envelope(&["you@pigeon.test"]),
            b"body\r\n",
            &mut || Err(MailError::Locked),
            Duration::from_millis(300),
        );
        drop(conn);
        assert!(matches!(outcome, Submission::NotSent { retry: true, .. }));
        let received = server.join().unwrap();
        assert!(!received.terminated);
    }

    #[test]
    fn rejected_credentials_are_an_authentication_error() {
        let (address, _server) = server(Fault::None);
        let mut conn = Conn::new(Box::new(TcpStream::connect(&address).unwrap()));
        conn.greeting().unwrap();
        let capabilities = conn.ehlo().unwrap();
        let error = conn
            .authenticate(&capabilities, "user", &Secret::new(b"wrong".to_vec()))
            .unwrap_err();
        assert!(matches!(error, MailError::Authentication));
    }

    #[test]
    fn starttls_is_required_and_pre_encryption_data_is_refused() {
        let (address, _server) = server(Fault::Advertise("250-test\r\n250 AUTH PLAIN\r\n"));
        let error = starttls(TcpStream::connect(&address).unwrap()).unwrap_err();
        assert!(matches!(error, MailError::Tls));

        let (address, _server) = server(Fault::InjectAfterStartTls);
        let error = starttls(TcpStream::connect(&address).unwrap()).unwrap_err();
        assert!(matches!(error, MailError::Tls));

        let (address, _server) = server(Fault::Advertise("250-test\r\n250 STARTTLS\r\n"));
        assert!(starttls(TcpStream::connect(&address).unwrap()).is_ok());
    }
}
