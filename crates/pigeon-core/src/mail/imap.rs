use super::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
};
use super::{AccountConfig, Attachment, MailError, MessageContent, TransportSecurity};
use crate::secrets::Secret;
use ::imap::types::Flag;
use ::imap::types::NameAttribute;
use ::imap::{Client, Session};
use imap_proto::types::{BodyStructure, MessageSection, SectionPath};
use mailparse::MailHeaderMap;
use native_tls::{Certificate, Protocol, TlsConnector};
use std::collections::{BTreeMap, HashMap};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub(super) struct ImapConnector;
impl MailConnector for ImapConnector {
    fn connect(
        &self,
        config: &AccountConfig,
        password: &Secret,
    ) -> Result<Box<dyn MailSession>, MailError> {
        let mut builder = TlsConnector::builder();
        builder.min_protocol_version(Some(Protocol::Tlsv12));
        if let Some(pem) = &config.ca_certificate {
            builder.add_root_certificate(Certificate::from_pem(pem.as_bytes()).map_err(|_| {
                MailError::InvalidConfig("the additional CA certificate is not valid PEM".into())
            })?);
        }
        let tls = builder.build().map_err(|_| MailError::Tls)?;
        let addresses = (config.host.as_str(), config.port)
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
        let mut client = match config.security {
            TransportSecurity::Tls => {
                let stream = tls
                    .connect(&config.host, stream)
                    .map_err(|_| MailError::Tls)?;
                let mut client = Client::new(stream);
                client
                    .read_greeting()
                    .map_err(|error| imap_error("reading the server greeting", error))?;
                client
            }
            TransportSecurity::StartTls => {
                let mut client = Client::new(stream);
                client
                    .read_greeting()
                    .map_err(|error| imap_error("reading the server greeting", error))?;
                client
                    .secure(&config.host, &tls)
                    .map_err(|_| MailError::Tls)?
            }
        };
        // Never enable this library's wire-debug mode: it includes LOGIN data.
        client.debug = false;
        let password = std::str::from_utf8(password).map_err(|_| {
            MailError::InvalidConfig("the account password is not valid text".into())
        })?;
        let session =
            client
                .login(&config.username, password)
                .map_err(|(error, _)| match error {
                    ::imap::Error::No(_) | ::imap::Error::Bad(_) => MailError::Authentication,
                    other => imap_error("authenticating", other),
                })?;
        Ok(Box::new(ImapSession { session }))
    }
}

// Never display/debug-format the library error: NO/BAD and Parse variants can
// contain arbitrary server responses, including private message content.
fn imap_error(operation: &'static str, error: ::imap::Error) -> MailError {
    use ::imap::Error;
    let reason = match error {
        Error::No(_) => "the server rejected the command (IMAP NO)",
        Error::Bad(_) => "the server rejected the command (IMAP BAD)",
        Error::ConnectionLost => "the server closed the connection",
        Error::Io(error) => match error.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                "the server response timed out"
            }
            std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe => "the server closed the connection",
            _ => "a network read or write failed",
        },
        Error::Parse(_) => return MailError::ResponseParse { operation },
        Error::Validate(_) => "this IMAP client could not encode the command arguments",
        Error::Tls(_) | Error::TlsHandshake(_) => "the encrypted connection failed",
        _ => "the IMAP operation failed",
    };
    MailError::Imap { operation, reason }
}

