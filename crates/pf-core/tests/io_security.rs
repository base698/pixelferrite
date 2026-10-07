use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pf_core::io::{self, atomic, limits, ora};
use pf_core::{Document, Mask, Pixmap, composite};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

struct Temp(PathBuf);
impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "pf-io-{name}-{}-{}",
            std::process::id(),
            pf_core::document::next_id()
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    io::encode_png(&Pixmap::filled(w, h, [255, 0, 0, 255])).unwrap()
}
fn archive(path: &Path, xml: &str, entries: &[(&str, &[u8])]) {
    let mut zip = ZipWriter::new(fs::File::create(path).unwrap());
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("mimetype", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"image/openraster").unwrap();
    zip.start_file("stack.xml", options).unwrap();
    zip.write_all(xml.as_bytes()).unwrap();
    for (name, bytes) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
}
fn simple_xml(w: u32, h: u32, attributes: &str) -> String {
    format!(
        "<image w=\"{w}\" h=\"{h}\"><stack><layer src=\"data/p.png\" {attributes}/></stack></image>"
    )
}
fn error(path: &Path) -> String {
    match ora::load(path) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("malformed fixture unexpectedly opened: {}", path.display()),
    }
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320u32 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}
fn declared_png(w: u32, h: u32) -> Vec<u8> {
    let mut bytes = png(1, 1);
    bytes[16..20].copy_from_slice(&w.to_be_bytes());
    bytes[20..24].copy_from_slice(&h.to_be_bytes());
    let crc = crc32(&bytes[12..29]);
    bytes[29..33].copy_from_slice(&crc.to_be_bytes());
    bytes
}

#[test]
fn tiny_ora_rejects_absurd_canvas_and_invalid_coordinates() {
    let dir = Temp::new("dimensions");
    let path = dir.path("bad.ora");
    let pixel = png(1, 1);
    for (w, h) in [
        (0, 1),
        (1, 0),
        (1_000_000, 1_000_000),
        (limits::MAX_DIMENSION, limits::MAX_DIMENSION),
    ] {
        archive(&path, &simple_xml(w, h, ""), &[("data/p.png", &pixel)]);
        assert!(fs::metadata(&path).unwrap().len() < 1024);
        assert!(error(&path).contains("dimensions"));
    }
    for attributes in [
        "x=\"2147483647\"",
        "y=\"NaN\"",
        "opacity=\"NaN\"",
        "opacity=\"2\"",
        "composite-op=\"unknown\"",
    ] {
        archive(
            &path,
            &simple_xml(1, 1, attributes),
            &[("data/p.png", &pixel)],
        );
        error(&path);
    }
}

#[test]
fn flat_and_memory_decoders_reject_large_headers_before_pixel_decode() {
    let dir = Temp::new("flat");
    for (w, h) in [
        (limits::MAX_DIMENSION + 1, 1),
        (limits::MAX_DIMENSION, limits::MAX_DIMENSION),
    ] {
        let bytes = declared_png(w, h);
        assert!(bytes.len() < 256);
        assert!(io::decode_image(&bytes).is_err());
        let path = dir.path("bad.png");
        fs::write(&path, &bytes).unwrap();
        assert!(io::load_pixmap(&path).is_err());
    }
    let bytes = png(3, 2);
    assert_eq!(
        (
            io::decode_image(&bytes).unwrap().w,
            io::decode_image(&bytes).unwrap().h
        ),
        (3, 2)
    );
}

#[test]
fn archive_limits_cover_xml_layers_entries_and_declared_expansion() {
    let dir = Temp::new("zip-limits");
    let path = dir.path("bad.ora");
    let pixel = png(1, 1);
    let xml = format!(
        "{}{}",
        simple_xml(1, 1, ""),
        " ".repeat(limits::MAX_XML_BYTES as usize)
    );
    archive(&path, &xml, &[("data/p.png", &pixel)]);
    assert!(error(&path).contains("expanded-size"));
    let xml = format!(
        "<image w=\"1\" h=\"1\"><stack>{}</stack></image>",
        "<layer src=\"data/p.png\"/>".repeat(limits::MAX_LAYERS + 1)
    );
    archive(&path, &xml, &[("data/p.png", &pixel)]);
    assert!(error(&path).contains("layer limit"));

    let mut zip = ZipWriter::new(fs::File::create(&path).unwrap());
    for i in 0..=limits::MAX_ARCHIVE_ENTRIES {
        zip.start_file(format!("entry{i}"), SimpleFileOptions::default())
            .unwrap();
    }
    zip.finish().unwrap();
    assert!(error(&path).contains("directory exceeds"));

    archive(&path, &simple_xml(1, 1, ""), &[("data/p.png", &pixel)]);
    let mut bytes = fs::read(&path).unwrap();
    // Malicious central-directory size advertising a huge compressed member.
    let pos = bytes
        .windows(4)
        .enumerate()
        .filter(|(_, v)| *v == b"PK\x01\x02")
        .last()
        .unwrap()
        .0;
    bytes[pos + 24..pos + 28]
        .copy_from_slice(&((limits::MAX_ENCODED_IMAGE_BYTES + 1) as u32).to_le_bytes());
    fs::write(&path, &bytes).unwrap();
    assert!(error(&path).contains("expanded-size"));
}

