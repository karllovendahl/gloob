//! semantic, application-local shortcuts, independent of gtk or global hotkeys.
//!
//! printable navigation requires ctrl or alt so search keeps ordinary typing.
//! the same rule protects printable select/close bindings; shift alone is not
//! enough. navigation without ctrl/alt is limited to arrows.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    MoveLeft,
    MoveDown,
    MoveUp,
    MoveRight,
    Select,
    Close,
    NextPage,
}

impl Action {
    pub const ALL: [Self; 7] = [
        Self::MoveLeft,
        Self::MoveDown,
        Self::MoveUp,
        Self::MoveRight,
        Self::Select,
        Self::Close,
        Self::NextPage,
    ];

    fn is_navigation(self) -> bool {
        matches!(
            self,
            Self::MoveLeft | Self::MoveDown | Self::MoveUp | Self::MoveRight
        )
    }
}

/// characters are lowercase ascii; named keys use the variants below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Character(char),
    Left,
    Down,
    Up,
    Right,
    Enter,
    Escape,
    Tab,
    Space,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// input adapters should lowercase character keys and ignore lock modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub modifiers: Modifiers,
}

impl FromStr for KeyChord {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        let mut parts = normalized.split('+');
        let mut token = parts.next().unwrap_or_default().trim();
        let mut modifiers = Modifiers::default();
        for next in parts {
            let flag = match token {
                "ctrl" => &mut modifiers.ctrl,
                "alt" => &mut modifiers.alt,
                "shift" => &mut modifiers.shift,
                _ => bail!("invalid modifier in chord {value:?}"),
            };
            ensure!(!*flag, "repeated modifier in chord {value:?}");
            *flag = true;
            token = next.trim();
        }
        let key = match token {
            "left" => Key::Left,
            "down" => Key::Down,
            "up" => Key::Up,
            "right" => Key::Right,
            "enter" => Key::Enter,
            "escape" => Key::Escape,
            "tab" => Key::Tab,
            "space" => Key::Space,
            "plus" => Key::Character('+'),
            _ => {
                let mut chars = token.chars();
                let character = chars.next().context("a chord must contain a key")?;
                ensure!(
                    chars.next().is_none() && character.is_ascii_graphic(),
                    "invalid key in chord {value:?}"
                );
                Key::Character(character)
            }
        };
        Ok(Self { key, modifiers })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    System,
    Light,
    #[default]
    Dark,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub theme: Theme,
    pub background_opacity: f64,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: Theme::Dark,
            background_opacity: 0.95,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    keybindings: BTreeMap<Action, Vec<KeyChord>>,
    pub appearance: Appearance,
}