struct ImapSession<T: std::io::Read + std::io::Write> {
    session: Session<T>,
}
impl<T: std::io::Read + std::io::Write + Send> MailSession for ImapSession<T> {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        let folders = self
            .session
            .list(None, Some("*"))
            .map_err(|error| imap_error("listing folders", error))?;
        Ok(folders
            .iter()
            .filter(|folder| !folder.attributes().contains(&NameAttribute::NoSelect))
            .map(|folder| {
                let mut kind = match folder.name().to_ascii_lowercase().as_str() {
                    "inbox" => "inbox",
                    "sent" | "sent items" => "sent",
                    "drafts" => "drafts",
                    "archive" | "archives" => "archive",
                    "junk" | "spam" => "junk",
                    "trash" | "deleted items" => "trash",
                    _ => "other",
                };
                for attribute in folder.attributes() {
                    if let NameAttribute::Custom(name) = attribute {
                        kind = match name.to_ascii_lowercase().as_str() {
                            "\\sent" => "sent",
                            "\\drafts" => "drafts",
                            "\\archive" | "\\all" => "archive",
                            "\\junk" => "junk",
                            "\\trash" => "trash",
                            _ => kind,
                        };
                    }
                }
                RemoteFolder {
                    name: folder.name().to_owned(),
                    kind: kind.into(),
                }
            })
            .collect())
    }
    fn snapshot(&mut self, folder: &str) -> Result<FolderSnapshot, MailError> {
        // EXAMINE plus PEEK fetches never set Seen, and never expunge mail.
        let mailbox = self
            .session
            .examine(folder)
            .map_err(|error| imap_error("opening a folder read-only", error))?;
        let uid_validity = mailbox.uid_validity.ok_or(MailError::Protocol)?;
        let mut messages = Vec::new();
        if mailbox.exists > 0 {
            let flags = self
                .session
                .uid_fetch("1:*", "(UID FLAGS)")
                .map_err(|error| imap_error("reading message identifiers and flags", error))?;
            for message in flags.iter() {
                if message.flags().contains(&Flag::Deleted) {
                    continue;
                }
                messages.push(RemoteFlag {
                    uid: message.uid.ok_or(MailError::Protocol)?,
                    unread: !message.flags().contains(&Flag::Seen),
                });
            }
        }
        Ok(FolderSnapshot {
            uid_validity,
            messages,
        })
    }

    fn reselect(&mut self, folder: &str, expected_validity: u32) -> Result<(), MailError> {
        let mailbox = self
            .session
            .examine(folder)
            .map_err(|error| imap_error("reopening a folder read-only", error))?;
        if mailbox.uid_validity != Some(expected_validity) {
            return Err(MailError::Protocol);
        }
        Ok(())
    }

    fn fetch_messages(&mut self, uids: &[u32]) -> Result<Vec<RemoteMessage>, MailError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let fetched = self
            .session
            .uid_fetch(
                uid_set(uids),
                "(UID BODY.PEEK[HEADER] BODYSTRUCTURE INTERNALDATE)",
            )
            .map_err(|error| imap_error("reading message headers and MIME structure", error))?;
        let mut drafts = HashMap::<u32, Draft>::new();
        let mut groups = BTreeMap::<Vec<u32>, Vec<u32>>::new();
        for item in fetched.iter() {
            let uid = item.uid.ok_or(MailError::Protocol)?;
            let header = item.header().ok_or(MailError::Protocol)?;
            if header.len() > 262_144 {
                return Err(MailError::Protocol);
            }
            let headers = mailparse::parse_headers(header)
                .map_err(|_| MailError::Protocol)?
                .0;
            let mut content = MessageContent {
                sender: headers.get_first_value("From").unwrap_or_default(),
                recipients: [
                    headers.get_first_value("To"),
                    headers.get_first_value("Cc"),
                    headers.get_first_value("Bcc"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
                subject: headers
                    .get_first_value("Subject")
                    .unwrap_or_else(|| "(No subject)".into()),
                date: headers
                    .get_first_value("Date")
                    .and_then(|value| mailparse::dateparse(&value).ok())
                    .or_else(|| item.internal_date().map(|value| value.timestamp()))
                    .unwrap_or(0),
                message_id: headers.get_first_value("Message-ID").unwrap_or_default(),
                ..Default::default()
            };
            let mut parts = Vec::new();
            collect_parts(
                item.bodystructure().ok_or(MailError::Protocol)?,
                Vec::new(),
                &mut parts,
                &mut content.attachments,
                0,
            )?;
            for part in parts {
                groups.entry(part).or_default().push(uid);
            }
            drafts.insert(
                uid,
                Draft {
                    header: header.to_vec(),
                    content,
                },
            );
        }
        drop(fetched);

        // Group equal MIME section paths across the batch. A plain-text batch
        // takes two requests per 100 messages, not a request per message. No
        // BODY[]/RFC822 fetch is used, so attachment payloads stay on the server.
        for (path, part_uids) in groups {
            let section = if path.is_empty() {
                "TEXT".into()
            } else {
                path.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(".")
            };
            let query = if path.is_empty() {
                "(UID BODY.PEEK[TEXT])".into()
            } else {
                format!("(UID BODY.PEEK[{section}.MIME] BODY.PEEK[{section}])")
            };
            let responses = self
                .session
                .uid_fetch(uid_set(&part_uids), &query)
                .map_err(|error| imap_error("downloading message text", error))?;
            let mut received = std::collections::HashSet::new();
            for response in responses.iter() {
                let uid = response.uid.ok_or(MailError::Protocol)?;
                if !part_uids.contains(&uid) || !received.insert(uid) {
                    return Err(MailError::Protocol);
                }
                let draft = drafts.get_mut(&uid).ok_or(MailError::Protocol)?;
                let (mime, body) = if path.is_empty() {
                    (
                        draft.header.as_slice(),
                        response.text().ok_or(MailError::Protocol)?,
                    )
                } else {
                    (
                        response
                            .section(&SectionPath::Part(path.clone(), Some(MessageSection::Mime)))
                            .ok_or(MailError::Protocol)?,
                        response
                            .section(&SectionPath::Part(path.clone(), None))
                            .ok_or(MailError::Protocol)?,
                    )
                };
                if body.len() > 16 * 1024 * 1024 {
                    return Err(MailError::Protocol);
                }
                let mut raw = mime.to_vec();
                if !raw.ends_with(b"\r\n\r\n") && !raw.ends_with(b"\n\n") {
                    raw.extend_from_slice(b"\r\n");
                }
                raw.extend_from_slice(body);
                let part = mailparse::parse_mail(&raw).map_err(|_| MailError::Protocol)?;
                let body = part.get_body().map_err(|_| MailError::Protocol)?;
                match part.ctype.mimetype.to_ascii_lowercase().as_str() {
                    "text/plain" => {
                        if !draft.content.body_text.is_empty() {
                            draft.content.body_text.push('\n');
                        }
                        draft.content.body_text.push_str(&body);
                    }
                    "text/html" => {
                        let html = draft.content.body_html.get_or_insert_with(String::new);
                        html.push_str(&body);
                    }
                    _ => return Err(MailError::Protocol),
                }
            }
            if received.len() != part_uids.len() {
                return Err(MailError::Protocol);
            }
        }
        let mut messages = Vec::new();
        for uid in uids {
            let mut draft = drafts.remove(uid).ok_or(MailError::Protocol)?;
            if draft.content.body_text.trim().is_empty()
                && let Some(html) = &draft.content.body_html
            {
                let clean = crate::viewer::sanitise(html, crate::viewer::RemoteContent::Blocked);
                draft.content.body_text =
                    html2text::from_read(clean.as_bytes(), 100).map_err(|_| MailError::Protocol)?;
            }
            messages.push(RemoteMessage {
                uid: *uid,
                content: draft.content,
            });
        }
        Ok(messages)
    }
}

