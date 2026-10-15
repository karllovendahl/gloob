# gloob

a small, single-shot gif picker for wayland. search gifsnap, move through animated gif and webp previews, copy the selected file, and exit. no account, provider key, background gloob service, or automatic paste.

## requirements

- gtk4 4.10 or newer and gtk4-layer-shell runtime libraries
- wl-clipboard (`wl-copy`)
- an active wayland session with layer-shell and clipboard support; initial target is sway
- internet access for search and gif downloads

building from source also requires rust/cargo with edition 2024 support, a c compiler, pkg-config, and gtk4/layer-shell development files. the build checks native libraries but never installs system packages.

fedora source-build dependencies:

```sh
sudo dnf install rust cargo gcc pkgconf-pkg-config gtk4-devel gtk4-layer-shell-devel wl-clipboard
```

arch source-build dependencies:

```sh
sudo pacman -S rust base-devel gtk4 gtk4-layer-shell wl-clipboard
```

rustfmt and clippy are contributor checks, not dependencies for using gloob.

## build and use

```sh
cargo build --locked
./target/debug/gloob
./target/debug/gloob --config /path/to/config.toml
./target/debug/gloob copy /path/to/reaction.gif
./target/debug/gloob copy /path/to/reaction.webp
```

type into the focused search field. ctrl+h/j/k/l moves left/down/up/right; enter downloads and copies the selected full asset in its original gif or webp format, then exits. escape closes without copying. mouse clicks also select and copy. ctrl+n loads the next bounded page when available; it is ignored while loading or copying. titles stay in tooltips and accessibility labels so the grid stays image-only. tab navigates the remaining controls. copy failures stay visible so you can retry with enter. once copying starts, close/escape waits for its result to avoid claiming a cancelled copy while the clipboard operation completes.

gif and webp previews load near the visible rows with four bounded workers, and only visible tiles animate. previews are limited to 25 mib, one million canvas pixels, 500 frames and 40 million canvas pixels of decoding work; only small sampled rgba frames remain in memory. each preview retains at most 48 downsampled frames, preserving the total playback duration; selected originals are never re-encoded or resized. search is debounced, old search/preview results are discarded, and a separate copy worker prevents preview downloads from holding up selection. empty search uses the feed.

the clipboard contains a file reference, not a web link or flattened image. paste manually into a file-aware client using ctrl+v. whether that becomes an animated attachment depends on the client and format; gif support does not imply animated webp support. real animated gif paste was confirmed on the initial sway setup, but the client was not recorded. animated webp paste compatibility needs separate checking in the intended app. gloob never disguises a webp as a gif or converts it.

launch hotkeys belong to the compositor. add a sway binding yourself, using your actual binary path:

```text
bindsym $mod+g exec /absolute/path/to/gloob
```

gloob does not edit compositor configs. a keybinding launches a fresh picker; no resident gloob process is needed.

## configuration

picker keybindings use `$XDG_CONFIG_HOME/gloob/config.toml`, or `~/.config/gloob/config.toml` when xdg_config_home is unset, empty, or relative. missing default files use built-in defaults. an explicitly requested config must exist; symlinks are supported. use `--config <path>` to choose another file.

see [`examples/config.toml`](examples/config.toml) for defaults. `[keybindings]` supports `move_left`, `move_down`, `move_up`, `move_right`, `select`, `close`, and `next_page`, each with a nonempty array of chords. override only what you need. default movement is ctrl+h/j/k/l; enter selects, escape closes, and ctrl+n loads the next page. modifiers are ctrl, alt, and shift; named keys include arrows, enter, escape, tab, space, and plus. printable shortcuts require ctrl or alt so ordinary search typing remains available. unmodified navigation is limited to arrows. duplicate bindings and unknown fields are errors rather than silently ignored typos. if an existing config uses ctrl+n for another action, explicitly remap `next_page` too, for example `next_page = ["alt+n"]`; defaults do not silently take over an existing shortcut.

these are in-app bindings, not compositor hotkeys. there is no gui configuration editor.

