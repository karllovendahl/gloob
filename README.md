# gloob

a small gif picker for wayland. currently just the clipboard slice: copy a local gif and exit.

## requirements

- rust and cargo with edition 2024 support
- wl-clipboard (`wl-copy`)
- an active wayland session with clipboard support; initial target is sway

no gtk dependencies yet. the picker ui comes later.

## build and use

```sh
cargo build --locked
./target/debug/gloob copy /path/to/reaction.gif
```

the command copies a file reference, not a url or a still image. paste manually into a file-aware client using ctrl+v. whether that becomes an animated attachment depends on the client; slack and discord compatibility is not yet verified.

## clipboard and cache

validated gifs go in `$XDG_CACHE_HOME/gloob/gifs`, falling back to `~/.cache/gloob/gifs` when the variable is unset, empty, or relative. filenames use a sha256 content hash so moving or renaming a source does not create another cached copy.

gloob hands a percent-encoded `file://` uri to `wl-copy` as `text/uri-list`. wl-copy forks a background clipboard owner, so gloob can exit without dropping the selection. the helper serves subsequent paste requests until the selection is replaced or the session ends. gloob itself does not stay running.

the file must remain available because the clipboard refers to its path. do not clear the cache before pasting. there is no automatic eviction yet, and deleting the original source does not remove the cached copy.

files are limited to 25 mib. validation decodes frames with bounded memory and total decoding work. incomplete or invalid assets are rejected before publication. cache files are written atomically; an existing damaged asset produces an error rather than silently replacing a file another paste might be reading.

## checks

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

tests use a fake clipboard helper and do not change your clipboard. real sway/client testing is separate: verify the mime offer and cached uri with `wl-paste --list-types` and `wl-paste --type text/uri-list`, then paste into the intended client after gloob exits.
