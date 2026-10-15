use std::fs::{self, FileTimes};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output};

use gloob::media::cache_gif;
use tempfile::TempDir;
use url::Url;

mod common;

fn gif_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut bytes, 2, 1, &[0, 0, 0, 255, 255, 255]).unwrap();
        for pixels in [[0, 1], [1, 0]] {
            let frame = gif::Frame {
                width: 2,
                height: 1,
                delay: 10,
                buffer: pixels.to_vec().into(),
                ..Default::default()
            };
            encoder.write_frame(&frame).unwrap();
        }
    }
    bytes
}

#[test]
fn webp_copies_keep_original_bytes_and_detect_format_not_extension() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("misleading.gif");
    for bytes in [
        common::animation(2, 1, 2),
        common::still(2, 1, [20, 40, 60, 255]),
    ] {
        fs::write(&source, &bytes).unwrap();
        let output = copy_command(
            root.path(),
            &source,
            Some("#!/bin/sh\n/bin/cat > \"$CAPTURE/uri\"\n"),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let path = Url::parse(fs::read_to_string(root.path().join("uri")).unwrap().trim())
            .unwrap()
            .to_file_path()
            .unwrap();
        assert_eq!(path.extension().unwrap(), "webp");
        assert_eq!(fs::read(&path).unwrap(), bytes);
        let cached = gloob::media::cache_bytes(&bytes, path.parent().unwrap()).unwrap();
        assert_eq!(cached.path(), path);
    }
}

#[test]
fn rejects_bad_webp_before_cache_or_clipboard_publication() {
    let root = TempDir::new().unwrap();
    let bytes = common::animation(2, 1, 2);
    let mut truncated = bytes[..bytes.len() - 2].to_vec();
    let size = (truncated.len() - 8) as u32;
    truncated[4..8].copy_from_slice(&size.to_le_bytes());
    let mut corrupt_later = bytes.clone();
    let last_image = corrupt_later
        .windows(4)
        .rposition(|tag| tag == b"VP8L")
        .unwrap();
    corrupt_later[last_image + 8] = 0;
    let mut huge = bytes.clone();
    huge[24..27].copy_from_slice(&common::u24(16_000));
    huge[27..30].copy_from_slice(&common::u24(16_000));
    for bytes in [
        truncated,
        corrupt_later,
        huge,
        common::animation(1, 1, 1_001),
        common::animation(1024, 1024, 96),
    ] {
        let cache = root.path().join("cache");
        assert!(gloob::media::cache_bytes(&bytes, &cache).is_err());
        assert!(!cache.exists());
    }
}

