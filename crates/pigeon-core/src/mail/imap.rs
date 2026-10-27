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
use native_tls::{Certificate, Protocol, TlsConnector, TlsStream};
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
                client.read_greeting().map_err(|_| MailError::Connection)?;
                client
            }
            TransportSecurity::StartTls => {
                let mut client = Client::new(stream);
                client.read_greeting().map_err(|_| MailError::Connection)?;
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
        let session = client
            .login(&config.username, password)
            .map_err(|_| MailError::Authentication)?;
        Ok(Box::new(ImapSession { session }))
    }
}

struct ImapSession {
    session: Session<TlsStream<TcpStream>>,
}
impl MailSession for ImapSession {
    fn folders(&mut self) -> Result<Vec<RemoteFolder>, MailError> {
        let folders = self
            .session
            .list(None, Some("*"))
            .map_err(|_| MailError::Connection)?;
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
            .map_err(|_| MailError::Connection)?;
        let uid_validity = mailbox.uid_validity.ok_or(MailError::Protocol)?;
        let mut messages = Vec::new();
        if mailbox.exists > 0 {
            let flags = self
                .session
                .uid_fetch("1:*", "(UID FLAGS)")
                .map_err(|_| MailError::Connection)?;
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
            .map_err(|_| MailError::Connection)?;
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
                .map_err(|_| MailError::Connection)?;
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
