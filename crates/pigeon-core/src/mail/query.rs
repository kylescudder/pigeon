use super::{MailError, SearchQuery};
use rusqlite::types::Value;

pub(super) struct SearchPlan {
    pub filters: Vec<String>,
    pub values: Vec<Value>,
    pub text: String,
}

pub(super) fn compile(query: &SearchQuery) -> Result<SearchPlan, MailError> {
    if query.text.len() > 4096 {
        return Err(MailError::InvalidSearch(
            "use a query shorter than 4096 characters".into(),
        ));
    }
    let mut plan = SearchPlan {
        filters: Vec::new(),
        values: Vec::new(),
        text: String::new(),
    };
    if let Some(account) = &query.account_id {
        plan.filters.push("m.account=?".into());
        plan.values.push(account.clone().into());
    }
    if let Some(folder) = &query.folder {
        plan.filters.push("m.folder=?".into());
        plan.values.push(folder.clone().into());
    }
    if query.unread_only {
        plan.filters.push("m.unread=1".into());
    }
    if query.has_attachments {
        plan.filters.push("m.has_attachments=1".into());
    }
    let mut include_junk = query.include_junk_trash || query.folder.is_some();
    let mut text = Vec::new();
    for token in tokens(&query.text)? {
        if let Some((field, raw)) = token.split_once(':') {
            let value = unquote(raw);
            match field.to_ascii_lowercase().as_str() {
                "from" | "to" | "subject" => {
                    if value.is_empty() {
                        return Err(MailError::InvalidSearch(format!("{field}: needs a value")));
                    }
                    let column = match field.to_ascii_lowercase().as_str() {
                        "from" => "sender",
                        "to" => "recipients",
                        _ => "subject",
                    };
                    plan.filters.push(format!("m.{column} LIKE ? ESCAPE '\\'"));
                    plan.values.push(like(&value).into());
                    continue;
                }
                "account" => {
                    plan.filters.push("m.account IN (SELECT id FROM mail_accounts WHERE id=? OR json_extract(config,'$.name') LIKE ? ESCAPE '\\' OR json_extract(config,'$.address') LIKE ? ESCAPE '\\')".into());
                    plan.values.extend([
                        Value::Text(value.clone()),
                        Value::Text(like(&value)),
                        Value::Text(like(&value)),
                    ]);
                    continue;
                }
                "folder" => {
                    plan.filters.push("m.folder LIKE ? ESCAPE '\\'".into());
                    plan.values.push(like(&value).into());
                    include_junk = true;
                    continue;
                }
                "is" => {
                    match value.as_str() {
                        "unread" => plan.filters.push("m.unread=1".into()),
                        "read" => plan.filters.push("m.unread=0".into()),
                        _ => {
                            return Err(MailError::InvalidSearch(
                                "use is:unread or is:read".into(),
                            ));
                        }
                    };
                    continue;
                }
                "has" => {
                    if value != "attachment" && value != "attachments" {
                        return Err(MailError::InvalidSearch("use has:attachment".into()));
                    }
                    plan.filters.push("m.has_attachments=1".into());
                    continue;
                }
                "after" | "before" => {
                    let date = chrono::NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                        .map_err(|_| MailError::InvalidSearch("dates use YYYY-MM-DD".into()))?;
                    let timestamp = date
                        .and_hms_opt(0, 0, 0)
                        .and_then(|date| date.and_local_timezone(chrono::Local).earliest())
                        .ok_or_else(|| {
                            MailError::InvalidSearch(
                                "date cannot be interpreted in the local timezone".into(),
                            )
                        })?
                        .timestamp();
                    plan.filters.push(if field.eq_ignore_ascii_case("after") {
                        "m.date>=?".into()
                    } else {
                        "m.date<?".into()
                    });
                    plan.values.push(timestamp.into());
                    continue;
                }
                _ => {}
            }
        }
        if ["AND", "OR", "NOT", "(", ")"].contains(&token.as_str()) {
            text.push(token);
        } else {
            let opening = token.chars().take_while(|value| *value == '(').count();
            let closing = token
                .chars()
                .rev()
                .take_while(|value| *value == ')')
                .count();
            let middle = &token[opening..token.len().saturating_sub(closing)];
            let prefix = middle.ends_with('*');
            let value = unquote(middle.trim_end_matches('*'));
            if !value.is_empty() {
                text.push(format!(
                    "{}\"{}\"{}{}",
                    "(".repeat(opening),
                    value.replace('"', "\"\""),
                    if prefix { "*" } else { "" },
                    ")".repeat(closing)
                ));
            }
        }
    }
    if !include_junk {
        plan.filters.push("f.kind NOT IN ('junk','trash')".into());
    }
    plan.text = text.join(" ");
    Ok(plan)
}

fn like(value: &str) -> String {
    format!(
        "%{}%",
        value
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}
fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .replace("\\\"", "\"")
        .replace("\\\\", "\\")
}

fn tokens(query: &str) -> Result<Vec<String>, MailError> {
    let mut quoted = false;
    let mut escaped = false;
    let mut token = String::new();
    let mut tokens = Vec::new();
    for character in query.chars() {
        if character == '"' && !escaped {
            quoted = !quoted;
        }
        if character.is_whitespace() && !quoted {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
        escaped = character == '\\' && !escaped;
    }
    if quoted {
        return Err(MailError::InvalidSearch("close the quoted phrase".into()));
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Ok(tokens)
}
