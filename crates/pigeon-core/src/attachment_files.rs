//! Explicit file transfers across the encrypted-store boundary. The desktop
//! shell supplies paths from native choosers; none of these paths cross IPC.

use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

use crate::mail::{DraftAttachment, MAX_OUTGOING_ATTACHMENT_BYTES, MailError};

pub fn import(path: &Path) -> Result<DraftAttachment, MailError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Check the object actually opened, not metadata that may be stale by
        // the time open runs. NONBLOCK prevents a substituted pipe from hanging.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| invalid_file())?;
    let metadata = file.metadata().map_err(|_| invalid_file())?;
    if !metadata.is_file() {
        return Err(invalid_file());
    }
    if metadata.len() > MAX_OUTGOING_ATTACHMENT_BYTES as u64 {
        return Err(MailError::InvalidConfig(
            "attachments must total at most 20 MiB".into(),
        ));
    }
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid_file)?;
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    // A file can grow after stat; keep the read bounded and validate again.
    file.take(MAX_OUTGOING_ATTACHMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid_file())?;
    DraftAttachment::from_bytes(filename, &bytes)
}

fn invalid_file() -> MailError {
    MailError::InvalidConfig(
        "could not read the chosen file; choose a regular local file, not a link or directory"
            .into(),
    )
}

/// Export only to a new file. `create_new` refuses existing files and final
/// symlinks atomically, including dangling symlinks. No plaintext cache is used.
pub fn export_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}
