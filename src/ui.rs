use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use gtk::{gdk, gio, glib, prelude::*};
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};

use crate::app::{COLUMNS, Message, Retry, State, Workers};
use crate::config::{Action, Appearance, Config, Key, KeyChord, Modifiers, Theme};
use crate::previews::Previews;

const TILE_HEIGHT: i32 = 132;

struct Tile {
    button: gtk::Button,
    picture: gtk::Picture,
    placeholder: gtk::Label,
}

struct Picker {
    window: gtk::ApplicationWindow,
    search: gtk::SearchEntry,
    grid: gtk::Grid,
    scroll: gtk::ScrolledWindow,
    status: gtk::Label,
    retry: gtk::Button,
    state: State,
    workers: Workers,
    tiles: Vec<Tile>,
    previews: Previews,
    pending_search: Option<Instant>,
    config: Config,
}

pub fn run(config: Config) -> Result<()> {
    ensure!(
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        "a wayland session is required for the picker"
    );
    // do not fall back to x11 when launched in a mixed desktop session.
    gdk::set_allowed_backends("wayland");
    gtk::init().map_err(|error| anyhow::anyhow!("could not initialize gtk: {error}"))?;
    ensure!(
        gtk4_layer_shell::is_supported(),
        "the compositor does not support gtk layer-shell; use a supported wayland compositor such as sway"
    );
    let workers = Workers::new()?;
    let application = gtk::Application::new(
        Some("io.github.karllovendahl.gloob"),
        gio::ApplicationFlags::NON_UNIQUE,
    );
    let setup = Rc::new(RefCell::new(Some((config, workers))));
    application.connect_activate(move |app| {
        let Some((config, workers)) = setup.borrow_mut().take() else {
            return;
        };
        Picker::open(app, config, workers);
    });
    let result = application.run_with_args::<&str>(&[]);
    ensure!(
        result == glib::ExitCode::SUCCESS,
        "picker exited unsuccessfully"
    );
    Ok(())
}

