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

## configuration

picker keybindings use `$XDG_CONFIG_HOME/gloob/config.toml`, or `~/.config/gloob/config.toml` when xdg_config_home is unset, empty, or relative. missing default files use built-in defaults. an explicitly requested config must exist; symlinks are supported. the picker startup will accept `--config <path>`.

see [`examples/config.toml`](examples/config.toml) for defaults. `[keybindings]` supports `move_left`, `move_down`, `move_up`, `move_right`, `select`, and `close`, each with a nonempty array of chords. override only what you need. default movement is ctrl+h/j/k/l; enter selects and escape closes. modifiers are ctrl, alt, and shift; named keys include arrows, enter, escape, tab, space, and plus. printable shortcuts require ctrl or alt so ordinary search typing remains available. unmodified navigation is limited to arrows. duplicate bindings and unknown fields are errors rather than silently ignored typos.

these are in-app bindings, not compositor hotkeys. there is no gui configuration editor.

## provider

gifjif search and media retrieval live in `src/provider.rs`. search uses `sort=top`, requests 24 results, and passes pagination cursors through unchanged. an empty query uses the feed. the provider is public and requires no account or api key; search text is sent directly to gifjif. see [gifjif's developer documentation](https://gifjif.com/developers).

requests have connection and total timeouts, no automatic redirects or retries, and explicit byte limits: 2 mib for responses/previews and 25 mib for original gifs. media urls must use https on gifjif's domain. downloaded originals still go through the full gif validator before caching or copying. rate-limit errors preserve a numeric retry-after value when supplied.

## clipboard and cache

validated gifs go in `$XDG_CACHE_HOME/gloob/gifs`, falling back to `~/.cache/gloob/gifs` when the variable is unset, empty, or relative. filenames use a sha256 content hash so moving or renaming a source does not create another cached copy.

gloob hands a percent-encoded `file://` uri to `wl-copy` as `text/uri-list`. wl-copy forks a background clipboard owner, so gloob can exit without dropping the selection. the helper serves subsequent paste requests until the selection is replaced or the session ends. gloob itself does not stay running.

the file must remain available because the clipboard refers to its path. do not clear the cache before pasting. after a successful copy, gloob retains the ten most recently selected gifs, using a separate atomic recency index so reselecting does not rewrite gif bytes. failed clipboard publication neither advances recency nor removes previous selections. cleanup keeps the newly selected file and ignores unrelated filenames and symlinks. concurrent gloob copies serialize publication and cleanup with a cache lock; cleanup failures warn without claiming the successful copy failed. deleting the original source does not remove the cached copy.

this is a ten-file cache, not clipboard history. older file references can stop working after later selections evict them; the current selection remains available. previews never enter this cache.

files are limited to 25 mib. validation decodes frames with bounded memory and total decoding work. complete frames are accepted without an lzw end code because some real-world gifs omit it. truncated pixel data, malformed frames, and files without a gif trailer are still rejected before publication. cache files are written atomically; an existing damaged asset produces an error rather than silently replacing a file another paste might be reading.

## checks

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

tests use a fake clipboard helper and do not change your clipboard. real sway/client testing is separate: verify the mime offer and cached uri with `wl-paste --list-types` and `wl-paste --type text/uri-list`, then paste into the intended client after gloob exits.
