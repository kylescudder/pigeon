use pigeon_core::attachment_files;

#[test]
fn imported_bytes_are_a_snapshot_independent_of_the_source_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("receipt.bin");
    std::fs::write(&path, b"receipt\0\xff\r\n").unwrap();
    let attachment = attachment_files::import(&path).unwrap();
    std::fs::write(&path, b"replaced").unwrap();
    assert_eq!(attachment.filename, "receipt.bin");
    assert_eq!(attachment.data, "cmVjZWlwdAD/DQo=");
}

#[test]
fn export_creates_exact_bytes_without_overwriting_existing_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("receipt.bin");
    attachment_files::export_new(&path, b"receipt\0\xff\r\n").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"receipt\0\xff\r\n");
    assert!(attachment_files::export_new(&path, b"replacement").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"receipt\0\xff\r\n");
}

#[test]
fn imports_reject_directories_and_oversize_files() {
    let directory = tempfile::tempdir().unwrap();
    assert!(attachment_files::import(directory.path()).is_err());
    let path = directory.path().join("large.bin");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(pigeon_core::mail::MAX_OUTGOING_ATTACHMENT_BYTES as u64 + 1)
        .unwrap();
    assert!(attachment_files::import(&path).is_err());
}

#[cfg(unix)]
#[test]
fn symlinks_and_dangling_links_are_neither_imported_nor_overwritten() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("original");
    std::fs::write(&target, b"original").unwrap();
    for (name, destination) in [
        ("linked", &target),
        ("dangling", &directory.path().join("missing")),
    ] {
        let link = directory.path().join(name);
        symlink(destination, &link).unwrap();
        assert!(attachment_files::import(&link).is_err());
        assert!(attachment_files::export_new(&link, b"replacement").is_err());
        assert!(link.symlink_metadata().unwrap().is_symlink());
    }
    assert_eq!(std::fs::read(&target).unwrap(), b"original");
    assert!(!directory.path().join("missing").exists());
}

#[cfg(unix)]
#[test]
fn exports_are_private_to_the_owner() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private.bin");
    attachment_files::export_new(&path, b"private").unwrap();
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o077, 0);
}

#[cfg(unix)]
#[test]
fn named_pipes_are_rejected_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pipe");
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `name` is a live NUL-terminated pathname for the duration of mkfifo.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(attachment_files::import(&path).is_err());
    assert!(attachment_files::export_new(&path, b"replacement").is_err());
    assert!(path.exists());
}