impl Picker {
    fn open(app: &gtk::Application, config: Config, workers: Workers) {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("gloob")
            .default_width(760)
            .default_height(530)
            .resizable(false)
            .build();
        window.init_layer_shell();
        window.set_namespace(Some("gloob"));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::Exclusive);
        window.add_css_class("gloob");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_margin_top(20);
        root.set_margin_bottom(14);
        root.set_margin_start(20);
        root.set_margin_end(20);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("search gifs")
            .hexpand(true)
            .build();
        search.add_css_class("search-field");
        search.set_margin_bottom(16);
        root.append(&search);
        let grid = gtk::Grid::builder()
            .column_spacing(10)
            .row_spacing(10)
            .column_homogeneous(true)
            .build();
        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            // reserve a gutter instead of drawing the scrollbar over the last column.
            .overlay_scrolling(false)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&grid)
            .build();
        root.append(&scroll);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        footer.add_css_class("footer");
        let status = gtk::Label::builder()
            .label("loading gifs…")
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .build();
        status.add_css_class("status");
        let retry = gtk::Button::with_label("retry");
        retry.add_css_class("retry");
        retry.set_visible(false);
        let hint = gtk::Label::new(Some(&binding_hint(&config)));
        hint.add_css_class("shortcuts");
        hint.set_ellipsize(gtk::pango::EllipsizeMode::End);
        hint.set_tooltip_text(Some(&binding_hint(&config)));
        footer.append(&status);
        footer.append(&retry);
        footer.append(&hint);
        root.append(&footer);
        window.set_child(Some(&root));
        let css = gtk::CssProvider::new();
        css.load_from_data(&appearance_css(&config.appearance));
        gtk::style_context_add_provider_for_display(
            &WidgetExt::display(&window),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let picker = Rc::new(RefCell::new(Self {
            window: window.clone(),
            search: search.clone(),
            grid,
            scroll,
            status,
            retry: retry.clone(),
            state: State::default(),
            workers,
            tiles: Vec::new(),
            previews: Previews::default(),
            pending_search: None,
            config,
        }));
        let weak = Rc::downgrade(&picker);
        search.connect_changed(move |entry| {
            if let Some(picker) = weak.upgrade() {
                let mut picker = picker.borrow_mut();
                if picker.state.copying {
                    return;
                }
                let query = entry.text().trim().to_owned();
                if query == picker.state.query {
                    return;
                }
                let generation = picker.state.change_query(query);
                picker.workers.invalidate(generation);
                picker.clear_tiles();
                picker.pending_search = Some(Instant::now() + Duration::from_millis(350));
                picker.update_status();
            }
        });
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(&picker);
        controller.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(picker) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let input = chord(key, modifiers);
            {
                let picker = picker.borrow();
                if footer_activation(input, picker.retry.has_focus()) {
                    return glib::Propagation::Proceed;
                }
            }
            let action = input.and_then(|chord| picker.borrow().config.action_for(chord));
            if let Some(action) = action {
                picker.borrow_mut().action(action);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(controller);
        let weak = Rc::downgrade(&picker);
        retry.connect_clicked(move |_| {
            if let Some(picker) = weak.upgrade() {
                let mut picker = picker.borrow_mut();
                match picker.state.retry.clone() {
                    Some(Retry::Copy) => picker.action(Action::Select),
                    Some(Retry::Search(cursor)) => picker.start_search(cursor),
                    None => {}
                }
            }
        });
        // the source owns the picker, but not vice versa. it drops all animation state when unmapped.
        glib::timeout_add_local(Duration::from_millis(20), move || {
            if !picker.borrow().window.is_visible() {
                return glib::ControlFlow::Break;
            }
            Self::tick(&picker);
            glib::ControlFlow::Continue
        });
        window.present();
        search.grab_focus();
        // the first tick starts the feed without delaying window presentation.
        // search state is independent of keyboard focus and ordinary entry editing.
    }

    fn clear_tiles(&mut self) {
        self.previews.reset(self.state.generation);
        self.tiles.clear();
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
    }

    fn start_search(&mut self, cursor: Option<String>) {
        if !self.state.begin_search(cursor.clone()) {
            return;
        }
        self.pending_search = None;
        if cursor.is_none() {
            self.clear_tiles();
            self.scroll.vadjustment().set_value(0.0);
            self.workers.invalidate(self.state.generation);
        }
        if let Err(error) =
            self.workers
                .search(self.state.generation, self.state.query.clone(), cursor)
        {
            self.state.accept_page(self.state.generation, Err(error));
        }
        self.update_status();
    }

    fn action(&mut self, action: Action) {
        match action {
            Action::NextPage => {
                if let Some(cursor) = self.state.next_page_cursor() {
                    self.start_search(Some(cursor));
                }
            }
            Action::Close => {
                if self.state.copying {
                    return;
                }
                self.window.close();
            }
            Action::Select => {
                if self.state.loading && self.state.items.is_empty() {
                    return;
                }
                if let Some(gif) = self.state.begin_copy() {
                    if let Err(error) = self.workers.copy(gif) {
                        self.state.fail_copy(error);
                    }
                    self.search.set_sensitive(!self.state.copying);
                    self.update_status();
                }
            }
            _ => {
                self.state.navigate(action);
                self.update_selection(true);
            }
        }
    }

    fn update_selection(&self, scroll: bool) {
        for (index, tile) in self.tiles.iter().enumerate() {
            if index == self.state.selected {
                tile.button.add_css_class("selected");
            } else {
                tile.button.remove_css_class("selected");
            }
        }
        if scroll && let Some(tile) = self.tiles.get(self.state.selected) {
            let adjustment = self.scroll.vadjustment();
            let allocation = tile.button.allocation();
            let top = f64::from(allocation.y());
            let bottom = top + f64::from(allocation.height());
            let view_top = adjustment.value();
            if top < view_top {
                adjustment.set_value(top);
            } else if bottom > view_top + adjustment.page_size() {
                adjustment.set_value(bottom - adjustment.page_size());
            }
        }
    }

    fn update_status(&self) {
        let text = if self.state.copying {
            "downloading and copying…".to_owned()
        } else if let Some(error) = &self.state.error {
            error.clone()
        } else if self.state.loading {
            if self.state.items.is_empty() {
                "loading gifs…"
            } else {
                "loading more gifs…"
            }
            .to_owned()
        } else if self.state.items.is_empty() {
            "no gifs found".to_owned()
        } else {
            format!("{} gifs · powered by gifsnap", self.state.items.len())
        };
        self.status.set_text(&text);
        self.retry
            .set_visible(self.state.error.is_some() && !self.state.copying);
        if self.state.error.is_some() {
            self.status.add_css_class("error");
        } else {
            self.status.remove_css_class("error");
        }
        for tile in &self.tiles {
            tile.button.set_sensitive(!self.state.copying);
        }
    }

    fn populate(picker: &Rc<RefCell<Self>>) {
        let mut this = picker.borrow_mut();
        let first = this.tiles.len();
        for index in first..this.state.items.len() {
            let gif = this.state.items[index].clone();
            let picture = gtk::Picture::builder()
                .width_request(160)
                .height_request(TILE_HEIGHT)
                .can_shrink(true)
                .build();
            let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let preview = gtk::Overlay::new();
            preview.set_child(Some(&picture));
            let placeholder = gtk::Label::new(Some("loading…"));
            placeholder.add_css_class("preview-placeholder");
            preview.add_overlay(&placeholder);
            content.append(&preview);
            let button = gtk::Button::builder()
                .child(&content)
                .focusable(false)
                .tooltip_text(format!("{}\n{}", gif.title, gif.page_url))
                .build();
            button.add_css_class("gif-tile");
            button.update_property(&[gtk::accessible::Property::Label(&gif.title)]);
            let weak = Rc::downgrade(picker);
            button.connect_clicked(move |_| {
                if let Some(picker) = weak.upgrade() {
                    let mut picker = picker.borrow_mut();
                    if picker.state.copying {
                        return;
                    }
                    picker.state.selected = index;
                    picker.update_selection(false);
                    picker.action(Action::Select);
                }
            });
            this.grid.attach(
                &button,
                (index % COLUMNS) as i32,
                (index / COLUMNS) as i32,
                1,
                1,
            );
            this.tiles.push(Tile {
                button,
                picture,
                placeholder,
            });
        }
        if first == 0 {
            this.scroll.vadjustment().set_value(0.0);
            if !this.state.copying {
                this.search.grab_focus();
            }
        }
        this.update_selection(false);
        this.update_status();
    }

    fn tick(picker: &Rc<RefCell<Self>>) {
        {
            let mut this = picker.borrow_mut();
            if this.state.generation == 0
                || this
                    .pending_search
                    .is_some_and(|deadline| Instant::now() >= deadline)
            {
                this.start_search(None);
            }
        }
        loop {
            let message = picker.borrow().workers.messages.try_recv();
            let Ok(message) = message else {
                break;
            };
            match message {
                Message::Page(generation, result) => {
                    let accepted = picker.borrow_mut().state.accept_page(generation, result);
                    if accepted {
                        Self::populate(picker);
                    }
                }
                Message::Copied(result) => {
                    let mut this = picker.borrow_mut();
                    match result {
                        Ok(warning) => {
                            if let Some(warning) = warning {
                                eprintln!("gloob: {warning}");
                            }
                            this.window.close();
                        }
                        Err(error) => {
                            this.state.fail_copy(error);
                            this.search.set_sensitive(true);
                            this.search.grab_focus();
                            this.update_status();
                        }
                    }
                }
            }
        }
        let mut this = picker.borrow_mut();
        this.sync_previews();
        let adjustment = this.scroll.vadjustment();
        // newly appended widgets need a layout before their scroll extent is meaningful.
        if near_end(
            adjustment.value(),
            adjustment.page_size(),
            adjustment.upper(),
            this.tiles
                .last()
                .map_or(0, |tile| tile.button.allocation().height()),
        ) && let Some(cursor) = this.state.automatic_page_cursor()
        {
            this.start_search(Some(cursor));
        }
        while let Ok(message) = this.workers.preview_messages.try_recv() {
            if message.generation != this.state.generation {
                continue;
            }
            let index = message.index;
            this.previews
                .complete(message.generation, index, message.result);
            if let Some(tile) = this.tiles.get(index) {
                if this.previews.residents.contains_key(&index) {
                    tile.placeholder.set_visible(false);
                } else if let Some(error) = this.previews.failure(index) {
                    tile.placeholder.set_text("preview unavailable");
                    tile.picture
                        .set_tooltip_text(Some(&format!("preview unavailable: {error}")));
                }
            }
        }
        for index in this.previews.wanted().to_vec() {
            if this.previews.needs(index) {
                let gif = this.state.items[index].clone();
                if this.workers.preview(this.state.generation, index, gif) {
                    this.previews.requested(index);
                    this.tiles[index].placeholder.set_text("loading…");
                    this.tiles[index].placeholder.set_visible(true);
                    this.tiles[index].picture.set_tooltip_text(None);
                }
            }
        }
        let view_top = this.scroll.vadjustment().value();
        let view_bottom = view_top + this.scroll.vadjustment().page_size();
        let Self {
            tiles, previews, ..
        } = &mut *this;
        for (index, tile) in &mut previews.residents {
            let allocation = tiles[*index].button.allocation();
            let top = f64::from(allocation.y());
            let visible = top + f64::from(allocation.height()) >= view_top && top <= view_bottom;
            if visible && Instant::now() >= tile.next_frame {
                let frame = &tile.animation.frames[tile.frame];
                let bytes = glib::Bytes::from(&frame.rgba);
                let texture = gdk::MemoryTexture::new(
                    tile.animation.width as i32,
                    tile.animation.height as i32,
                    gdk::MemoryFormat::R8g8b8a8,
                    &bytes,
                    tile.animation.width * 4,
                );
                tiles[*index].picture.set_paintable(Some(&texture));
                tile.next_frame = Instant::now() + frame.delay;
                tile.frame = (tile.frame + 1) % tile.animation.frames.len();
            }
        }
    }

    fn sync_previews(&mut self) {
        let adjustment = self.scroll.vadjustment();
        let top = adjustment.value();
        let bottom = top + adjustment.page_size();
        let center = (top + bottom) / 2.0;
        let mut candidates: Vec<_> = self
            .tiles
            .iter()
            .enumerate()
            .filter_map(|(index, tile)| {
                let allocation = tile.button.allocation();
                if allocation.height() < TILE_HEIGHT {
                    return None;
                }
                let start = f64::from(allocation.y());
                let end = start + f64::from(allocation.height());
                (end >= top - 150.0 && start <= bottom + 150.0).then_some((
                    index,
                    end >= top && start <= bottom,
                    ((start + end) / 2.0 - center).abs(),
                ))
            })
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.total_cmp(&b.2)));
        for index in self
            .previews
            .set_wanted(candidates.into_iter().map(|item| item.0).collect())
        {
            self.tiles[index]
                .picture
                .set_paintable(None::<&gdk::Paintable>);
            self.tiles[index].placeholder.set_text("loading…");
            self.tiles[index].placeholder.set_visible(true);
        }
    }
}