appearance defaults to a charcoal panel at 95% background opacity:

```toml
[appearance]
theme = "dark"
background_opacity = 0.95
```

`theme` supports `dark`, `light`, and `system`. dark/light use gloob's fixed palettes regardless of the desktop setting; system uses the gtk theme's colors. `background_opacity` is a finite number from 0.0 to 1.0; 1.0 is solid. only the panel background is transparent, not gif textures, text, or the selection ring. opacity does not enable blur, which depends on compositor support. arbitrary css themes are not supported.

## provider

gifsnap search and media retrieval live in `src/provider.rs`. the client uses the rest api directly, requests 24 results, and follows `pagination.next_page`; an empty query uses trending. the public api currently requires no account or key. search text and download requests are sent directly to gifsnap. see [gifsnap's rest documentation](https://gifsnap.com/docs#rest).

supported `animated_preview` images are used when supplied, with a 512 kib download cap. missing, unsupported, failed or undecodable small previews fall back to the full selected asset within the preview decode limits. `preview_url` can be static and is not treated as an animation. full-asset fallback increases browsing bandwidth when small animations are unavailable; previews never enter the persistent selection cache. gif and webp results remain available without conversion. selected downloads preserve the returned full url, bytes and format, not an assumed upstream resolution.

requests have 5-second connection and 15-second total timeouts, no automatic redirects or retries, and explicit byte limits: 2 mib for api responses and 25 mib for full media. media urls must use https on gifsnap's domain; unexpected external hosts, credentials and custom ports are rejected. downloaded files go through full gif or webp validation before caching or copying. rate-limit errors preserve a numeric retry-after value when supplied. the picker caps its own search attempts at 40 per minute per process. that is a gloob safety policy, not a published gifsnap quota: the service provides no numeric rate-limit or unlimited-capacity promise. retry is explicit rather than an automatic loop.

## clipboard and cache

validated gif and webp files go in `$XDG_CACHE_HOME/gloob/gifs`, falling back to `~/.cache/gloob/gifs` when the variable is unset, empty, or relative. filenames use a sha256 content hash so moving or renaming a source does not create another cached copy.

gloob hands a percent-encoded `file://` uri to `wl-copy` as `text/uri-list`. wl-copy forks a background clipboard owner, so gloob can exit without dropping the selection. the helper serves subsequent paste requests until the selection is replaced or the session ends. gloob itself does not stay running.

the file must remain available because the clipboard refers to its path. do not clear the cache before pasting. after a successful copy, gloob retains the ten most recently selected files across both formats, using a separate atomic recency index so reselecting does not rewrite gif bytes. failed clipboard publication neither advances recency nor removes previous selections. cleanup keeps the newly selected file and ignores unrelated filenames and symlinks. concurrent gloob copies serialize publication and cleanup with a cache lock; cleanup failures warn without claiming the successful copy failed. deleting the original source does not remove the cached copy.

this is a ten-file cache, not clipboard history. older file references can stop working after later selections evict them; the current selection remains available. previews never enter this cache.

files are limited to 25 mib. gif and webp formats are detected from content, not filenames, and cached with the matching extension. original bytes are never converted. webp validation checks container boundaries, canvas and embedded frame dimensions before decoding every frame; this also bounds codec allocations that are not covered by its own memory limit. existing gif cache entries remain valid. validation decodes frames with bounded memory and total decoding work. complete frames are accepted without an lzw end code because some real-world gifs omit it. truncated pixel data, malformed frames, and files without a gif trailer are still rejected before publication. cache files are written atomically; an existing damaged asset produces an error rather than silently replacing a file another paste might be reading.

## checks

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

tests use a fake clipboard helper and do not change your clipboard. real sway/client testing is separate: launch the picker, type a query, verify animated previews and ctrl navigation, dismiss with escape, then launch again and select with enter. verify the mime offer and cached uri with `wl-paste --list-types` and `wl-paste --type text/uri-list`, then paste into the intended client after gloob exits.
