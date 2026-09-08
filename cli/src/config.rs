use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use jio_client::VmSize;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame, TerminalOptions, Viewport};
use std::env;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CONFIG_FILE: &str = "config";
const CREDENTIALS_FILE: &str = "credentials";
const MAX_CREDENTIAL_BYTES: u64 = 4096;
const MAX_CONFIG_BYTES: u64 = 128;
const SIZES: [VmSize; 3] = [VmSize::Small, VmSize::Medium, VmSize::Large];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Config {
    pub size: VmSize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            size: VmSize::Medium,
        }
    }
}

pub fn load() -> io::Result<Config> {
    ConfigStore::discover()?.load()
}

pub fn login(host: &str, api_key: &str) -> io::Result<()> {
    let client = jio_client::SessionClient::new(host, api_key)?;
    if client.endpoint().contains(['\n', '\r']) {
        return Err(invalid_input("Jio endpoint must not contain line endings"));
    }
    let contents = format!("{}\n{api_key}\n", client.endpoint());
    if contents.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err(invalid_input("Jio credentials are too large"));
    }
    let usage = client.usage()?;
    ConfigStore::discover()?.write(CREDENTIALS_FILE, contents.as_bytes())?;
    println!("Logged in to Jio as {}.", usage.account_id);
    if env::var_os("JIO_API_KEY").is_some() {
        println!("JIO_API_KEY is set and overrides this saved login; unset it to use this key.");
    }
    Ok(())
}

pub fn api_key(host: &str) -> io::Result<String> {
    match env::var("JIO_API_KEY") {
        Ok(key) => Ok(key),
        Err(env::VarError::NotUnicode(_)) => Err(invalid_input("JIO_API_KEY is not UTF-8")),
        Err(env::VarError::NotPresent) => {
            let contents = ConfigStore::discover()?
                .read(CREDENTIALS_FILE, MAX_CREDENTIAL_BYTES)?
                .ok_or_else(|| {
                    invalid_input("not logged in; run jio login <api-key> or set JIO_API_KEY")
                })?;
            let (endpoint, key) = contents
                .strip_suffix('\n')
                .and_then(|value| value.split_once('\n'))
                .ok_or_else(|| invalid_data("invalid saved Jio credentials"))?;
            let client = jio_client::SessionClient::new(endpoint, key)?;
            if client.endpoint() != host {
                return Err(invalid_input(
                    "saved login belongs to a different endpoint; run jio login <api-key> --host <host> or set JIO_API_KEY",
                ));
            }
            Ok(key.to_owned())
        }
    }
}

pub fn run() -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "jio config requires an interactive terminal",
        ));
    }

    let store = ConfigStore::discover()?;
    let config = store.load()?;
    let result = (|| {
        let mut terminal = ratatui::try_init_with_options(TerminalOptions {
            viewport: Viewport::Inline(6),
        })?;
        let result = ConfigApp::new(config.size).run(&mut terminal, &store);
        let clear = terminal.clear();
        let cursor = terminal.show_cursor();
        result.and(clear).and(cursor)
    })();
    let restore = crossterm::terminal::disable_raw_mode();
    result.and(restore)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum View {
    Settings,
    Sizes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Action {
    Stay,
    Save(VmSize),
    Exit,
}

struct ConfigApp {
    size: VmSize,
    highlighted: usize,
    view: View,
    notice: Option<String>,
}

impl ConfigApp {
    fn new(size: VmSize) -> Self {
        Self {
            size,
            highlighted: size_index(size),
            view: View::Settings,
            notice: None,
        }
    }

    fn run(mut self, terminal: &mut DefaultTerminal, store: &ConfigStore) -> io::Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(());
            }
            match self.handle_key(key.code) {
                Action::Stay => {}
                Action::Exit => return Ok(()),
                Action::Save(size) => {
                    store.save(Config { size })?;
                    self.size = size;
                    self.notice = Some(format!("Saved {} as the default size", size.label()));
                }
            }
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> Action {
        match self.view {
            View::Settings => match code {
                KeyCode::Enter | KeyCode::Right => {
                    self.highlighted = size_index(self.size);
                    self.notice = None;
                    self.view = View::Sizes;
                    Action::Stay
                }
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => Action::Exit,
                _ => Action::Stay,
            },
            View::Sizes => match code {
                KeyCode::Up => {
                    self.highlighted = self.highlighted.checked_sub(1).unwrap_or(SIZES.len() - 1);
                    Action::Stay
                }
                KeyCode::Down => {
                    self.highlighted = (self.highlighted + 1) % SIZES.len();
                    Action::Stay
                }
                KeyCode::Enter => {
                    let size = SIZES[self.highlighted];
                    self.view = View::Settings;
                    Action::Save(size)
                }
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Left => {
                    self.highlighted = size_index(self.size);
                    self.view = View::Settings;
                    Action::Stay
                }
                KeyCode::Char('q') => Action::Exit,
                _ => Action::Stay,
            },
        }
    }

    fn draw(&self, frame: &mut Frame) {
        match self.view {
            View::Settings => self.draw_settings(frame, frame.area()),
            View::Sizes => self.draw_sizes(frame, frame.area()),
        }
    }

    fn draw_settings(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("? ", Style::new().cyan().bold()),
                Span::styled("Size", Style::new().bold()),
                Span::raw("    "),
                Span::raw(self.size.label()),
                Span::styled(
                    format!("    {}", size_detail(self.size)),
                    Style::new().dark_gray(),
                ),
            ])),
            rows[0],
        );
        if let Some(notice) = &self.notice {
            frame.render_widget(
                Paragraph::new(notice.as_str()).style(Style::new().green()),
                rows[1],
            );
        }
        frame.render_widget(
            Paragraph::new("enter select  ·  backspace/esc exit").style(Style::new().dark_gray()),
            rows[3],
        );
    }

    fn draw_sizes(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(SIZES.len() as u16),
            Constraint::Length(1),
        ])
        .split(area);
        frame.render_widget(Paragraph::new("Size").style(Style::new().bold()), rows[0]);

        let items = SIZES.into_iter().map(|size| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<10}", size.label()), Style::new().bold()),
                Span::styled(size_detail(size), Style::new().dark_gray()),
            ]))
        });
        let list = List::new(items)
            .highlight_symbol("› ")
            .highlight_style(Style::new().cyan().add_modifier(Modifier::BOLD));
        let mut state = ListState::default().with_selected(Some(self.highlighted));
        frame.render_stateful_widget(list, rows[1], &mut state);
        frame.render_widget(
            Paragraph::new("↑/↓ move  ·  enter save  ·  backspace/esc back")
                .style(Style::new().dark_gray()),
            rows[2],
        );
    }
}