fn near_end(top: f64, height: f64, total: f64, last_tile_height: i32) -> bool {
    last_tile_height >= TILE_HEIGHT && height > 0.0 && total > 0.0 && top + height >= total - 150.0
}

fn appearance_css(appearance: &Appearance) -> String {
    let (background, foreground, error) = match appearance.theme {
        Theme::Dark => ("#252629", "#f1f1f1", "#ff958e"),
        Theme::Light => ("#f6f7f8", "#202227", "#aa3030"),
        Theme::System => ("@theme_bg_color", "@theme_fg_color", "@error_color"),
    };
    format!(
        "@define-color gloob_bg {background};\n@define-color gloob_panel alpha({background}, {});\n@define-color gloob_fg {foreground};\n@define-color gloob_error {error};\n{}",
        appearance.background_opacity,
        include_str!("picker.css")
    )
}

fn footer_activation(input: Option<KeyChord>, footer_focused: bool) -> bool {
    footer_focused
        && input.is_some_and(|chord| {
            chord.modifiers == Modifiers::default() && matches!(chord.key, Key::Enter | Key::Space)
        })
}

fn binding_hint(config: &Config) -> String {
    fn display(chord: KeyChord) -> String {
        let mut text = String::new();
        if chord.modifiers.ctrl {
            text.push_str("ctrl+");
        }
        if chord.modifiers.alt {
            text.push_str("alt+");
        }
        if chord.modifiers.shift {
            text.push_str("shift+");
        }
        text.push_str(&match chord.key {
            Key::Character(character) => character.to_string(),
            Key::Left => "left".into(),
            Key::Down => "down".into(),
            Key::Up => "up".into(),
            Key::Right => "right".into(),
            Key::Enter => "enter".into(),
            Key::Escape => "escape".into(),
            Key::Tab => "tab".into(),
            Key::Space => "space".into(),
        });
        text
    }
    let movement = [
        Action::MoveLeft,
        Action::MoveDown,
        Action::MoveUp,
        Action::MoveRight,
    ]
    .map(|action| config.bindings(action)[0]);
    let ctrl_letters = movement.iter().all(|chord| {
        chord.modifiers
            == Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            }
            && matches!(chord.key, Key::Character(_))
    });
    let movement = if ctrl_letters {
        format!(
            "ctrl+{}",
            movement
                .map(|chord| match chord.key {
                    Key::Character(key) => key.to_string(),
                    _ => unreachable!(),
                })
                .join("/")
        )
    } else {
        movement.map(display).join("/")
    };
    format!(
        "{movement} move · {} more · {} copy · {} close",
        display(config.bindings(Action::NextPage)[0]),
        display(config.bindings(Action::Select)[0]),
        display(config.bindings(Action::Close)[0])
    )
}