#[test]
fn mixed_gif_and_webp_selections_share_ten_file_retention() {
    let root = TempDir::new().unwrap();
    let source = root.path().join("source");
    let mut paths = Vec::new();
    for index in 0..12u8 {
        let bytes = if index % 2 == 0 {
            common::still(1, 1, [index, 0, 0, 255])
        } else {
            let mut bytes = gif_bytes();
            bytes.pop();
            bytes.extend_from_slice(&[0x21, 0xfe, 1, index, 0, 0x3b]);
            bytes
        };
        fs::write(&source, &bytes).unwrap();
        let output = copy_command(
            root.path(),
            &source,
            Some("#!/bin/sh\n/bin/cat > \"$CAPTURE/uri\"\n"),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        paths.push(
            Url::parse(fs::read_to_string(root.path().join("uri")).unwrap().trim())
                .unwrap()
                .to_file_path()
                .unwrap(),
        );
    }
    assert!(!paths[0].exists() && !paths[1].exists());
    assert!(paths[2..].iter().all(|path| path.exists()));
    let index: Vec<String> = serde_json::from_slice(
        &fs::read(paths[11].parent().unwrap().join(".recency.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(index.len(), 11);
    assert_eq!(
        index.iter().filter(|name| name.ends_with(".webp")).count(),
        5
    );
    // the index includes the last evicted entry so interrupted pruning can be retried.
    assert_eq!(paths[11].extension().unwrap(), "gif");
}

fn source(directory: &Path) -> std::path::PathBuf {
    let path = directory.join("source.gif");
    fs::write(&path, gif_bytes()).unwrap();
    path
}

#[test]
fn cached_asset_survives_source_removal_and_is_reused_by_content() {
    let root = TempDir::new().unwrap();
    let source = source(root.path());
    let cache = root.path().join("cache");
    let first = cache_gif(&source, &cache).unwrap();
    let old_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
    fs::File::options()
        .write(true)
        .open(first.path())
        .unwrap()
        .set_times(FileTimes::new().set_modified(old_time))
        .unwrap();
    let before = fs::metadata(first.path()).unwrap();
    let renamed = root.path().join("different-name.gif");
    fs::rename(&source, &renamed).unwrap();
    let second = cache_gif(&renamed, &cache).unwrap();
    assert_eq!(first.path(), second.path());
    let after = fs::metadata(second.path()).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(after.modified().unwrap(), old_time);
    fs::remove_file(renamed).unwrap();
    assert_eq!(fs::read(first.path()).unwrap(), gif_bytes());
    assert_eq!(fs::read_dir(cache).unwrap().count(), 1);
}

#[test]
fn rejects_bad_assets_without_creating_cache_entries() {
    let root = TempDir::new().unwrap();
    let cache = root.path().join("cache");
    let path = root.path().join("bad.gif");
    for bytes in [
        b"not a gif".to_vec(),
        b"GIF89a;".to_vec(),
        gif_bytes()[..20].to_vec(),
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(cache_gif(&path, &cache).is_err());
        assert!(!cache.exists());
    }
    assert!(cache_gif(&root.path().join("missing.gif"), &cache).is_err());
    assert!(cache_gif(root.path(), &cache).is_err());
    let mut oversized = gif_bytes();
    oversized.pop();
    oversized.extend_from_slice(&[0x21, 0xfe]);
    for _ in 0..(25 * 1024 * 1024 / 255 + 1) {
        oversized.push(255);
        oversized.extend_from_slice(&[0; 255]);
    }
    oversized.extend_from_slice(&[0, 0x3b]);
    fs::write(&path, oversized).unwrap();
    let error = cache_gif(&path, &cache).err().unwrap();
    assert!(error.to_string().contains("size limit"));
    assert!(!cache.exists());
}

#[test]
fn refuses_damaged_or_symlinked_cached_assets() {
    let root = TempDir::new().unwrap();
    let source = source(root.path());
    let cache = root.path().join("cache");
    let cached = cache_gif(&source, &cache).unwrap();
    fs::write(cached.path(), "damaged").unwrap();
    assert!(cache_gif(&source, &cache).is_err());
    assert_eq!(fs::read(cached.path()).unwrap(), b"damaged");
    fs::remove_file(cached.path()).unwrap();
    symlink(&source, cached.path()).unwrap();
    assert!(cache_gif(&source, &cache).is_err());
    assert_eq!(fs::read(source).unwrap(), gif_bytes());
}

fn gif_without_lzw_end_code(width: u16, height: u16, interlaced: bool) -> Vec<u8> {
    let mut bytes = b"GIF89a".to_vec();
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.extend_from_slice(&[0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
    bytes.extend_from_slice(&[0x2c, 0, 0, 0, 0]);
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    bytes.push(if interlaced { 0x40 } else { 0 });
    // three-bit codes, least significant bit first: clear (4), one pixel (0), no end (5).
    bytes.extend_from_slice(&[2, 1, 0x04, 0, 0x3b]);
    bytes
}

#[test]
fn accepts_complete_image_data_without_lzw_end_code() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("complete.gif");
    let bytes = gif_without_lzw_end_code(1, 1, false);
    fs::write(&path, &bytes).unwrap();
    let cached = cache_gif(&path, &root.path().join("cache")).unwrap();
    assert_eq!(fs::read(cached.path()).unwrap(), bytes);
}

#[test]
fn rejects_incomplete_image_data_even_with_a_trailer() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("incomplete.gif");
    let cache = root.path().join("cache");
    for bytes in [
        gif_without_lzw_end_code(2, 1, false),
        gif_without_lzw_end_code(1, 2, true),
    ] {
        // each frame requires two pixels, but its compressed data supplies only one.
        fs::write(&path, bytes).unwrap();
        let error = cache_gif(&path, &cache).err().unwrap();
        assert!(format!("{error:#}").contains("image truncated"));
        assert!(!cache.exists());
    }
}

#[test]
fn rejects_truncated_later_frame_without_publishing_earlier_frames() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("incomplete-animation.gif");
    let cache = root.path().join("cache");
    let mut bytes = gif_without_lzw_end_code(2, 1, false);
    let first_frame = gif_without_lzw_end_code(1, 1, false);
    bytes.splice(
        19..19,
        first_frame[19..first_frame.len() - 1].iter().copied(),
    );
    fs::write(&path, bytes).unwrap();
    let error = cache_gif(&path, &cache).err().unwrap();
    assert!(format!("{error:#}").contains("image truncated"));
    assert!(!cache.exists());
}

#[test]
fn rejects_huge_logical_canvas_even_with_small_frames() {
    let root = TempDir::new().unwrap();
    let mut bytes = gif_bytes();
    bytes[6..10].fill(255);
    let path = root.path().join("canvas.gif");
    fs::write(&path, bytes).unwrap();
    let cache = root.path().join("cache");
    let error = cache_gif(&path, &cache).err().unwrap();
    assert!(error.to_string().contains("canvas exceeds"));
    assert!(!cache.exists());
}

fn uniform_gif(width: u16, height: u16, frames: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder =
            gif::Encoder::new(&mut bytes, width, height, &[0, 0, 0, 255, 255, 255]).unwrap();
        let mut frame = gif::Frame {
            width,
            height,
            buffer: vec![0; usize::from(width) * usize::from(height)].into(),
            ..Default::default()
        };
        frame.make_lzw_pre_encoded();
        for _ in 0..frames {
            encoder.write_lzw_pre_encoded_frame(&frame).unwrap();
        }
    }
    bytes
}

#[test]
fn bounds_frame_count_and_cumulative_decoding_work() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("many-frames.gif");
    let cache = root.path().join("cache");
    for bytes in [uniform_gif(1, 1, 1_001), uniform_gif(1024, 1024, 96)] {
        fs::write(&path, bytes).unwrap();
        let error = cache_gif(&path, &cache).err().unwrap();
        assert!(error.to_string().contains("decoding limit"));
        assert!(!cache.exists());
    }
}

fn copy_command(root: &Path, path: &Path, helper: Option<&str>) -> Output {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    if let Some(script) = helper {
        let executable = bin.join("wl-copy");
        fs::write(&executable, script).unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o700)).unwrap();
    }
    Command::new(env!("CARGO_BIN_EXE_gloob"))
        .args(["copy".as_ref(), path.as_os_str()])
        .env("PATH", bin)
        .env("XDG_CACHE_HOME", root.join("cache space ü #"))
        .env("WAYLAND_DISPLAY", "test-wayland")
        .env("XDG_RUNTIME_DIR", root)
        .env("CAPTURE", root)
        .output()
        .unwrap()
}