#[test]
fn repeated_small_compressed_layer_cannot_evade_total_pixel_budget() {
    let dir = Temp::new("aggregate");
    let path = dir.path("layers.ora");
    let pixel = png(1024, 1024);
    let layers = limits::MAX_DOCUMENT_PIXELS / (1024 * 1024) + 1;
    let xml = format!(
        "<image w=\"1\" h=\"1\"><stack>{}</stack></image>",
        "<layer src=\"data/p.png\"/>".repeat(layers as usize)
    );
    archive(&path, &xml, &[("data/p.png", &pixel)]);
    assert!(error(&path).contains("total decoded"));
}

#[test]
fn grouped_and_effectful_stacks_are_rejected_without_changing_the_file() {
    let dir = Temp::new("groups");
    let path = dir.path("group.ora");
    let pixel = png(1, 1);
    for contents in [
        "<stack><stack visibility=\"hidden\"><layer src=\"data/p.png\"/></stack><layer src=\"data/p.png\"/></stack>",
        "<stack><stack opacity=\"0.5\"><layer src=\"data/p.png\"/></stack></stack>",
        "<stack opacity=\"0.5\"><layer src=\"data/p.png\"/></stack>",
        "<stack x=\"2\"><layer src=\"data/p.png\"/></stack>",
    ] {
        archive(
            &path,
            &format!("<image w=\"1\" h=\"1\">{contents}</image>"),
            &[("data/p.png", &pixel)],
        );
        let before = fs::read(&path).unwrap();
        assert!(error(&path).contains("this file was not changed"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn imported_text_metadata_is_validated_before_future_edits() {
    let dir = Temp::new("text");
    let path = dir.path("text.ora");
    let pixel = png(1, 1);
    for extra in [
        "pixelferrite-text-size=\"inf\"",
        "pixelferrite-text-path=\"NaN,2\"",
        "pixelferrite-text-leading=\"100000\"",
        "pixelferrite-text-offset=\"1e30\"",
    ] {
        let attrs = format!("pixelferrite-text=\"abc\" pixelferrite-text-raster=\"0,0\" {extra}");
        archive(&path, &simple_xml(1, 1, &attrs), &[("data/p.png", &pixel)]);
        assert!(error(&path).contains("text metadata"));
    }
}

#[test]
fn compatible_ora_bakes_masks_while_native_save_retains_editability() {
    let dir = Temp::new("compatible");
    let mut doc = Document::new(3, 1, None);
    let layer = &mut doc.state.layers[0];
    layer.pixels = Arc::new(Pixmap::filled(3, 1, [255, 0, 0, 255]));
    layer.mask = Some(Arc::new(Mask::from_raw(3, 1, vec![0, 128, 255])));
    let expected = composite::flatten(&doc.state).data;
    let native = dir.path("native.ora");
    ora::save(&doc.state, &native).unwrap();
    let back = ora::load(&native).unwrap();
    assert!(back.layers[0].mask.is_some());
    assert_eq!(composite::flatten(&back).data, expected);
    let compatible = dir.path("compatible.ora");
    io::export(&doc.state, &compatible).unwrap();
    let exported = ora::load(&compatible).unwrap();
    assert!(exported.layers[0].mask.is_none());
    assert_eq!(composite::flatten(&exported).data, expected);
    assert!(doc.state.layers[0].mask.is_some());
    let mut zip = ZipArchive::new(Cursor::new(fs::read(compatible).unwrap())).unwrap();
    let mut xml = String::new();
    zip.by_name("stack.xml")
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    assert!(!xml.contains("pixelferrite-mask"));
    assert!(!xml.contains("pixelferrite-text"));
}

#[test]
fn failed_or_abandoned_atomic_write_preserves_destination_and_cleans_temporary() {
    let dir = Temp::new("atomic-failure");
    let destination = dir.path("original.txt");
    fs::write(&destination, b"original").unwrap();
    {
        let mut temp = atomic::AtomicFile::new(&destination, false).unwrap();
        temp.file().write_all(b"partial replacement").unwrap();
    }
    assert_eq!(fs::read(&destination).unwrap(), b"original");
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    let subdirectory = dir.path("destination-directory");
    fs::create_dir(&subdirectory).unwrap();
    let mut temp = atomic::AtomicFile::new(&subdirectory, false).unwrap();
    temp.file().write_all(b"failed").unwrap();
    assert!(temp.commit().is_err());
    assert!(subdirectory.is_dir());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
}

#[test]
fn simultaneous_atomic_writers_use_distinct_files() {
    let dir = Temp::new("atomic-two");
    let destination = dir.path("same.txt");
    let mut a = atomic::AtomicFile::new(&destination, false).unwrap();
    let mut b = atomic::AtomicFile::new(&destination, false).unwrap();
    a.file().write_all(b"first complete value").unwrap();
    b.file().write_all(b"second complete value").unwrap();
    a.commit().unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"first complete value");
    b.commit().unwrap();
    assert_eq!(fs::read(&destination).unwrap(), b"second complete value");
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[cfg(unix)]
#[test]
fn ora_save_does_not_follow_old_temporary_symlink_and_private_files_start_private() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = Temp::new("symlink");
    let victim = dir.path("victim.txt");
    fs::write(&victim, b"private marker").unwrap();
    let destination = dir.path("document.ora");
    symlink(&victim, destination.with_extension("ora.tmp")).unwrap();
    let doc = Document::new(1, 1, Some([255; 4]));
    ora::save(&doc.state, &destination).unwrap();
    assert_eq!(fs::read(&victim).unwrap(), b"private marker");
    ora::load(&destination).unwrap();
    let private_path = dir.path("private/settings.toml");
    let mut private = atomic::AtomicFile::new(&private_path, true).unwrap();
    assert_eq!(
        private.file().metadata().unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(private);
    assert_eq!(
        fs::metadata(private_path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    ora::save_private(&doc.state, &private_path.with_extension("ora")).unwrap();
    assert_eq!(
        fs::metadata(private_path.with_extension("ora"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let symlink_dir = dir.path("alias");
    symlink(private_path.parent().unwrap(), &symlink_dir).unwrap();
    assert!(atomic::create_private_dir(&symlink_dir).is_err());
    let leaf = dir.path("leaf.txt");
    symlink(&victim, &leaf).unwrap();
    atomic::write(&leaf, b"new value", false).unwrap();
    assert_eq!(fs::read(&victim).unwrap(), b"private marker");
    assert_eq!(fs::read(&leaf).unwrap(), b"new value");
}

/// A deterministic malformed-input sweep complements the targeted fixtures.
/// Seeds and mutations stay tiny; accepted documents must still meet every
/// editor limit. This is a regression corpus, not a claim of exhaustive fuzzing.
#[test]
fn malformed_small_ora_and_png_mutations_do_not_panic() {
    let dir = Temp::new("mutations");
    let path = dir.path("mutated.ora");
    let png = png(2, 2);
    archive(&path, &simple_xml(2, 2, ""), &[("data/p.png", &png)]);
    let original = fs::read(&path).unwrap();
    let mut cases = Vec::new();
    for seed in [&original, &png] {
        for end in (0..seed.len()).step_by(3) {
            cases.push((std::ptr::eq(seed, &original), seed[..end].to_vec()));
        }
        for i in 0..seed.len() {
            let mut bytes = seed.clone();
            bytes[i] ^= 0xff;
            cases.push((std::ptr::eq(seed, &original), bytes));
        }
    }
    for (i, (is_ora, bytes)) in cases.into_iter().enumerate() {
        if is_ora {
            fs::write(&path, &bytes).unwrap();
            let result = std::panic::catch_unwind(|| ora::load(&path));
            assert!(result.is_ok(), "ORA mutation {i} panicked");
            if let Ok(state) = result.unwrap() {
                limits::validate_document(&state).unwrap();
            }
            assert_eq!(fs::read(&path).unwrap(), bytes);
        } else {
            let result = std::panic::catch_unwind(|| io::decode_image(&bytes));
            assert!(result.is_ok(), "PNG mutation {i} panicked");
            if let Ok(pixels) = result.unwrap() {
                limits::validate_dimensions(pixels.w, pixels.h).unwrap();
            }
        }
    }
}

#[test]
fn save_rejects_unreadable_metadata_without_replacing_existing_file() {
    let dir = Temp::new("save-metadata");
    let path = dir.path("original.ora");
    let mut doc = Document::new(1, 1, None);
    ora::save(&doc.state, &path).unwrap();
    let original = fs::read(&path).unwrap();
    doc.state.layers[0].name = "x".repeat(limits::MAX_XML_BYTES as usize);
    assert!(
        ora::save(&doc.state, &path)
            .unwrap_err()
            .to_string()
            .contains("XML-size")
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    doc.state.layers[0].name = "bad\u{1}name".to_owned();
    assert!(ora::save(&doc.state, &path).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}
