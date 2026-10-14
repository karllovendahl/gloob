use std::ffi::OsString;
use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

const MAX_FILE_BYTES: u64 = 25 * 1024 * 1024;
const MAX_FRAME_PIXELS: u64 = 16_000_000;
const MAX_TOTAL_PIXELS: usize = 100_000_000;
const MAX_FRAMES: usize = 1_000;

pub struct CachedGif {
    path: PathBuf,
}

impl CachedGif {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub fn cache_dir() -> Result<PathBuf> {
    resolve_cache_dir(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
}

fn resolve_cache_dir(xdg: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    if let Some(base) = xdg.filter(|value| Path::new(value).is_absolute()) {
        return Ok(PathBuf::from(base).join("gloob/gifs"));
    }
    let home = home.context("set home or an absolute xdg_cache_home to locate the cache")?;
    ensure!(
        Path::new(&home).is_absolute(),
        "home must be an absolute path"
    );
    Ok(PathBuf::from(home).join(".cache/gloob/gifs"))
}

pub fn cache_gif(source: &Path, directory: &Path) -> Result<CachedGif> {
    let bytes = read_bounded(source)?;
    cache_bytes(&bytes, directory)
}

pub fn cache_bytes(bytes: &[u8], directory: &Path) -> Result<CachedGif> {
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "gif exceeds the 25 mib size limit"
    );
    validate_gif(bytes)?;

    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .context("could not create the gif cache")?;
    let directory = directory
        .canonicalize()
        .context("could not resolve the gif cache")?;
    let path = directory.join(format!("{:x}.gif", Sha256::digest(bytes)));
    let mut temporary =
        tempfile::NamedTempFile::new_in(&directory).context("could not create a cached gif")?;
    temporary
        .write_all(bytes)
        .context("could not write the cached gif")?;
    temporary
        .flush()
        .context("could not flush the cached gif")?;

    // publish only complete files; never replace an asset another paste may be reading.
    match temporary.persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata =
                fs::symlink_metadata(&path).context("could not inspect the cached gif")?;
            ensure!(
                metadata.file_type().is_file(),
                "cached gif is not a regular file"
            );
            ensure!(
                read_bounded(&path)? == bytes,
                "cached gif is damaged; remove it and retry"
            );
        }
        Err(error) => return Err(error.error).context("could not publish the cached gif"),
    }

    Ok(CachedGif { path })
}

pub(crate) fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let metadata =
        fs::metadata(path).with_context(|| format!("could not inspect {}", path.display()))?;
    ensure!(metadata.is_file(), "gif must be a regular file");
    let file = File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let metadata = file.metadata().context("could not inspect the gif")?;
    ensure!(metadata.is_file(), "gif must be a regular file");
    ensure!(
        metadata.len() <= MAX_FILE_BYTES,
        "gif exceeds the 25 mib size limit"
    );
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("could not read the gif")?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "gif exceeds the 25 mib size limit"
    );
    Ok(bytes)
}

pub(crate) fn validate_gif(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "gif exceeds the 25 mib size limit"
    );
    ensure!(
        bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "file is not a gif"
    );
    ensure!(bytes.last() == Some(&0x3b), "gif is missing its trailer");
    let mut options = gif::DecodeOptions::new();
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        MAX_FRAME_PIXELS.try_into().unwrap(),
    ));
    options.check_frame_consistency(true);
    // tolerate missing lzw end codes; frame decoding still requires every declared pixel.
    options.check_lzw_end_code(false);
    let mut decoder = options.read_info(bytes).context("invalid gif header")?;
    ensure!(
        decoder.width() > 0 && decoder.height() > 0,
        "gif dimensions must be nonzero"
    );
    let canvas_pixels = u64::from(decoder.width()) * u64::from(decoder.height());
    ensure!(
        canvas_pixels <= MAX_FRAME_PIXELS,
        "gif canvas exceeds the pixel limit"
    );
    let mut frames = 0;
    let mut pixels = 0;
    while let Some(frame) = decoder.read_next_frame().context("invalid gif frame")? {
        frames += 1;
        pixels += frame.buffer.len();
        if frames > MAX_FRAMES || pixels > MAX_TOTAL_PIXELS {
            bail!("gif exceeds the decoding limit");
        }
        ensure!(
            frame.width > 0 && frame.height > 0,
            "gif frame dimensions must be nonzero"
        );
    }
    ensure!(frames > 0, "gif contains no frames");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_follow_xdg_rules() {
        let home = Some(OsString::from("/home/test"));
        assert_eq!(
            resolve_cache_dir(Some("/custom/cache".into()), home.clone()).unwrap(),
            Path::new("/custom/cache/gloob/gifs")
        );
        for xdg in [None, Some("".into()), Some("relative/cache".into())] {
            assert_eq!(
                resolve_cache_dir(xdg, home.clone()).unwrap(),
                Path::new("/home/test/.cache/gloob/gifs")
            );
        }
        assert!(resolve_cache_dir(None, None).is_err());
        assert!(resolve_cache_dir(None, Some("relative/home".into())).is_err());
    }
}
