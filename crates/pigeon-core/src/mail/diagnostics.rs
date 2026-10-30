//! Deliberately lossy protocol shape for user-shareable diagnosis. Never emit
//! arbitrary server strings, header literals, numeric values, or custom atoms.
const WORDS: &[&str] = &[
    "FETCH",
    "UID",
    "BODYSTRUCTURE",
    "BODY",
    "HEADER",
    "TEXT",
    "MIME",
    "INTERNALDATE",
    "FLAGS",
    "NIL",
    "OK",
    "NO",
    "BAD",
    "PLAIN",
    "HTML",
    "MULTIPART",
    "MIXED",
    "ALTERNATIVE",
    "RELATED",
    "MESSAGE",
    "RFC822",
    "APPLICATION",
    "PDF",
    "OCTET-STREAM",
    "IMAGE",
    "PNG",
    "JPEG",
    "CHARSET",
    "UTF-8",
    "US-ASCII",
    "NAME",
    "FILENAME",
    "ATTACHMENT",
    "INLINE",
    "BASE64",
    "QUOTED-PRINTABLE",
    "7BIT",
    "8BIT",
    "BINARY",
];

// Which characters a redacted value contains, not what it says: control bytes
// and atom punctuation are what make a server's syntax unparseable.
const MAX_TOKENS: usize = 2_000;

fn word(bytes: &[u8], atom: bool) -> String {
    if let Some(known) = WORDS
        .iter()
        .find(|known| bytes.eq_ignore_ascii_case(known.as_bytes()))
    {
        return (*known).into();
    }
    let encoding = match std::str::from_utf8(bytes) {
        Ok(text) if text.is_ascii() => "ascii",
        Ok(_) => "utf8",
        Err(_) => "non-utf8",
    };
    let control = if bytes.iter().any(u8::is_ascii_control) {
        ";control"
    } else {
        ""
    };
    let mut punctuation: Vec<char> = bytes
        .iter()
        .filter(|byte| atom && byte.is_ascii_punctuation())
        .map(|byte| char::from(*byte))
        .collect();
    punctuation.sort_unstable();
    punctuation.dedup();
    let punctuation = if punctuation.is_empty() {
        String::new()
    } else {
        format!(
            ";punctuation {}",
            punctuation.into_iter().collect::<String>()
        )
    };
    format!("redacted[{};{encoding}{control}{punctuation}]", bytes.len())
}

pub(super) fn shape(raw: &[u8]) -> String {
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut literals = 0;
    // Bound processing/output even for malformed or malicious server data, but
    // let omitted literals pass: the failure usually follows the header literal.
    while index < raw.len() && tokens.len() < MAX_TOKENS && index - literals < 65_536 {
        let byte = raw[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        match byte {
            b'{' => {
                let start = index + 1;
                let mut end = start;
                while end < raw.len() && raw[end].is_ascii_digit() {
                    end += 1;
                }
                if end == start || raw.get(end..end + 3) != Some(b"}\r\n") {
                    tokens.push("invalid-literal; remainder omitted".into());
                    break;
                }
                let length = std::str::from_utf8(&raw[start..end])
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok());
                let Some(next) = length
                    .and_then(|length| (end + 3).checked_add(length))
                    .filter(|next| *next <= raw.len())
                else {
                    tokens.push("literal not received, the parser stopped before it".into());
                    break;
                };
                tokens.push("{literal omitted}".into());
                literals += next - index;
                index = next;
            }
            b'"' => {
                index += 1;
                let mut value = Vec::new();
                let mut closed = false;
                while index < raw.len() && value.len() < 65_536 {
                    let byte = raw[index];
                    index += 1;
                    if byte == b'"' {
                        closed = true;
                        break;
                    }
                    if byte == b'\\' && index < raw.len() {
                        value.push(raw[index]);
                        index += 1;
                    } else {
                        value.push(byte);
                    }
                }
                tokens.push(format!("\"{}\"", word(&value, false)));
                if !closed {
                    tokens.push("unterminated-string; remainder omitted".into());
                    break;
                }
            }
            b'(' | b')' | b'[' | b']' | b'<' | b'>' => {
                tokens.push(char::from(byte).to_string());
                index += 1;
            }
            _ => {
                let start = index;
                while index < raw.len()
                    && !raw[index].is_ascii_whitespace()
                    && !b"()[]<>{\"".contains(&raw[index])
                    && index - start < 65_536
                {
                    index += 1;
                }
                if index == start {
                    tokens.push("invalid-token".into());
                    index += 1;
                } else if raw[start..index].iter().all(u8::is_ascii_digit) {
                    tokens.push(format!("number[{} digits]", index - start));
                } else {
                    tokens.push(word(&raw[start..index], true));
                }
            }
        }
    }
    if index < raw.len() && tokens.len() >= MAX_TOKENS {
        tokens.push("truncated".into());
    }
    tokens.join(" ")
}