fn chord(key: gdk::Key, state: gdk::ModifierType) -> Option<KeyChord> {
    if state.intersects(
        gdk::ModifierType::SUPER_MASK
            | gdk::ModifierType::META_MASK
            | gdk::ModifierType::HYPER_MASK,
    ) {
        return None;
    }
    let key = match key {
        gdk::Key::Left => Key::Left,
        gdk::Key::Down => Key::Down,
        gdk::Key::Up => Key::Up,
        gdk::Key::Right => Key::Right,
        gdk::Key::Return | gdk::Key::KP_Enter => Key::Enter,
        gdk::Key::Escape => Key::Escape,
        gdk::Key::Tab | gdk::Key::ISO_Left_Tab => Key::Tab,
        gdk::Key::space => Key::Space,
        other => Key::Character(other.to_unicode()?.to_ascii_lowercase()),
    };
    Some(KeyChord {
        key,
        modifiers: Modifiers {
            ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
            alt: state.contains(gdk::ModifierType::ALT_MASK),
            shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_enter_and_space_activate_the_focused_control_not_the_gif() {
        let enter = Some("enter".parse().unwrap());
        assert!(footer_activation(enter, true));
        assert!(!footer_activation(enter, false));
        assert!(footer_activation(Some("space".parse().unwrap()), true));
        assert!(!footer_activation(Some("ctrl+j".parse().unwrap()), true));
        assert!(!footer_activation(Some("escape".parse().unwrap()), true));
    }

    #[test]
    #[ignore = "requires an isolated wayland display; never opens on the user's desktop"]
    fn evicting_a_tile_releases_its_gtk_texture_and_allows_reload() {
        assert!(
            std::env::var_os("GLOOB_HEADLESS_TEST").is_some(),
            "run only with an isolated test compositor"
        );
        gdk::set_allowed_backends("wayland");
        gtk::init().unwrap();
        let app = gtk::Application::new(None::<&str>, gio::ApplicationFlags::NON_UNIQUE);
        app.register(None::<&gio::Cancellable>).unwrap();
        let window = gtk::ApplicationWindow::builder()
            .application(&app)
            .default_width(760)
            .default_height(530)
            .build();
        let grid = gtk::Grid::builder()
            .column_spacing(10)
            .row_spacing(10)
            .build();
        let scroll = gtk::ScrolledWindow::builder().child(&grid).build();
        window.set_child(Some(&scroll));
        let mut state = State::default();
        state.generation = 1;
        state.items = (0..24)
            .map(|index| {
                let url = url::Url::parse("https://media.gifsnap.com/test.gif").unwrap();
                crate::provider::Gif {
                    id: index.to_string(),
                    title: "test".into(),
                    page_url: url.clone(),
                    preview_url: None,
                    download_url: url,
                    source: None,
                    content_id: None,
                }
            })
            .collect();
        let mut previews = Previews::default();
        previews.reset(1);
        let picker = Rc::new(RefCell::new(Picker {
            window: window.clone(),
            grid,
            scroll: scroll.clone(),
            state,
            previews,
            workers: Workers::new().unwrap(),
            search: gtk::SearchEntry::new(),
            status: gtk::Label::new(None),
            retry: gtk::Button::new(),
            tiles: Vec::new(),
            pending_search: None,
            config: Config::default(),
        }));
        Picker::populate(&picker);
        window.present();
        let context = glib::MainContext::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        while (picker.borrow().tiles[23].button.allocation().height() < TILE_HEIGHT
            || scroll.vadjustment().upper() <= scroll.vadjustment().page_size())
            && Instant::now() < deadline
        {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        {
            let mut picker = picker.borrow_mut();
            picker.sync_previews();
            assert!(picker.previews.needs(0));
            picker.previews.requested(0);
            let bytes = crate::test_media::animation(2, 1, 2);
            picker
                .previews
                .complete(1, 0, crate::preview::decode(&bytes));
            assert!(picker.previews.residents.contains_key(&0));
            let pixels = glib::Bytes::from_static(&[255, 0, 0, 255, 255, 0, 0, 255]);
            let texture = gdk::MemoryTexture::new(2, 1, gdk::MemoryFormat::R8g8b8a8, &pixels, 8);
            picker.tiles[0].picture.set_paintable(Some(&texture));
            picker.tiles[0].placeholder.set_visible(false);
            assert!(picker.tiles[0].picture.paintable().is_some());
            scroll
                .vadjustment()
                .set_value(scroll.vadjustment().upper() - scroll.vadjustment().page_size());
            picker.sync_previews();
            assert!(!picker.previews.residents.contains_key(&0));
            assert!(picker.tiles[0].picture.paintable().is_none());
            assert!(picker.tiles[0].placeholder.is_visible());
            scroll.vadjustment().set_value(0.0);
            picker.sync_previews();
            assert!(picker.previews.needs(0));
        }
        window.close();
    }

    #[test]
    fn only_a_laid_out_viewport_near_the_bottom_requests_more() {
        assert!(!near_end(0.0, 400.0, 1000.0, 142));
        assert!(!near_end(0.0, 0.0, 0.0, 0));
        // gtk gives a new, unallocated button a small placeholder rectangle.
        assert!(!near_end(0.0, 404.0, 404.0, 10));
        assert!(near_end(450.0, 400.0, 1000.0, 142));
        assert!(near_end(0.0, 400.0, 300.0, 142));
    }

    #[test]
    fn key_adapter_resolves_actions_without_consuming_normal_typing() {
        let config = Config::default();
        let ctrl = gdk::ModifierType::CONTROL_MASK;
        assert_eq!(
            config.action_for(chord(gdk::Key::h, ctrl).unwrap()),
            Some(Action::MoveLeft)
        );
        assert_eq!(
            config.action_for(chord(gdk::Key::H, ctrl | gdk::ModifierType::LOCK_MASK).unwrap()),
            Some(Action::MoveLeft)
        );
        assert_eq!(
            config.action_for(chord(gdk::Key::h, gdk::ModifierType::empty()).unwrap()),
            None
        );
        assert_eq!(
            config.action_for(chord(gdk::Key::Return, gdk::ModifierType::empty()).unwrap()),
            Some(Action::Select)
        );
        assert!(chord(gdk::Key::h, ctrl | gdk::ModifierType::SUPER_MASK).is_none());
        let custom = Config::from_toml("[keybindings]\nclose=['ctrl+q']").unwrap();
        assert!(binding_hint(&custom).contains("ctrl+q close"));
        assert!(binding_hint(&custom).contains("ctrl+n more"));
        assert!(binding_hint(&custom).contains("ctrl+h/j/k/l move"));
        let pagination = Config::from_toml("[keybindings]\nnext_page=['alt+n']").unwrap();
        assert!(binding_hint(&pagination).contains("alt+n more"));
        assert_eq!(
            pagination.action_for(chord(gdk::Key::n, gdk::ModifierType::ALT_MASK).unwrap()),
            Some(Action::NextPage)
        );
        assert!(!binding_hint(&custom).contains("escape"));
    }
}