#[test]
fn hands_encoded_file_uri_to_helper_and_exits_successfully() {
    let root = TempDir::new().unwrap();
    let source = source(root.path());
    let output = copy_command(
        root.path(),
        &source,
        Some("#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$CAPTURE/args\"\n/bin/cat > \"$CAPTURE/uri\"\n"),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.path().join("args")).unwrap(),
        "--type\ntext/uri-list\n"
    );
    let payload = fs::read_to_string(root.path().join("uri")).unwrap();
    assert!(payload.ends_with("\r\n"));
    let path = Url::parse(payload.trim_end())
        .unwrap()
        .to_file_path()
        .unwrap();
    assert!(path.starts_with(root.path().join("cache space ü #/gloob/gifs")));
    fs::remove_file(source).unwrap();
    assert_eq!(fs::read(path).unwrap(), gif_bytes());
}

#[test]
fn reports_failed_or_missing_helper_without_claiming_success() {
    for helper in [None, Some("#!/bin/sh\n/bin/cat > /dev/null\nexit 7\n")] {
        let root = TempDir::new().unwrap();
        let source = source(root.path());
        let output = copy_command(root.path(), &source, helper);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("wl-copy"));
    }
}

#[test]
fn prunes_only_after_success_and_keeps_reselected_assets() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("source.gif");
    let mut assets = Vec::new();
    let helper = "#!/bin/sh\n/bin/cat > \"$CAPTURE/uri\"\n";
    for index in 0..11u8 {
        let mut bytes = gif_bytes();
        bytes.pop();
        bytes.extend_from_slice(&[0x21, 0xfe, 1, index, 0, 0x3b]);
        fs::write(&path, bytes).unwrap();
        let output = copy_command(root.path(), &path, Some(helper));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assets.push(
            Url::parse(fs::read_to_string(root.path().join("uri")).unwrap().trim())
                .unwrap()
                .to_file_path()
                .unwrap(),
        );
    }
    assert!(!assets[0].exists());
    assert!(assets[1..].iter().all(|asset| asset.exists()));
    let directory = assets[10].parent().unwrap();
    let before = fs::read(directory.join(".recency.json")).unwrap();
    let mut bytes = gif_bytes();
    bytes.pop();
    bytes.extend_from_slice(&[0x21, 0xfe, 1, 99, 0, 0x3b]);
    fs::write(&path, bytes).unwrap();
    assert!(
        !copy_command(root.path(), &path, Some("#!/bin/sh\nexit 7\n"))
            .status
            .success()
    );
    assert_eq!(fs::read(directory.join(".recency.json")).unwrap(), before);
    assert!(assets[1..].iter().all(|asset| asset.exists()));
    fs::copy(&assets[1], &path).unwrap();
    assert!(
        copy_command(root.path(), &path, Some(helper))
            .status
            .success()
    );
    assert!(assets[1].exists());
    // the unselected asset left by the failed copy is removed at the next successful copy.
    assert_eq!(
        fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "gif"))
            .count(),
        10
    );
}