fn size_index(size: VmSize) -> usize {
    SIZES
        .iter()
        .position(|candidate| *candidate == size)
        .unwrap_or_default()
}

fn size_detail(size: VmSize) -> String {
    let memory = if size.memory_mib() < 1024 {
        format!("{} MiB", size.memory_mib())
    } else {
        format!("{} GiB", size.memory_mib() / 1024)
    };
    format!("{} vCPU · {memory}", size.vcpus())
}

struct ConfigStore {
    root: PathBuf,
}

impl ConfigStore {
    fn discover() -> io::Result<Self> {
        let root = match env::var_os("JIO_STATE_DIR") {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            Some(_) => return Err(invalid_input("JIO_STATE_DIR must not be empty")),
            None => env::var_os("HOME")
                .filter(|path| !path.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| invalid_input("HOME or JIO_STATE_DIR is required"))?
                .join(".jio"),
        };
        Ok(Self { root })
    }

    #[cfg(test)]
    fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn read(&self, name: &str, max_bytes: u64) -> io::Result<Option<String>> {
        create_private_directory(&self.root)?;
        let path = self.root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        require_private_file(&path)?;
        let expected = fs::symlink_metadata(&path)?;
        let file = File::open(&path)?;
        let opened = file.metadata()?;
        if opened.dev() != expected.dev() || opened.ino() != expected.ino() {
            return Err(invalid_data("local Jio file changed while it was opened"));
        }
        let mut bytes = Vec::new();
        file.take(max_bytes + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max_bytes {
            return Err(invalid_data("local Jio file is too large"));
        }
        String::from_utf8(bytes).map(Some).map_err(invalid_data)
    }

    fn load(&self) -> io::Result<Config> {
        let Some(contents) = self.read(CONFIG_FILE, MAX_CONFIG_BYTES)? else {
            return Ok(Config::default());
        };
        let line = contents.strip_suffix('\n').unwrap_or(&contents);
        if line.contains(['\n', '\r']) {
            return Err(invalid_data("local Jio config has invalid line endings"));
        }
        let size = line
            .strip_prefix("size=")
            .ok_or_else(|| invalid_data("local Jio config must contain one size setting"))?
            .parse::<VmSize>()
            .map_err(|_| invalid_data("local Jio config contains an unknown size"))?;
        Ok(Config { size })
    }

    fn save(&self, config: Config) -> io::Result<()> {
        self.write(
            CONFIG_FILE,
            format!("size={}\n", config.size.id()).as_bytes(),
        )
    }

    fn write(&self, name: &str, contents: &[u8]) -> io::Result<()> {
        create_private_directory(&self.root)?;
        let target = self.root.join(name);
        match fs::symlink_metadata(&target) {
            Ok(_) => require_private_file(&target)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        replace_private_file(&self.root, &target, contents)
    }
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.mode() & 0o777 == 0o700 =>
        {
            Ok(())
        }
        Ok(_) => Err(invalid_data(format!(
            "local state path is not a private regular directory: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(path)
        }
        Err(error) => Err(error),
    }
}

fn require_private_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid_data("local config path has no parent directory"))?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != parent_metadata.uid()
    {
        return Err(invalid_data(format!(
            "local config path is not a private regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn replace_private_file(directory: &Path, target: &Path, contents: &[u8]) -> io::Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for suffix in 0..16_u8 {
        let temporary = directory.join(format!(
            ".{CONFIG_FILE}-{}-{timestamp}-{suffix}",
            std::process::id()
        ));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        if let Err(error) = file.write_all(contents).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temporary, target) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        require_private_file(target)?;
        return File::open(directory)?.sync_all();
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a temporary Jio config file",
    ))
}

fn invalid_input(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

fn invalid_data(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::{Action, Config, ConfigApp, ConfigStore, View, size_detail};
    use crossterm::event::KeyCode;
    use jio_client::VmSize;
    use std::fs;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> io::Result<Self> {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "jio-config-test-{}-{timestamp}",
                std::process::id()
            ));
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_to_medium_and_round_trips_each_size_privately() -> io::Result<()> {
        let directory = TestDirectory::new()?;
        let store = ConfigStore::at(directory.path());
        assert_eq!(store.load()?, Config::default());
        assert_eq!(store.load()?.size, VmSize::Medium);
        for size in VmSize::ALL {
            store.save(Config { size })?;
            assert_eq!(store.load()?.size, size);
        }
        assert_eq!(
            fs::metadata(directory.path().join("config"))?.mode() & 0o777,
            0o600
        );
        Ok(())
    }

    #[test]
    fn rejects_an_unknown_persisted_size() -> io::Result<()> {
        let directory = TestDirectory::new()?;
        let store = ConfigStore::at(directory.path());
        let _ = store.load()?;
        store.save(Config::default())?;
        fs::write(directory.path().join("config"), b"size=enormous\n")?;
        assert!(store.load().is_err());
        Ok(())
    }

    #[test]
    fn navigates_saves_and_goes_back_without_changing_the_size() {
        let mut app = ConfigApp::new(VmSize::Small);
        assert_eq!(app.handle_key(KeyCode::Enter), Action::Stay);
        assert_eq!(app.view, View::Sizes);
        assert_eq!(app.handle_key(KeyCode::Down), Action::Stay);
        assert_eq!(app.handle_key(KeyCode::Enter), Action::Save(VmSize::Medium));
        assert_eq!(app.view, View::Settings);

        app.size = VmSize::Medium;
        assert_eq!(app.handle_key(KeyCode::Enter), Action::Stay);
        assert_eq!(app.handle_key(KeyCode::Down), Action::Stay);
        assert_eq!(app.handle_key(KeyCode::Backspace), Action::Stay);
        assert_eq!(app.view, View::Settings);
        assert_eq!(app.size, VmSize::Medium);
    }

    #[test]
    fn draws_inline_at_the_prompt_without_replacing_terminal_history()
    -> Result<(), std::convert::Infallible> {
        use ratatui::backend::{Backend, TestBackend};
        use ratatui::{Terminal, TerminalOptions, Viewport};

        let mut backend = TestBackend::new(80, 24);
        let mut history = ratatui::buffer::Cell::default();
        history.set_symbol("H");
        backend.draw([(0, 0, &history)].into_iter())?;
        backend.set_cursor_position((0, 18))?;
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(6),
            },
        )?;
        let mut app = ConfigApp::new(VmSize::Small);
        terminal.draw(|frame| app.draw(frame))?;
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "H");
        assert_eq!(terminal.backend().buffer()[(0, 18)].symbol(), "?");
        app.handle_key(KeyCode::Enter);
        terminal.draw(|frame| app.draw(frame))?;
        assert_eq!(terminal.backend().buffer()[(0, 18)].symbol(), "S");
        assert_eq!(terminal.backend().buffer()[(0, 19)].symbol(), "›");
        assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "H");
        Ok(())
    }

    #[test]
    fn formats_the_fixed_resource_profiles() {
        assert_eq!(size_detail(VmSize::Small), "1 vCPU · 2 GiB");
        assert_eq!(size_detail(VmSize::Medium), "2 vCPU · 4 GiB");
        assert_eq!(size_detail(VmSize::Large), "4 vCPU · 8 GiB");
        assert_eq!(size_detail(VmSize::XLarge), "8 vCPU · 16 GiB");
    }
}