impl Default for Config {
    fn default() -> Self {
        let keys = [
            "ctrl+h", "ctrl+j", "ctrl+k", "ctrl+l", "enter", "escape", "ctrl+n",
        ];
        Self {
            appearance: Appearance::default(),
            keybindings: Action::ALL
                .into_iter()
                .zip(keys)
                .map(|(action, key)| (action, vec![key.parse().expect("valid default chord")]))
                .collect(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    keybindings: FileBindings,
    appearance: Appearance,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileBindings {
    move_left: Option<Vec<String>>,
    move_down: Option<Vec<String>>,
    move_up: Option<Vec<String>>,
    move_right: Option<Vec<String>>,
    select: Option<Vec<String>>,
    close: Option<Vec<String>>,
    next_page: Option<Vec<String>>,
}

impl Config {
    /// missing default files use defaults; explicitly requested files must exist.
    /// reading follows symlinks, but other read and parse errors are never ignored.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let default_path;
        let selected = match path {
            Some(path) => path,
            None => {
                default_path = Self::default_path()?;
                &default_path
            }
        };
        Self::load_path(selected, path.is_none())
    }

    pub fn default_path() -> Result<PathBuf> {
        config_path(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
        )
    }

    fn load_path(path: &Path, missing_is_default: bool) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Self::from_toml(&contents)
                .with_context(|| format!("invalid config {}", path.display())),
            Err(error) if missing_is_default && error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::default())
            }
            Err(error) => {
                Err(error).with_context(|| format!("could not read config {}", path.display()))
            }
        }
    }

    pub fn from_toml(contents: &str) -> Result<Self> {
        let raw: FileConfig = toml::from_str(contents).context("could not parse config toml")?;
        let overrides = [
            raw.keybindings.move_left,
            raw.keybindings.move_down,
            raw.keybindings.move_up,
            raw.keybindings.move_right,
            raw.keybindings.select,
            raw.keybindings.close,
            raw.keybindings.next_page,
        ];
        ensure!(
            raw.appearance.background_opacity.is_finite()
                && (0.0..=1.0).contains(&raw.appearance.background_opacity),
            "background_opacity must be a finite number between 0 and 1"
        );
        let mut config = Self {
            appearance: raw.appearance,
            ..Self::default()
        };
        for (action, keys) in Action::ALL.into_iter().zip(overrides) {
            if let Some(keys) = keys {
                ensure!(
                    !keys.is_empty(),
                    "{action:?} must have at least one binding"
                );
                let chords = keys
                    .into_iter()
                    .map(|key| key.parse())
                    .collect::<Result<Vec<_>>>()?;
                config.keybindings.insert(action, chords);
            }
        }
        let mut assigned = BTreeMap::new();
        for (&action, chords) in &config.keybindings {
            for &chord in chords {
                let command_modifier = chord.modifiers.ctrl || chord.modifiers.alt;
                let printable = matches!(chord.key, Key::Character(_) | Key::Space);
                ensure!(
                    !printable || command_modifier,
                    "{action:?}: printable shortcuts require ctrl or alt to preserve search typing"
                );
                ensure!(
                    !action.is_navigation()
                        || command_modifier
                        || matches!(chord.key, Key::Left | Key::Down | Key::Up | Key::Right),
                    "{action:?}: navigation requires ctrl/alt or an arrow key"
                );
                if let Some(previous) = assigned.insert(chord, action) {
                    bail!("duplicate binding {chord:?} for {previous:?} and {action:?}");
                }
            }
        }
        Ok(config)
    }

    pub fn bindings(&self, action: Action) -> &[KeyChord] {
        &self.keybindings[&action]
    }

    pub fn action_for(&self, chord: KeyChord) -> Option<Action> {
        self.keybindings
            .iter()
            .find_map(|(&action, keys)| keys.contains(&chord).then_some(action))
    }
}