#[test]
fn cleanup_failure_warns_but_copy_succeeds_and_preserves_the_file() {
    let root = TempDir::new().unwrap();
    let source = source(root.path());
    let directory = root.path().join("cache space ü #/gloob/gifs");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(".recency.json"), "invalid index").unwrap();
    let output = copy_command(
        root.path(),
        &source,
        Some("#!/bin/sh\n/bin/cat > \"$CAPTURE/uri\"\n"),
    );
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cleanup failed"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("copied file"));
    let path = Url::parse(fs::read_to_string(root.path().join("uri")).unwrap().trim())
        .unwrap()
        .to_file_path()
        .unwrap();
    assert_eq!(fs::read(path).unwrap(), gif_bytes());
}

#[test]
fn concurrent_copies_serialize_publication_and_pruning() {
    let root = TempDir::new().unwrap();
    let source = source(root.path());
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let helper = bin.join("wl-copy");
    fs::write(&helper, "#!/bin/sh\n/bin/cat > \"$CAPTURE/uri-$NAME\"\nprintf started > \"$CAPTURE/started-$NAME\"\nif [ \"$NAME\" = first ]; then\n while [ ! -f \"$CAPTURE/release\" ]; do /bin/sleep 0.02; done\nfi\n").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
    let spawn = |name| {
        Command::new(env!("CARGO_BIN_EXE_gloob"))
            .args(["copy".as_ref(), source.as_os_str()])
            .env("PATH", &bin)
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("WAYLAND_DISPLAY", "test-wayland")
            .env("XDG_RUNTIME_DIR", root.path())
            .env("CAPTURE", root.path())
            .env("NAME", name)
            .spawn()
            .unwrap()
    };
    let mut first = spawn("first");
    let started = std::time::Instant::now();
    while !root.path().join("started-first").exists() && started.elapsed().as_secs() < 5 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(root.path().join("started-first").exists());
    let mut second = spawn("second");
    std::thread::sleep(std::time::Duration::from_millis(150));
    let serialized =
        !root.path().join("started-second").exists() && second.try_wait().unwrap().is_none();
    fs::write(root.path().join("release"), "release").unwrap();
    assert!(first.wait().unwrap().success());
    assert!(second.wait().unwrap().success());
    assert!(
        serialized,
        "second publication started while first still held the clipboard/cache transaction"
    );
    assert!(root.path().join("started-second").exists());
}

#[test]
fn requires_wayland_session_for_publication() {
    let root = TempDir::new().unwrap();
    let source = source(root.path());
    let output = Command::new(env!("CARGO_BIN_EXE_gloob"))
        .args(["copy".as_ref(), source.as_os_str()])
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("wayland session"));
}

#[test]
fn rejects_extra_arguments_and_shows_help_without_a_session() {
    let output = Command::new(env!("CARGO_BIN_EXE_gloob"))
        .args(["copy", "file.gif", "extra"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let help = Command::new(env!("CARGO_BIN_EXE_gloob"))
        .arg("--help")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("gloob copy <path>"));
}
