use super::source::{
    FolderSnapshot, MailConnector, MailSession, RemoteFlag, RemoteFolder, RemoteMessage,
    WriteCapabilities,
};
use super::{AccountConfig, Attachment, MailError, MessageContent, TransportSecurity};
use crate::secrets::Secret;
use ::imap::types::Flag;
use ::imap::types::NameAttribute;
use ::imap::{Client, Session};
use imap_proto::types::{BodyStructure, ContentEncoding, MessageSection, SectionPath};
use mailparse::MailHeaderMap;
use std::collections::{BTreeMap, HashMap};
use zeroize::Zeroizing;

pub(super) struct ImapConnector;
impl MailConnector for ImapConnector {
    fn connect(
        &self,
        config: &AccountConfig,
        password: &Secret,
    ) -> Result<Box<dyn MailSession>, MailError> {
        let tls = super::net::tls_connector(config.ca_certificate.as_deref())?;
        let stream = super::net::tcp_connect(&config.host, config.port)?;
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
    if matches!(error, Error::Parse(_)) {
        let diagnostic = super::diagnostics::report(&error);
        return MailError::ResponseParse {
            operation,
            diagnostic,
        };
    }
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
        Error::Validate(_) => "this IMAP client could not encode the command arguments",
        Error::Tls(_) | Error::TlsHandshake(_) => "the encrypted connection failed",
        _ => "the IMAP operation failed",
    };
    MailError::Imap { operation, reason }
}