fn config_path(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Result<PathBuf> {
    if let Some(path) = xdg.map(Path::new).filter(|path| path.is_absolute()) {
        return Ok(path.join("gloob/config.toml"));
    }
    let home = home
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .context(
            "an absolute home is required when xdg_config_home is unset, empty, or relative",
        )?;
    Ok(home.join(".config/gloob/config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_empty_config_agree() {
        let config = Config::from_toml("").unwrap();
        assert_eq!(config, Config::default());
        for (action, key) in Action::ALL.into_iter().zip([
            "ctrl+h", "ctrl+j", "ctrl+k", "ctrl+l", "enter", "escape", "ctrl+n",
        ]) {
            assert_eq!(config.action_for(key.parse().unwrap()), Some(action));
        }
        assert_eq!(config.action_for("h".parse().unwrap()), None);
    }

    #[test]
    fn partial_overrides_replace_one_action_and_allow_multiple_keys() {
        let config = Config::from_toml("[keybindings]\nmove_left = ['left', 'alt+h']").unwrap();
        assert_eq!(config.bindings(Action::MoveLeft).len(), 2);
        assert_eq!(
            config.action_for("left".parse().unwrap()),
            Some(Action::MoveLeft)
        );
        assert_eq!(config.action_for("ctrl+h".parse().unwrap()), None);
        assert_eq!(
            config.bindings(Action::Close),
            Config::default().bindings(Action::Close)
        );
    }

    #[test]
    fn appearance_defaults_and_partial_overrides_are_validated() {
        let config = Config::from_toml("").unwrap();
        assert_eq!(config.appearance.theme, Theme::Dark);
        assert_eq!(config.appearance.background_opacity, 0.95);
        for (theme, expected) in [
            ("system", Theme::System),
            ("light", Theme::Light),
            ("dark", Theme::Dark),
        ] {
            let config = Config::from_toml(&format!("[appearance]\ntheme='{theme}'")).unwrap();
            assert_eq!(config.appearance.theme, expected);
            assert_eq!(config.appearance.background_opacity, 0.95);
            assert_eq!(
                config.bindings(Action::NextPage),
                Config::default().bindings(Action::NextPage)
            );
        }
        let config = Config::from_toml("[appearance]\nbackground_opacity=1.0").unwrap();
        assert_eq!(config.appearance.theme, Theme::Dark);
        assert_eq!(config.appearance.background_opacity, 1.0);
        for text in [
            "theme='custom'",
            "background_opacity=-0.1",
            "background_opacity=1.1",
            "background_opacity=nan",
            "background_opacity=inf",
            "background_opacity='0.95'",
            "unknown=true",
        ] {
            assert!(
                Config::from_toml(&format!("[appearance]\n{text}")).is_err(),
                "{text}"
            );
        }
    }

    #[test]
    fn next_page_can_be_overridden_without_stealing_search_input() {
        let config = Config::from_toml("[keybindings]\nnext_page=['alt+n']").unwrap();
        assert_eq!(
            config.action_for("alt+n".parse().unwrap()),
            Some(Action::NextPage)
        );
        assert_eq!(config.action_for("ctrl+n".parse().unwrap()), None);
        assert!(Config::from_toml("[keybindings]\nnext_page=['ctrl+j']").is_err());
        assert!(Config::from_toml("[keybindings]\nnext_page=['n']").is_err());
    }

    #[test]
    fn rejects_unknown_fields_and_actions() {
        for text in [
            "unknown = true",
            "[keybindings]\nunknown = ['ctrl+x']",
            "[unknown]",
            "keybindings = 'bad'",
        ] {
            assert!(Config::from_toml(text).is_err(), "{text}");
        }
    }

    #[test]
    fn rejects_empty_invalid_ambiguous_and_search_stealing_bindings() {
        for binding in [
            "[]",
            "['']",
            "['ctrl+']",
            "['super+h']",
            "['ctrl+ctrl+h']",
            "['ctrl+unknown']",
            "['ctrl+h', 'CTRL+H']",
            "['enter']",
            "['h']",
            "['shift+h']",
            "['space']",
            "['tab']",
            "['é']",
        ] {
            let text = format!("[keybindings]\nmove_left = {binding}");
            assert!(Config::from_toml(&text).is_err(), "{text}");
        }
        for action in ["select", "close", "next_page"] {
            assert!(Config::from_toml(&format!("[keybindings]\n{action} = []")).is_err());
            assert!(Config::from_toml(&format!("[keybindings]\n{action} = ['x']")).is_err());
        }
        assert!(Config::from_toml("[keybindings]\nmove_left=['ctrl+j']").is_err());
    }

    #[test]
    fn normalizes_case_modifier_order_and_named_keys() {
        assert_eq!(
            " CTRL + Shift + H ".parse::<KeyChord>().unwrap(),
            "shift+ctrl+h".parse().unwrap()
        );
        for key in [
            "left", "down", "up", "right", "enter", "escape", "tab", "space", "plus",
        ] {
            assert!(key.parse::<KeyChord>().is_ok(), "{key}");
        }
        assert_eq!("alt+space".parse::<KeyChord>().unwrap().key, Key::Space);
        assert!(
            Config::from_toml("[keybindings]\nmove_left=['shift+left', 'ctrl+shift+h', 'alt+h']")
                .is_ok()
        );
    }

    #[test]
    fn loads_explicit_files_and_distinguishes_missing_default() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        assert!(Config::load(Some(&path)).is_err());
        assert_eq!(Config::load_path(&path, true).unwrap(), Config::default());
        std::fs::write(&path, "[keybindings]\nclose=['ctrl+q']").unwrap();
        let config = Config::load(Some(&path)).unwrap();
        assert_eq!(
            config.action_for("ctrl+q".parse().unwrap()),
            Some(Action::Close)
        );
        std::fs::write(&path, "not toml").unwrap();
        assert!(Config::load_path(&path, true).is_err());
        assert!(Config::load_path(directory.path(), true).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn follows_config_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.toml");
        let link = directory.path().join("config.toml");
        std::fs::write(&target, "[keybindings]\nclose=['ctrl+q']").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            Config::load(Some(&link)).unwrap(),
            Config::load(Some(&target)).unwrap()
        );
    }

    #[test]
    fn resolves_xdg_and_home_without_mutating_process_environment() {
        let home = Some(OsStr::new("/home/test"));
        assert_eq!(
            config_path(Some(OsStr::new("/custom")), None).unwrap(),
            PathBuf::from("/custom/gloob/config.toml")
        );
        for xdg in [None, Some(OsStr::new("")), Some(OsStr::new("relative"))] {
            assert_eq!(
                config_path(xdg, home).unwrap(),
                PathBuf::from("/home/test/.config/gloob/config.toml")
            );
            assert!(config_path(xdg, None).is_err());
            assert!(config_path(xdg, Some(OsStr::new("relative"))).is_err());
        }
    }

    #[test]
    fn example_is_valid() {
        assert_eq!(
            Config::from_toml(include_str!("../examples/config.toml")).unwrap(),
            Config::default()
        );
    }
}