struct Draft {
    header: Vec<u8>,
    content: MessageContent,
}
fn uid_set(uids: &[u32]) -> String {
    uids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn collect_parts(
    body: &BodyStructure<'_>,
    path: Vec<u32>,
    text: &mut Vec<Vec<u32>>,
    attachments: &mut Vec<Attachment>,
    depth: usize,
) -> Result<(), MailError> {
    if depth > 32 || text.len() + attachments.len() > 2048 {
        return Err(MailError::Protocol);
    }
    match body {
        BodyStructure::Multipart { bodies, .. } => {
            for (index, child) in bodies.iter().enumerate() {
                let mut path = path.clone();
                path.push(index as u32 + 1);
                collect_parts(child, path, text, attachments, depth + 1)?;
            }
        }
        BodyStructure::Basic { common, other, .. }
        | BodyStructure::Text { common, other, .. }
        | BodyStructure::Message { common, other, .. } => {
            let filename = common
                .disposition
                .as_ref()
                .and_then(|disposition| disposition.params.as_ref())
                .and_then(|params| {
                    params
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case("filename"))
                })
                .map(|(_, value)| (*value).to_owned())
                .or_else(|| {
                    common
                        .ty
                        .params
                        .as_ref()
                        .and_then(|params| {
                            params
                                .iter()
                                .find(|(key, _)| key.eq_ignore_ascii_case("name"))
                        })
                        .map(|(_, value)| (*value).to_owned())
                });
            let attached = filename.is_some()
                || common
                    .disposition
                    .as_ref()
                    .is_some_and(|value| value.ty.eq_ignore_ascii_case("attachment"));
            let is_body = !attached
                && common.ty.ty.eq_ignore_ascii_case("text")
                && ["plain", "html"]
                    .iter()
                    .any(|subtype| common.ty.subtype.eq_ignore_ascii_case(subtype));
            if is_body {
                if other.octets > 16 * 1024 * 1024 {
                    return Err(MailError::Protocol);
                }
                text.push(path);
            } else {
                let part = if path.is_empty() {
                    "1".into()
                } else {
                    path.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(".")
                };
                attachments.push(Attachment {
                    part,
                    filename: filename.unwrap_or_else(|| {
                        if common.ty.ty.eq_ignore_ascii_case("message") {
                            "Attached message".into()
                        } else {
                            "Unnamed attachment".into()
                        }
                    }),
                    content_type: format!("{}/{}", common.ty.ty, common.ty.subtype)
                        .to_ascii_lowercase(),
                    size: other.octets,
                    content_id: other
                        .id
                        .map(|value| value.trim_matches(['<', '>']).to_owned()),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    // Real adapter calls over a disposable server connection. This exercises
    // response parsing and error translation, not a mock of that translation.
    #[test]
    fn folder_errors_distinguish_rejection_from_disconnect_without_echoing_server_data() {
        for (reply, expected) in [
            (Some("NO"), "rejected"),
            (Some("BAD"), "rejected"),
            (Some("MALFORMED"), "parsed"),
            (Some("TIMEOUT"), "timed out"),
            (None, "closed"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream.write_all(b"* OK Test IMAP\r\n").unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                for command in ["LOGIN", "LIST"] {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let mut fields = line.split_whitespace();
                    let tag = fields.next().unwrap();
                    assert_eq!(fields.next().unwrap(), command);
                    if command == "LOGIN" {
                        writeln!(stream, "{tag} OK authenticated\r").unwrap();
                    } else if reply == Some("TIMEOUT") {
                        std::thread::sleep(Duration::from_millis(1200));
                    } else if let Some(status) = reply {
                        writeln!(stream, "{tag} {status} PRIVATE_RESPONSE_CANARY\r").unwrap();
                    }
                }
            });
            let stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut client = Client::new(stream);
            client.read_greeting().unwrap();
            let session = client
                .login("synthetic-user", "synthetic-password")
                .map_err(|(e, _)| e)
                .unwrap();
            let error = ImapSession { session }.folders().unwrap_err().to_string();
            server.join().unwrap();
            assert!(
                error.contains("listing folders"),
                "missing operation: {error}"
            );
            assert!(
                error.contains(expected),
                "missing failure category: {error}"
            );
            assert!(!error.contains("PRIVATE_RESPONSE_CANARY"));
        }
    }

    #[test]
    fn parser_failure_in_real_fetch_response_is_isolated_from_readable_messages() {
        use crate::mail::{MailEngine, SearchQuery};
        use crate::secrets::{KeyringState, MemoryStore};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Connector(std::net::SocketAddr);
        impl MailConnector for Connector {
            fn connect(
                &self,
                _: &AccountConfig,
                _: &Secret,
            ) -> Result<Box<dyn MailSession>, MailError> {
                let stream = TcpStream::connect(self.0).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut client = Client::new(stream);
                client.read_greeting().unwrap();
                let session = client
                    .login("synthetic", "synthetic")
                    .map_err(|(e, _)| imap_error("test login", e))?;
                Ok(Box::new(ImapSession { session }))
            }
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let server = std::thread::spawn(move || {
            let mut workers = Vec::new();
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                workers.push(std::thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                    stream.write_all(b"* OK Synthetic IMAP\r\n").unwrap();
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 { break; }
                        let fields: Vec<_> = line.split_whitespace().collect();
                        let tag = fields[0];
                        match fields[1] {
                            "LOGIN" => {},
                            "LIST" => stream.write_all(b"* LIST () \"/\" \"INBOX\"\r\n").unwrap(),
                            "EXAMINE" => stream.write_all(b"* 3 EXISTS\r\n* OK [UIDVALIDITY 1] epoch\r\n").unwrap(),
                            "UID" if line.contains("FLAGS") => {
                                for uid in 1..=3 { write!(stream, "* {uid} FETCH (UID {uid} FLAGS ())\r\n").unwrap(); }
                            }
                            "UID" => {
                                let uids: Vec<u32> = fields[3].split(',').map(|uid| uid.parse().unwrap()).collect();
                                if line.contains("BODYSTRUCTURE") && uids.contains(&2) {
                                    // RFC BODYSTRUCTURE permits multiple extension values;
                                    // the current parser only accepts one. No private mail.
                                    stream.write_all(b"* 2 FETCH (UID 2 BODYSTRUCTURE (\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 6 1 NIL NIL NIL NIL \"ext1\" \"ext2\"))\r\n").unwrap();
                                    write!(stream, "{tag} OK fetched\r\n").unwrap();
                                    break;
                                }
                                for uid in uids {
                                    if line.contains("BODYSTRUCTURE") {
                                        let header = format!("From: synthetic@example.test\r\nSubject: Message {uid}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n");
                                        write!(stream, "* {uid} FETCH (UID {uid} BODY[HEADER] {{{}}}\r\n{header} BODYSTRUCTURE (\"TEXT\" \"PLAIN\" NIL NIL NIL \"7BIT\" 6 1))\r\n", header.len()).unwrap();
                                    } else {
                                        write!(stream, "* {uid} FETCH (UID {uid} BODY[TEXT] {{6}}\r\nhello\n)\r\n").unwrap();
                                    }
                                }
                            }
                            _ => panic!("unexpected synthetic command"),
                        }
                        if write!(stream, "{tag} OK complete\r\n").is_err() { break; }
                    }
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let engine = MailEngine::with_connector(
            dir.path().join("mail.db"),
            Arc::new(MemoryStore::new(KeyringState::Unlocked)),
            Arc::new(Connector(address)),
        );
        let account = engine
            .add_account(
                AccountConfig {
                    name: "Synthetic".into(),
                    address: "synthetic@example.test".into(),
                    host: "localhost".into(),
                    port: address.port(),
                    username: "synthetic".into(),
                    security: TransportSecurity::Tls,
                    ca_certificate: None,
                },
                Secret::new(b"synthetic".to_vec()),
            )
            .unwrap();
        let result = engine.sync_account(&account.id);
        let messages = engine.search(&SearchQuery::default()).unwrap();
        drop(engine);
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
        assert!(
            matches!(result, Err(MailError::IncompleteSync(1))),
            "{result:?}"
        );
        assert_eq!(messages.total, 2);
        assert!(
            messages
                .messages
                .iter()
                .any(|message| message.subject == "Message 3")
        );
    }
}
