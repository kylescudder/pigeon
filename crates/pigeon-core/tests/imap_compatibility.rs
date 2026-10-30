//! Synthetic protocol fixtures: no user messages or credentials.
#[test]
fn zoho_style_bare_content_id_and_repeated_body_extensions_parse_completely() {
    for structure in [
        r#"("TEXT" "PLAIN" NIL <synthetic@example.test> NIL "7BIT" 6 1)"#,
        r#"("TEXT" "PLAIN" NIL NIL NIL "7BIT" 6 1 NIL NIL NIL NIL "ext1" "ext2")"#,
        r#"(("TEXT" "PLAIN" NIL NIL NIL "7BIT" 6 1) "MIXED" NIL NIL NIL NIL 1 "ext2")"#,
    ] {
        let wire = format!("* 1 FETCH (UID 1 BODYSTRUCTURE {structure})\r\n");
        let (remaining, _) = imap_proto::parse_response(wire.as_bytes())
            .expect("compatible MIME response must parse");
        assert!(remaining.is_empty());
    }
}

#[test]
fn zoho_multipart_without_child_parts_parses_at_any_depth() {
    let empty = r#"("ALTERNATIVE" ("BOUNDARY" "synthetic" "CHARSET" "UTF-8") NIL NIL NIL)"#;
    for structure in [
        empty.to_owned(),
        format!(r#"({empty}("TEXT" "PLAIN" NIL NIL NIL "7BIT" 6 1) "MIXED" NIL NIL NIL NIL)"#),
    ] {
        let wire = format!("* 1 FETCH (UID 1 BODYSTRUCTURE {structure})\r\n");
        let (remaining, _) =
            imap_proto::parse_response(wire.as_bytes()).expect("childless multipart must parse");
        assert!(remaining.is_empty());
    }
}

#[test]
fn zoho_wildcard_flags_parse_in_both_mailbox_and_fetch_responses() {
    for wire in [
        b"* FLAGS (\\Seen \\*)\r\n".as_slice(),
        b"* 1 FETCH (UID 1 FLAGS (\\Seen \\*))\r\n".as_slice(),
    ] {
        let (remaining, _) =
            imap_proto::parse_response(wire).expect("Zoho wildcard flag must parse");
        assert!(remaining.is_empty());
    }
}

#[test]
fn malformed_bodystructure_still_fails_closed() {
    for structure in [
        r#"("TEXT" "PLAIN" NIL NIL NIL "7BIT" not-a-number 1)"#,
        r#"("TEXT" "PLAIN" NIL NIL NIL "7BIT" 6 1 NIL NIL NIL NIL !)"#,
        // Must not be mistaken for a childless multipart: octets are missing.
        r#"("TEXT" "PLAIN" NIL NIL NIL "7BIT")"#,
        r#"("ALTERNATIVE" ("BOUNDARY" "synthetic") !)"#,
    ] {
        let wire = format!("* 1 FETCH (UID 1 BODYSTRUCTURE {structure})\r\n");
        assert!(imap_proto::parse_response(wire.as_bytes()).is_err());
    }
}
