use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};
use url::Url;

use crate::media::CachedGif;

pub fn publish(asset: &CachedGif) -> Result<()> {
    let display = std::env::var_os("WAYLAND_DISPLAY");
    let runtime = std::env::var_os("XDG_RUNTIME_DIR");
    ensure!(
        display.is_some_and(|value| !value.is_empty()),
        "a wayland session is required (wayland_display is unset)"
    );
    ensure!(
        runtime.is_some_and(|value| !value.is_empty()),
        "a wayland session is required (xdg_runtime_dir is unset)"
    );

    let uri = Url::from_file_path(asset.path())
        .map_err(|()| anyhow::anyhow!("could not encode the cached gif as a file uri"))?;
    let mut child = Command::new("wl-copy")
        .args(["--type", "text/uri-list"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .context("could not start wl-copy; install wl-clipboard and check your path")?;

    let result = {
        let mut stdin = child.stdin.take().context("could not open wl-copy input")?;
        write!(stdin, "{uri}\r\n")
    };

    // the launcher exits after handing off to its background clipboard owner.
    let status = child.wait().context("could not wait for wl-copy")?;
    ensure!(status.success(), "wl-copy failed ({status})");
    result.context("could not send the gif uri to wl-copy")?;
    Ok(())
}
