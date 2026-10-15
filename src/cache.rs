use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use fs2::FileExt;

use crate::{clipboard, media};

const RETAIN: usize = 10;
const INDEX: &str = ".recency.json";

/// a cleanup warning does not turn a successful clipboard publication into failure.
pub fn copy_path(path: &Path) -> Result<Option<String>> {
    copy_bytes(&media::read_bounded(path)?)
}

pub fn copy_bytes(bytes: &[u8]) -> Result<Option<String>> {
    media::validate(bytes)?;
    let directory = media::cache_dir()?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .context("could not create the gif cache")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(".lock"))
        .context("could not open the cache lock")?;
    lock.lock_exclusive()
        .context("could not lock the gif cache")?;
    let asset = media::cache_bytes(bytes, &directory)?;
    clipboard::publish(&asset)?;
    let name = asset.path().file_name().unwrap().to_str().unwrap();
    let warning = record_selection(&directory, name)
        .err()
        .map(|error| format!("copied gif, but cache cleanup failed: {error:#}"));
    // the lock lasts through publication and pruning so another gloob cannot evict the new selection.
    Ok(warning)
}

fn owned_name(name: &str) -> bool {
    let Some((hash, extension)) = name.rsplit_once('.') else {
        return false;
    };
    hash.len() == 64
        && matches!(extension, "gif" | "webp")
        && hash
            .as_bytes()
            .iter()
            .copied()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn regular_asset(directory: &Path, name: &str) -> bool {
    owned_name(name)
        && fs::symlink_metadata(directory.join(name))
            .is_ok_and(|metadata| metadata.file_type().is_file())
}

fn record_selection(directory: &Path, selected: &str) -> Result<()> {
    ensure!(
        regular_asset(directory, selected),
        "selected cache asset is not a regular file"
    );
    let index = directory.join(INDEX);
    let mut recent: Vec<String> = match fs::symlink_metadata(&index) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "cache index is not a regular file"
            );
            ensure!(
                metadata.len() <= 128 * 1024,
                "cache index exceeds its size limit"
            );
            let mut bytes = Vec::new();
            fs::File::open(&index)?
                .take(128 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 128 * 1024,
                "cache index exceeds its size limit"
            );
            let names: Vec<String> = serde_json::from_slice(&bytes)
                .context("invalid cache recency index; no files were pruned")?;
            ensure!(
                names.iter().all(|name| owned_name(name)),
                "invalid cache index names; no files were pruned"
            );
            names
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // adopt content-addressed assets from earlier versions; oldest modified files go first.
            let mut existing = Vec::new();
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if regular_asset(directory, &name) {
                    existing.push((entry.metadata()?.modified()?, name));
                }
            }
            existing.sort();
            existing.into_iter().map(|(_, name)| name).collect()
        }
        Err(error) => return Err(error).context("could not inspect the cache index"),
    };
    recent.retain(|name| name != selected && regular_asset(directory, name));
    recent.dedup();
    recent.push(selected.to_owned());
    let expired = recent.len().saturating_sub(RETAIN);
    // publish the entire order before pruning. interrupted cleanup is retried on the next selection.
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec(&recent)?)?;
    temporary.flush()?;
    temporary
        .persist(&index)
        .context("could not publish cache recency")?;
    for entry in fs::read_dir(directory)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name != selected && regular_asset(directory, &name) && !recent[expired..].contains(&name)
        {
            fs::remove_file(directory.join(name)).context("could not remove an old cached gif")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(index: usize) -> String {
        format!("{index:064x}.gif")
    }

    fn select(directory: &Path, index: usize) {
        fs::write(directory.join(name(index)), b"unchanged").unwrap();
        record_selection(directory, &name(index)).unwrap();
    }

    #[test]
    fn retains_ten_selections_and_refreshes_reused_assets() {
        let directory = tempfile::tempdir().unwrap();
        for index in 0..10 {
            select(directory.path(), index);
        }
        let path = directory.path().join(name(0));
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        record_selection(directory.path(), &name(0)).unwrap();
        select(directory.path(), 10);
        assert!(path.exists());
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
        assert!(!directory.path().join(name(1)).exists());
        assert!(directory.path().join(name(10)).exists());
        assert_eq!(
            fs::read_dir(directory.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| regular_asset(
                    directory.path(),
                    &entry.file_name().to_string_lossy()
                ))
                .count(),
            10
        );
        for index in 11..24 {
            select(directory.path(), index);
        }
        assert!(directory.path().join(name(23)).exists());
        assert!(!path.exists());
    }

    #[test]
    fn ignores_unrelated_entries_and_symlinks_and_refuses_bad_index() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("important.gif"), "keep").unwrap();
        let target = directory.path().join("important.gif");
        std::os::unix::fs::symlink(&target, directory.path().join(name(99))).unwrap();
        for index in 0..12 {
            select(directory.path(), index);
        }
        assert!(target.exists());
        assert!(
            fs::symlink_metadata(directory.path().join(name(99)))
                .unwrap()
                .is_symlink()
        );
        fs::write(directory.path().join(INDEX), "bad index").unwrap();
        fs::write(directory.path().join(name(15)), "keep").unwrap();
        assert!(record_selection(directory.path(), &name(15)).is_err());
        assert!(directory.path().join(name(2)).exists());
        assert!(directory.path().join(name(15)).exists());
    }
}
