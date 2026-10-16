# gloob

a small gif and animated webp picker for wayland. search, copy, paste. powered by [gifsnap](https://gifsnap.com), no api key needed.

## setup

requires a wayland compositor with layer-shell support, such as sway, and gtk4 4.10 or newer.

install build dependencies:

```sh
# fedora
sudo dnf install rust cargo gcc pkgconf-pkg-config gtk4-devel gtk4-layer-shell-devel wl-clipboard

# arch
sudo pacman -S rust base-devel gtk4 gtk4-layer-shell wl-clipboard
```

from the repo directory:

```sh
cargo build --release --locked
install -Dm755 target/release/gloob ~/.local/bin/gloob
~/.local/bin/gloob
```

optional sway shortcut:

```text
bindsym $mod+g exec ~/.local/bin/gloob
```

## use

- type to search; scroll for more results.
- ctrl+h/j/k/l moves left/down/up/right.
- enter or click copies the selected file and closes gloob.
- escape closes without copying.
- ctrl+n loads more results manually.

paste with ctrl+v in your app. gloob copies the original gif or webp file without conversion; animated paste support depends on the app. the last ten selections stay cached so they remain available after gloob closes.

## configuration

optional: create `~/.config/gloob/config.toml`, or `$XDG_CONFIG_HOME/gloob/config.toml` if set. for example:

```toml
[appearance]
theme = "light" # dark, light, or system
background_opacity = 0.95
```

see [examples/config.toml](examples/config.toml) for keybindings and defaults. use `gloob --config /path/to/config.toml` to load a different file.