const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;

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
                    delimiter: folder.delimiter().map(str::to_owned),
                }
            })
            .collect())
    }
    fn snapshot(&mut self, folder: &str) -> Result<FolderSnapshot, MailError> {
        // Sync is read-only: EXAMINE plus PEEK fetches never set Seen and never
        // expunge. Only the action queue's methods below write.
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

    fn append(&mut self, folder: &str, message: &[u8]) -> Result<(), MailError> {
        self.session
            .append_with_flags(folder, message, &[Flag::Seen])
            .map_err(|error| imap_error("saving to the Sent folder", error))
    }

    fn has_message_id(&mut self, folder: &str, message_id: &str) -> Result<bool, MailError> {
        // Pigeon generates these IDs; refuse anything that could break out
        // of the quoted search string.
        if message_id.contains(['"', '\\', '\r', '\n']) {
            return Ok(false);
        }
        self.session
            .examine(folder)
            .map_err(|error| imap_error("opening the Sent folder", error))?;
        let found = self
            .session
            .uid_search(format!("HEADER Message-ID \"{message_id}\""))
            .map_err(|error| imap_error("checking the Sent folder", error))?;
        Ok(!found.is_empty())
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
        let mut unstructured = Vec::new();
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
            if parts.is_empty() && content.attachments.is_empty() {
                unstructured.push(uid);
            }
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
                if body.len() > MAX_TEXT_BYTES {
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
        // A multipart listing no child parts gives no sections to request, so
        // read the raw text. Nothing says how large or what it holds, hence
        // the same cap as other text, applied by the server.
        if !unstructured.is_empty() {
            let responses = self
                .session
                .uid_fetch(
                    uid_set(&unstructured),
                    format!("(UID BODY.PEEK[TEXT]<0.{MAX_TEXT_BYTES}>)"),
                )
                .map_err(|error| imap_error("downloading unstructured message text", error))?;
            for response in responses.iter() {
                let uid = response.uid.ok_or(MailError::Protocol)?;
                if !unstructured.contains(&uid) {
                    return Err(MailError::Protocol);
                }
                let draft = drafts.get_mut(&uid).ok_or(MailError::Protocol)?;
                recover_text(draft, response.text().unwrap_or_default());
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

    fn capabilities(&mut self) -> Result<WriteCapabilities, MailError> {
        let capabilities = self
            .session
            .capabilities()
            .map_err(|error| imap_error("checking server capabilities", error))?;
        Ok(WriteCapabilities {
            moves: capabilities.has_str("MOVE"),
            uidplus: capabilities.has_str("UIDPLUS"),
        })
    }

    fn select_writable(&mut self, folder: &str) -> Result<u32, MailError> {
        let mailbox = self
            .session
            .select(folder)
            .map_err(|error| imap_error("opening a folder to change it", error))?;
        mailbox.uid_validity.ok_or(MailError::Protocol)
    }

    fn present(&mut self, uids: &[u32]) -> Result<Vec<RemoteFlag>, MailError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let fetched = self
            .session
            .uid_fetch(uid_set(uids), "(UID FLAGS)")
            .map_err(|error| imap_error("checking messages before changing them", error))?;
        let mut present = Vec::new();
        for message in fetched.iter() {
            // Servers may add unsolicited FETCH responses for other messages.
            if let Some(uid) = message.uid.filter(|uid| uids.contains(uid)) {
                present.push(RemoteFlag {
                    uid,
                    unread: !message.flags().contains(&Flag::Seen),
                });
            }
        }
        Ok(present)
    }

    fn set_seen(&mut self, uids: &[u32], seen: bool) -> Result<(), MailError> {
        let change = if seen {
            "+FLAGS.SILENT (\\Seen)"
        } else {
            "-FLAGS.SILENT (\\Seen)"
        };
        self.session
            .uid_store(uid_set(uids), change)
            .map_err(|error| imap_error("changing read status", error))?;
        Ok(())
    }

    fn move_to(&mut self, uids: &[u32], target: &str) -> Result<(), MailError> {
        // The library quotes the mailbox name for MOVE.
        self.session
            .uid_mv(uid_set(uids), target)
            .map_err(|error| imap_error("moving messages", error))
    }

    fn copy_to(&mut self, uids: &[u32], target: &str) -> Result<(), MailError> {
        // Unlike MOVE, the library sends the COPY mailbox name verbatim.
        self.session
            .uid_copy(uid_set(uids), quoted(target)?)
            .map_err(|error| imap_error("copying messages", error))
    }

    fn expunge_uids(&mut self, uids: &[u32]) -> Result<(), MailError> {
        let set = uid_set(uids);
        self.session
            .uid_store(&set, "+FLAGS.SILENT (\\Deleted)")
            .map_err(|error| imap_error("marking messages for deletion", error))?;
        // UID EXPUNGE removes only these UIDs; never a plain EXPUNGE.
        self.session
            .uid_expunge(&set)
            .map_err(|error| imap_error("deleting messages permanently", error))?;
        Ok(())
    }

    fn fetch_part(
        &mut self,
        uid: u32,
        attachment: &Attachment,
        max_bytes: usize,
    ) -> Result<Zeroizing<Vec<u8>>, MailError> {
        const OPERATION: &str = "downloading an attachment";
        let path = parse_part(&attachment.part).ok_or(MailError::NotFound)?;
        // Re-read the structure first: it gives the transfer encoding, and a
        // part that no longer matches the cached metadata is refused.
        let structure = self
            .session
            .uid_fetch(uid.to_string(), "(UID BODYSTRUCTURE)")
            .map_err(|error| imap_error("reading an attachment's MIME structure", error))?;
        let item = structure
            .iter()
            .find(|item| item.uid == Some(uid))
            .ok_or(MailError::NotFound)?;
        let (common, other) =
            match find_part(item.bodystructure().ok_or(MailError::Protocol)?, &path)
                .ok_or(MailError::Protocol)?
            {
                BodyStructure::Basic { common, other, .. }
                | BodyStructure::Text { common, other, .. }
                | BodyStructure::Message { common, other, .. } => (common, other),
                BodyStructure::Multipart { .. } => return Err(MailError::Protocol),
            };
        if format!("{}/{}", common.ty.ty, common.ty.subtype).to_ascii_lowercase()
            != attachment.content_type
            || other.octets != attachment.size
        {
            return Err(MailError::Protocol);
        }
        if other.octets as usize > super::attachments::encoded_limit(max_bytes) {
            return Err(MailError::Imap {
                operation: OPERATION,
                reason: "the attachment is larger than Pigeon downloads",
            });
        }
        let encoding = match other.transfer_encoding {
            ContentEncoding::Base64 => "base64",
            ContentEncoding::QuotedPrintable => "quoted-printable",
            ContentEncoding::SevenBit | ContentEncoding::EightBit | ContentEncoding::Binary => {
                "binary"
            }
            ContentEncoding::Other(_) => return Err(MailError::Protocol),
        };
        drop(structure);
        // PEEK keeps the message's read state; only this part is transferred.
        let fetched = self
            .session
            .uid_fetch(
                uid.to_string(),
                format!("(UID BODY.PEEK[{}])", attachment.part),
            )
            .map_err(|error| imap_error(OPERATION, error))?;
        let item = fetched
            .iter()
            .find(|item| item.uid == Some(uid))
            .ok_or(MailError::NotFound)?;
        let raw = item
            .section(&SectionPath::Part(path, None))
            .ok_or(MailError::Protocol)?;
        let mut wrapped =
            Zeroizing::new(format!("Content-Transfer-Encoding: {encoding}\r\n\r\n").into_bytes());
        wrapped.extend_from_slice(raw);
        drop(fetched);
        let data = Zeroizing::new(
            mailparse::parse_mail(&wrapped)
                .and_then(|part| part.get_body_raw())
                .map_err(|_| MailError::Protocol)?,
        );
        if data.len() > max_bytes {
            return Err(MailError::Imap {
                operation: OPERATION,
                reason: "the attachment is larger than Pigeon downloads",
            });
        }
        Ok(data)
    }
}

/// A dotted IMAP part number such as "2.1". Nothing else reaches a command.
fn parse_part(part: &str) -> Option<Vec<u32>> {
    if part.is_empty() || part.len() > 64 {
        return None;
    }
    part.split('.')
        .map(|number| {
            (!number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| number.parse::<u32>().ok())
                .flatten()
                .filter(|number| *number > 0)
        })
        .collect()
}

/// The structure a part number refers to. Part 1 of a single-part message is
/// its body, which is how `collect_parts` names it.
fn find_part<'a>(body: &'a BodyStructure<'a>, path: &[u32]) -> Option<&'a BodyStructure<'a>> {
    match (body, path) {
        (_, []) => Some(body),
        (BodyStructure::Multipart { bodies, .. }, [index, rest @ ..]) => {
            find_part(bodies.get(*index as usize - 1)?, rest)
        }
        (_, [1]) => Some(body),
        _ => None,
    }
}

/// An IMAP quoted string. Line breaks and NUL cannot be quoted.
fn quoted(name: &str) -> Result<String, MailError> {
    if name.contains(['\r', '\n', '\0']) {
        return Err(MailError::InvalidAction(
            "this folder name cannot be used in a command",
        ));
    }
    Ok(format!(
        "\"{}\"",
        name.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

struct Draft {
    header: Vec<u8>,
    content: MessageContent,
}
/// Fills a message from its raw text when the server gave no usable structure.
/// Never fails: one odd message must not stop the rest of the account syncing.
fn recover_text(draft: &mut Draft, text: &[u8]) {
    let mut raw = draft.header.clone();
    raw.extend_from_slice(text);
    let (mut plain, mut html) = (String::new(), String::new());
    if let Ok(mail) = mailparse::parse_mail(&raw)
        && !mail.subparts.is_empty()
    {
        collect_text(&mail, &mut plain, &mut html);
    }
    if plain.is_empty() && html.is_empty() {
        plain = String::from_utf8_lossy(text).into_owned();
    }
    draft.content.body_text = plain;
    draft.content.body_html = (!html.is_empty()).then_some(html);
}

fn collect_text(part: &mailparse::ParsedMail<'_>, plain: &mut String, html: &mut String) {
    use mailparse::DispositionType;
    if !part.subparts.is_empty() {
        for child in &part.subparts {
            collect_text(child, plain, html);
        }
        return;
    }
    if part.get_content_disposition().disposition == DispositionType::Attachment {
        return;
    }
    let target = match part.ctype.mimetype.to_ascii_lowercase().as_str() {
        "text/plain" => plain,
        "text/html" => html,
        _ => return,
    };
    if let Ok(body) = part.get_body() {
        target.push_str(&body);
    }
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
                if other.octets as usize > MAX_TEXT_BYTES {
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

    #[test]
    fn only_dotted_part_numbers_reach_a_fetch_command() {
        assert_eq!(parse_part("2"), Some(vec![2]));
        assert_eq!(parse_part("2.1.3"), Some(vec![2, 1, 3]));
        for hostile in [
            "",
            "0",
            "1.",
            ".1",
            "1..2",
            "2]",
            "1 BODY[]",
            "TEXT",
            "1.MIME",
            "-1",
            "+1",
            "99999999999",
        ] {
            assert_eq!(parse_part(hostile), None, "{hostile:?}");
        }
    }
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

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

    /// Syncs three messages through a real IMAP session against a scripted
    /// server. `metadata` writes the headers/BODYSTRUCTURE response for a UID.
    fn sync_scripted(
        metadata: fn(u32, &mut TcpStream),
        check: impl FnOnce(&crate::mail::MailEngine, Result<(), MailError>),
    ) {
        use crate::mail::MailEngine;
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
                                for uid in uids {
                                    if line.contains("BODYSTRUCTURE") {
                                        metadata(uid, &mut stream);
                                    } else {
                                        let text: &[u8] = match uid {
                                            1 if line.contains("<0.") => MULTIPART_TEXT,
                                            2 if line.contains("<0.") => b"Recovered loose text\r\n",
                                            _ => b"hello\n",
                                        };
                                        let origin = if line.contains("<0.") { "<0>" } else { "" };
                                        write!(stream, "* {uid} FETCH (UID {uid} BODY[TEXT]{origin} {{{}}}\r\n", text.len()).unwrap();
                                        stream.write_all(text).unwrap();
                                        stream.write_all(b")\r\n").unwrap();
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
                    managed: false,
                    name: "Synthetic".into(),
                    address: "synthetic@example.test".into(),
                    host: "localhost".into(),
                    port: address.port(),
                    username: "synthetic".into(),
                    security: TransportSecurity::Tls,
                    ca_certificate: None,
                    sending: Default::default(),
                },
                Secret::new(b"synthetic".to_vec()),
            )
            .unwrap();
        let result = engine.sync_account(&account.id);
        check(&engine, result);
        drop(engine);
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();
    }

    const MULTIPART_TEXT: &[u8] = b"--synthetic\r\nContent-Type: text/plain\r\n\r\nRecovered plain text\r\n--synthetic\r\nContent-Type: text/html\r\n\r\n<p>Recovered html</p>\r\n--synthetic\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=notes.txt\r\n\r\nAttached notes stay out\r\n--synthetic--\r\n";

    fn write_metadata(stream: &mut TcpStream, uid: u32, cid: &str, extension: &str) {
        let header = format!(
            "From: synthetic@example.test\r\nSubject: Message {uid}\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n"
        );
        write!(
            stream,
            "* {uid} FETCH (UID {uid} BODY[HEADER] {{{}}}\r\n{header} BODYSTRUCTURE (\"TEXT\" \"PLAIN\" NIL {cid} NIL \"7BIT\" 6 1{extension}))\r\n",
            header.len()
        )
        .unwrap();
    }

    #[test]
    fn compatible_fetch_responses_download_every_message() {
        use crate::mail::SearchQuery;
        sync_scripted(
            |uid, stream| {
                let cid = if uid == 3 {
                    "<synthetic@example.test>"
                } else {
                    "NIL"
                };
                let extension = if uid == 2 {
                    r#" NIL NIL NIL NIL "ext1" "ext2""#
                } else {
                    ""
                };
                write_metadata(stream, uid, cid, extension);
            },
            |engine, result| {
                let messages = engine.search(&SearchQuery::default()).unwrap();
                assert!(result.is_ok(), "{result:?}");
                assert_eq!(messages.total, 3);
                assert!(
                    messages
                        .messages
                        .iter()
                        .any(|message| message.subject == "Message 3")
                );
            },
        );
    }

    /// Without the fallback these messages cache cleanly with blank bodies,
    /// which would hide mail behind a successful sync.
    #[test]
    fn childless_multipart_recovers_text_from_the_raw_body() {
        use crate::mail::SearchQuery;
        sync_scripted(
            |uid, stream| {
                if uid == 3 {
                    return write_metadata(stream, uid, "NIL", "");
                }
                let header = format!(
                    "From: synthetic@example.test\r\nSubject: Message {uid}\r\nContent-Type: multipart/alternative; boundary=synthetic\r\n\r\n"
                );
                write!(
                    stream,
                    "* {uid} FETCH (INTERNALDATE \"01-Jan-2026 00:00:00 +0000\" BODYSTRUCTURE (\"ALTERNATIVE\" (\"BOUNDARY\" \"synthetic\" \"CHARSET\" \"UTF-8\") NIL NIL NIL) UID {uid} BODY[HEADER] {{{}}}\r\n{header})\r\n",
                    header.len()
                )
                .unwrap();
            },
            |engine, result| {
                assert!(result.is_ok(), "{result:?}");
                let found = engine.search(&SearchQuery::default()).unwrap();
                assert_eq!(found.total, 3);
                let body = |subject: &str| {
                    let id = found
                        .messages
                        .iter()
                        .find(|m| m.subject == subject)
                        .unwrap()
                        .id;
                    engine.message(id).unwrap()
                };
                let parts = body("Message 1");
                assert!(
                    parts.body_text.contains("Recovered plain text"),
                    "{parts:?}"
                );
                assert!(!parts.body_text.contains("Attached notes"), "{parts:?}");
                assert!(!parts.body_text.contains("--synthetic"), "{parts:?}");
                assert!(parts.body_html.unwrap().contains("Recovered html"));
                let loose = body("Message 2");
                assert!(
                    loose.body_text.contains("Recovered loose text"),
                    "{loose:?}"
                );
                assert_eq!(body("Message 3").body_text.trim(), "hello");
            },
        );
    }

    #[test]
    fn diagnostic_report_separates_syntax_failures_from_server_refusals_without_private_data() {
        use crate::mail::SearchQuery;
        sync_scripted(
            |uid, stream| match uid {
                2 => write_metadata(stream, uid, "PRIVATE_CANARY%bad", " PRIVATE_CANARY"),
                3 => stream
                    .write_all(b"* NO [ALERT] PRIVATE_CANARY cannot fetch\r\n")
                    .unwrap(),
                _ => write_metadata(stream, uid, "NIL", ""),
            },
            |engine, result| {
                let error = result.unwrap_err();
                assert!(matches!(error, MailError::IncompleteSync(2)), "{error:?}");
                assert_eq!(engine.search(&SearchQuery::default()).unwrap().total, 1);
                let report = engine.diagnose_sync().unwrap();
                assert!(report.contains("could not parse 2 message(s)"), "{report}");
                assert!(
                    report.contains("Failure 1 in a folder of type inbox"),
                    "{report}"
                );
                assert!(report.contains("invalid response syntax"), "{report}");
                assert!(report.contains("BODYSTRUCTURE"), "{report}");
                assert!(report.contains("punctuation %_"), "{report}");
                assert!(
                    report.contains("unexpected response: Data status=No code=Alert"),
                    "{report}"
                );
                for private in [
                    "PRIVATE_CANARY",
                    "synthetic@example.test",
                    "Subject",
                    "Message 2",
                ] {
                    assert!(!report.contains(private), "{private} leaked: {report}");
                }
            },
        );
    }
}
