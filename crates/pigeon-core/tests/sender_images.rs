//! Sender icons: what is looked up, what is remembered, and what is not.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pigeon_core::mail::sender_images::IconSource;
use pigeon_core::mail::{MailEngine, MailError};
use pigeon_core::secrets::{KeyringState, MemoryStore};
use pigeon_core::unlock::{UnlockOutcome, open_store};

const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1];
const OTHER_PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 2];
const PNG_URL: &str = "data:image/png;base64,iVBORw0KGgoB";
const OTHER_PNG_URL: &str = "data:image/png;base64,iVBORw0KGgoC";
const DAY: i64 = 24 * 60 * 60;

#[derive(Clone)]
enum Answer {
    Icon(&'static [u8]),
    NoIcon,
    Unreachable,
}

struct Icons {
    answer: Mutex<Answer>,
    asked: Mutex<Vec<String>>,
}

impl Icons {
    fn answer(&self, answer: Answer) {
        *self.answer.lock().unwrap() = answer;
    }
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

impl IconSource for Icons {
    fn fetch(&self, domain: &str) -> Result<Option<Vec<u8>>, MailError> {
        self.asked.lock().unwrap().push(domain.to_owned());
        match self.answer.lock().unwrap().clone() {
            Answer::Icon(data) => Ok(Some(data.to_vec())),
            Answer::NoIcon => Ok(None),
            Answer::Unreachable => Err(MailError::Connection),
        }
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    secrets: Arc<MemoryStore>,
    icons: Arc<Icons>,
    engine: MailEngine,
}

fn fixture(answer: Answer) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mail.db");
    let secrets = Arc::new(MemoryStore::new(KeyringState::Unlocked));
    let icons = Arc::new(Icons {
        answer: Mutex::new(answer),
        asked: Mutex::default(),
    });
    let engine = MailEngine::new(path.clone(), secrets.clone()).with_icon_source(icons.clone());
    Fixture {
        _dir: dir,
        path,
        secrets,
        icons,
        engine,
    }
}

/// Pretends the stored answer for `domain` was reached this many days ago.
fn age(path: &Path, secrets: &MemoryStore, domain: &str, days: i64) {
    let UnlockOutcome::Opened { store, .. } = open_store(secrets, path) else {
        panic!("store should open");
    };
    let changed = store
        .connection()
        .execute(
            "UPDATE sender_images SET checked=checked-?1 WHERE domain=?2",
            rusqlite::params![days * DAY, domain],
        )
        .unwrap();
    assert_eq!(changed, 1);
}

#[test]
fn an_icon_is_asked_for_once_by_registrable_domain() {
    let f = fixture(Answer::Icon(PNG));
    assert_eq!(
        f.engine
            .sender_image("em.example.co.uk")
            .unwrap()
            .as_deref(),
        Some(PNG_URL)
    );
    assert_eq!(
        f.engine
            .sender_image("mail.example.co.uk")
            .unwrap()
            .as_deref(),
        Some(PNG_URL)
    );
    assert_eq!(f.icons.asked(), ["example.co.uk"]);
}

#[test]
fn skipped_domains_never_reach_the_service() {
    let f = fixture(Answer::Icon(PNG));
    for domain in ["gmail.com", "localhost", "10.0.0.1", "pigeon.test", ""] {
        assert_eq!(f.engine.sender_image(domain).unwrap(), None, "{domain:?}");
    }
    assert!(f.icons.asked().is_empty());
}

#[test]
fn having_no_icon_is_remembered_for_a_while() {
    let f = fixture(Answer::NoIcon);
    assert_eq!(f.engine.sender_image("example.com").unwrap(), None);
    assert_eq!(f.engine.sender_image("example.com").unwrap(), None);
    assert_eq!(f.icons.asked().len(), 1);

    age(&f.path, &f.secrets, "example.com", 8);
    f.icons.answer(Answer::Icon(PNG));
    assert_eq!(
        f.engine.sender_image("example.com").unwrap().as_deref(),
        Some(PNG_URL)
    );
    assert_eq!(f.icons.asked().len(), 2);
}

#[test]
fn being_offline_is_not_mistaken_for_having_no_icon() {
    let f = fixture(Answer::Unreachable);
    assert!(matches!(
        f.engine.sender_image("example.com"),
        Err(MailError::Connection)
    ));
    f.icons.answer(Answer::Icon(PNG));
    assert_eq!(
        f.engine.sender_image("example.com").unwrap().as_deref(),
        Some(PNG_URL)
    );
    assert_eq!(f.icons.asked().len(), 2);
}

#[test]
fn an_old_icon_keeps_showing_offline_and_is_replaced_once_reachable() {
    let f = fixture(Answer::Icon(PNG));
    f.engine.sender_image("example.com").unwrap();
    age(&f.path, &f.secrets, "example.com", 31);

    f.icons.answer(Answer::Unreachable);
    assert_eq!(
        f.engine.sender_image("example.com").unwrap().as_deref(),
        Some(PNG_URL)
    );

    f.icons.answer(Answer::Icon(OTHER_PNG));
    assert_eq!(
        f.engine.sender_image("example.com").unwrap().as_deref(),
        Some(OTHER_PNG_URL)
    );
    assert_eq!(f.icons.asked().len(), 3);
}

#[test]
fn a_recent_icon_is_not_asked_for_again() {
    let f = fixture(Answer::Icon(PNG));
    f.engine.sender_image("example.com").unwrap();
    age(&f.path, &f.secrets, "example.com", 29);
    f.engine.sender_image("example.com").unwrap();
    assert_eq!(f.icons.asked().len(), 1);
}

#[test]
fn something_that_is_not_an_image_is_never_shown() {
    let f = fixture(Answer::Icon(b"<svg onload='alert(1)'></svg>"));
    assert_eq!(f.engine.sender_image("example.com").unwrap(), None);
    assert_eq!(f.engine.sender_image("example.com").unwrap(), None);
    assert_eq!(f.icons.asked().len(), 1);
}

#[test]
fn a_locked_store_asks_nothing() {
    let f = fixture(Answer::Icon(PNG));
    f.secrets.set_state(KeyringState::Locked);
    assert!(matches!(
        f.engine.sender_image("example.com"),
        Err(MailError::Locked)
    ));
    assert!(f.icons.asked().is_empty());
}