// The library's text is a Debug dump of the whole response, including server
// prose. Keep only Rust identifiers from fixed positions, never field values.
fn unexpected(debug: &str) -> String {
    let identifier = |text: &str| {
        text.chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>()
    };
    let field = |name: &str| {
        debug
            .split_once(name)
            .map(|(_, rest)| identifier(rest))
            .unwrap_or_default()
    };
    let variant = identifier(debug);
    if variant != "Data" {
        return variant;
    }
    let code = debug
        .split_once("code: Some(")
        .map(|(_, rest)| identifier(rest))
        .unwrap_or_else(|| "none".into());
    format!("Data status={} code={code}", field("status: "))
}

pub(super) fn report(error: &::imap::Error) -> Option<String> {
    use ::imap::error::ParseError;
    match error {
        ::imap::Error::Parse(ParseError::Invalid(raw)) => {
            Some(format!("invalid response syntax\nshape: {}", shape(raw)))
        }
        ::imap::Error::Parse(ParseError::DataNotUtf8(raw, _)) => {
            Some(format!("non-UTF-8 response data\nshape: {}", shape(raw)))
        }
        ::imap::Error::Parse(ParseError::Unexpected(debug)) => {
            Some(format!("unexpected response: {}", unexpected(debug)))
        }
        ::imap::Error::Parse(ParseError::Authentication(_, _)) => {
            Some("invalid authentication response (contents omitted)".into())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_fields_and_literals_are_omitted_but_protocol_shape_remains() {
        let header = b"Subject: PRIVATE_HEADER\r\nFrom: secret@example.test\r\n\r\n";
        let mut raw =
            format!("* 127 FETCH (UID 999 BODY[HEADER] {{{}}}\r\n", header.len()).into_bytes();
        raw.extend_from_slice(header);
        raw.extend_from_slice(b" BODYSTRUCTURE (\"TEXT\" \"PLAIN\" (\"NAME\" \"PRIVATE_FILENAME\") <PRIVATE_ID@example.test> NIL \"7BIT\" 1234567890 1 NIL))\r\n");
        let output = shape(&raw);
        for secret in [
            "PRIVATE_HEADER",
            "secret@example.test",
            "PRIVATE_FILENAME",
            "PRIVATE_ID",
            "1234567890",
            "999",
        ] {
            assert!(!output.contains(secret), "{output}");
        }
        assert!(output.contains("BODYSTRUCTURE"));
        assert!(output.contains("{literal omitted}"));
        assert!(output.contains("number[10 digits]"));
    }
    #[test]
    fn long_structures_after_large_literals_keep_their_failure_point() {
        let mut raw = b"* 1 FETCH (BODY[HEADER] {200000}\r\n".to_vec();
        raw.extend_from_slice(&vec![b'x'; 200_000]);
        raw.extend_from_slice(" NIL".repeat(500).as_bytes());
        raw.extend_from_slice(b" MARKER%@.\r\n");
        let output = shape(&raw);
        assert!(
            output.contains("redacted[9;ascii;punctuation %.@]"),
            "{output}"
        );
    }
    #[test]
    fn unexpected_responses_keep_only_variant_status_and_code() {
        let debug = r#"Data { status: No, code: Some(Alert), information: Some("PRIVATE_PROSE status: Ok") }"#;
        assert_eq!(unexpected(debug), "Data status=No code=Alert");
        let debug = r#"Data { status: Bad, code: None, information: Some("PRIVATE_PROSE code: Some(Secret)") }"#;
        assert!(!unexpected(debug).contains("PRIVATE"));
        assert_eq!(unexpected(r#"Fetch(1, [PRIVATE])"#), "Fetch");
    }
    #[test]
    fn malformed_strings_and_literals_do_not_leak_remainders() {
        for raw in [
            b"{999999999999999999999999999}\r\nPRIVATE_REMAINDER".as_slice(),
            b"{4x}\r\nPRIVATE_REMAINDER".as_slice(),
            b"\"PRIVATE_REMAINDER".as_slice(),
        ] {
            assert!(!shape(raw).contains("PRIVATE_REMAINDER"));
        }
        assert!(shape(b"(\"NAME\" \"\xff\xfe\")").contains("non-utf8"));
        assert!(shape(&b"()".repeat(100_000)).len() < 4096);
    }
}
