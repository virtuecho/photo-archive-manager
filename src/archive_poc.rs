use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use sevenz_rust2::{
    Archive, ArchiveEntry, ArchiveReader, ArchiveWriter, EncoderMethod, Password,
    encoder_options::AesEncoderOptions,
};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

fn temp_dir() -> PathBuf {
    let suffix = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "photo-archive-manager-7z-poc-{}-{suffix}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn encrypted_store_7z_round_trips_unicode_path_and_password() {
    const PASSWORD_TEXT: &str = "照片备份-密碼-123";
    let root = temp_dir();
    let source = root.join("RAW/相机.CR3");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let source_bytes = b"store without recompression";
    File::create(&source)
        .unwrap()
        .write_all(source_bytes)
        .unwrap();
    let archive_path = root.join("RAW.7z");

    let mut writer = ArchiveWriter::create(&archive_path).unwrap();
    writer.set_content_methods(vec![
        AesEncoderOptions::new(PASSWORD_TEXT.into()).into(),
        EncoderMethod::COPY.into(),
    ]);
    writer
        .push_archive_entry::<&[u8]>(ArchiveEntry::new_directory("RAW"), None)
        .unwrap();
    writer
        .push_archive_entry(
            ArchiveEntry::new_file("RAW/相机.CR3"),
            Some(File::open(&source).unwrap()),
        )
        .unwrap();
    writer.finish().unwrap();

    let mut raw_archive = File::open(&archive_path).unwrap();
    assert!(Archive::read(&mut raw_archive, &Password::empty()).is_err());

    assert!(ArchiveReader::open(&archive_path, Password::from("wrong")).is_err());
    let mut reader = ArchiveReader::open(&archive_path, Password::from(PASSWORD_TEXT)).unwrap();
    assert_eq!(reader.read_file("RAW/相机.CR3").unwrap(), source_bytes);
    let mut methods = Vec::new();
    reader
        .file_compression_methods("RAW/相机.CR3", &mut methods)
        .unwrap();
    assert!(methods.contains(&EncoderMethod::COPY));

    let archive = Archive::read(
        &mut File::open(&archive_path).unwrap(),
        &Password::from(PASSWORD_TEXT),
    )
    .unwrap();
    let names: Vec<_> = archive.files.iter().map(ArchiveEntry::name).collect();
    assert_eq!(names, ["RAW", "RAW/相机.CR3"]);

    fs::remove_dir_all(root).unwrap();
}
