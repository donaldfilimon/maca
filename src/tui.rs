//! Interactive terminal UI (`maca tui`).
//!
//! Tabs: **Installed**, **Search**, **Groups**, **Snapshots**.
//! The body is a list plus a detail pane. `?` opens help. Remove and snapshot
//! restore ask for confirmation; while that overlay or the help overlay is
//! open, `q` does not quit.
//!
//! Keys:
//! - `1`..`4` switch tab, `q` / `Ctrl-C` quit
//! - `j` / `k` or arrows move the selection
//! - `/` focus search, `Enter` run search, `Esc` clear
//! - `i` install the selected search result
//! - `r` remove the selected installed package (confirms first)
//! - `u` upgrade the selected installed package
//! - `s` snapshot the prefix
//! - `g` install the selected group
//! - `f` cycle the installed kind filter
//! - `p` pin or unpin the selected installed package
//! - `z` restore the selected snapshot (confirms first)

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs};
use tokio::sync::mpsc;

use crate::Result;
use crate::backend::Package;
use crate::group::{Group, Kind};
use crate::snapshot;
use crate::state::{self, Ledger};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Installed,
    Search,
    Groups,
    Snapshots,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::Installed, Tab::Search, Tab::Groups, Tab::Snapshots];
    fn title(&self) -> &'static str {
        match self {
            Tab::Installed => "Installed",
            Tab::Search => "Search",
            Tab::Groups => "Groups",
            Tab::Snapshots => "Snapshots",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    ConfirmRemove(usize),
    ConfirmRestore(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindFilter {
    All,
    App,
    Plugin,
    Cli,
    Library,
    Other,
}

struct App {
    tab: Tab,
    installed: Vec<state::Record>,
    groups: Vec<Group>,
    snapshots: Vec<snapshot::SnapshotManifest>,
    search_input: String,
    search_results: Vec<Package>,
    search_state: ListState,
    installed_state: ListState,
    group_state: ListState,
    snapshot_state: ListState,
    status: String,
    input_mode: bool,
    overlay: Overlay,
    kind_filter: KindFilter,
    pins: BTreeMap<String, String>,
    rx: mpsc::UnboundedReceiver<Vec<Package>>,
    tx: mpsc::UnboundedSender<Vec<Package>>,
}

impl App {
    fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut app = Self {
            tab: Tab::Installed,
            installed: Vec::new(),
            groups: Vec::new(),
            snapshots: Vec::new(),
            search_input: String::new(),
            search_results: Vec::new(),
            search_state: ListState::default(),
            installed_state: ListState::default(),
            group_state: ListState::default(),
            snapshot_state: ListState::default(),
            status: "ready".into(),
            input_mode: false,
            overlay: Overlay::None,
            kind_filter: KindFilter::All,
            pins: BTreeMap::new(),
            rx,
            tx,
        };
        app.reload();
        app
    }

    fn reload(&mut self) {
        let ledger = Ledger::load().unwrap_or_default();
        self.installed = ledger.packages;
        self.groups = Group::list().unwrap_or_default();
        self.snapshots = snapshot::list().unwrap_or_default();
        match state::load_pins() {
            Ok(pins) => self.pins = pins,
            Err(e) => self.status = format!("pins unreadable: {e}"),
        }
        self.clamp_selection();
    }

    fn visible_installed(&self) -> Vec<state::Record> {
        self.installed
            .iter()
            .filter(|r| kind_allowed(self.kind_filter, r.kind))
            .cloned()
            .collect()
    }

    fn clamp_selection(&mut self) {
        let len = match self.tab {
            Tab::Installed => self.visible_installed().len(),
            Tab::Search => self.search_results.len(),
            Tab::Groups => self.groups.len(),
            Tab::Snapshots => self.snapshots.len(),
        };
        let state = match self.tab {
            Tab::Installed => &mut self.installed_state,
            Tab::Search => &mut self.search_state,
            Tab::Groups => &mut self.group_state,
            Tab::Snapshots => &mut self.snapshot_state,
        };
        if len == 0 {
            state.select(None);
        } else {
            let sel = state.selected().unwrap_or(0).min(len - 1);
            state.select(Some(sel));
        }
    }

    fn selected_search(&self) -> Option<Package> {
        self.search_state
            .selected()
            .and_then(|i| self.search_results.get(i).cloned())
    }

    fn selected_installed(&self) -> Option<state::Record> {
        self.installed_state
            .selected()
            .and_then(|i| self.visible_installed().get(i).cloned())
    }

    fn selected_group(&self) -> Option<Group> {
        self.group_state
            .selected()
            .and_then(|i| self.groups.get(i).cloned())
    }

    fn move_selection(&mut self, delta: isize) {
        let len = match self.tab {
            Tab::Installed => self.visible_installed().len(),
            Tab::Search => self.search_results.len(),
            Tab::Groups => self.groups.len(),
            Tab::Snapshots => self.snapshots.len(),
        };
        if len == 0 {
            return;
        }
        let state = match self.tab {
            Tab::Installed => &mut self.installed_state,
            Tab::Search => &mut self.search_state,
            Tab::Groups => &mut self.group_state,
            Tab::Snapshots => &mut self.snapshot_state,
        };
        let cur = state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, len as isize - 1) as usize;
        state.select(Some(next));
    }

    fn run_search(&mut self) {
        let query = self.search_input.clone();
        self.status = format!("searching for '{query}'...");
        let tx = self.tx.clone();
        tracing::info!("TUI search started: {query}");
        tokio::spawn(async move {
            let results = crate::backend::search_all(&query, 25).await;
            let results = crate::insight::rank_packages(&query, results);
            tracing::info!("TUI search finished: {} results", results.len());
            let _ = tx.send(results);
        });
    }
}

/// Run the TUI. Returns when the user quits.
pub async fn run(prefix: std::path::PathBuf) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let result = event_loop(&mut terminal, &mut app, &prefix).await;

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    prefix: &std::path::Path,
) -> Result<()> {
    loop {
        // Drain any completed searches.
        while let Ok(results) = app.rx.try_recv() {
            tracing::info!("TUI drained {} search results", results.len());
            app.search_results = results;
            app.search_state.select(None);
            app.clamp_selection();
            app.status = format!("{} results", app.search_results.len());
        }
        terminal.draw(|f| draw(f, app))?;
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(());
        }
        if overlay_consumes_quit(app.overlay) {
            match key.code {
                KeyCode::Char('q') | KeyCode::Char('n') | KeyCode::Esc => {
                    app.overlay = Overlay::None;
                }
                KeyCode::Char('y') => {
                    confirm_yes(app, prefix).await;
                }
                KeyCode::Char('?') => {
                    app.overlay = if app.overlay == Overlay::Help {
                        Overlay::None
                    } else {
                        Overlay::Help
                    };
                }
                _ => {}
            }
            continue;
        }
        if app.input_mode {
            match key.code {
                KeyCode::Enter => {
                    app.input_mode = false;
                    app.run_search();
                }
                KeyCode::Esc => {
                    app.input_mode = false;
                }
                KeyCode::Backspace => {
                    app.search_input.pop();
                }
                KeyCode::Char(c) => app.search_input.push(c),
                _ => {}
            }
            continue;
        }
        // Mutating keys take the prefix lock for that one action (never for
        // the whole session; `u` takes it itself, after its network check).
        // Remove and restore only lock when the confirmation is accepted.
        // A TUI left open does not block other maca commands.
        let mutating = matches!(key.code, KeyCode::Char('i' | 's' | 'g'));
        let _lock = if mutating {
            match crate::lock::PrefixLock::try_acquire(prefix) {
                Ok(Some(lock)) => Some(lock),
                Ok(None) => {
                    app.status = "prefix busy: another maca command is running".into();
                    continue;
                }
                Err(e) => {
                    app.status = format!("cannot lock prefix: {e}");
                    continue;
                }
            }
        } else {
            None
        };
        match key.code {
            KeyCode::Char('q') => return Ok(()),
            KeyCode::Char('?') => app.overlay = Overlay::Help,
            KeyCode::Char('1') => switch_tab(app, Tab::Installed),
            KeyCode::Char('2') => switch_tab(app, Tab::Search),
            KeyCode::Char('3') => switch_tab(app, Tab::Groups),
            KeyCode::Char('4') => switch_tab(app, Tab::Snapshots),
            KeyCode::Char('/') => {
                app.input_mode = true;
                app.tab = Tab::Search;
            }
            KeyCode::Char('j') | KeyCode::Down => app.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1),
            KeyCode::Enter if app.tab == Tab::Search => app.run_search(),
            KeyCode::Char('i') => {
                if let Some(pkg) = app.selected_search() {
                    install_pkg(prefix, &pkg.name, &pkg.backend, &mut app.status).await;
                    app.reload();
                }
            }
            KeyCode::Char('r') => {
                if app.tab == Tab::Installed
                    && let Some(i) = app.installed_state.selected()
                    && app.visible_installed().get(i).is_some()
                {
                    app.overlay = Overlay::ConfirmRemove(i);
                }
            }
            KeyCode::Char('f') if app.tab == Tab::Installed => {
                app.kind_filter = cycle_kind_filter(app.kind_filter);
                app.clamp_selection();
                app.status = format!("kind filter: {}", kind_filter_label(app.kind_filter));
            }
            KeyCode::Char('p') => {
                if let Some(rec) = app.selected_installed() {
                    let name = rec.name.clone();
                    let version = rec.version.clone();
                    if let Some(_lock) = try_prefix_lock(app, prefix) {
                        toggle_pin_on_disk(app, &name, &version);
                    }
                }
            }
            KeyCode::Char('u') => {
                if let Some(rec) = app.selected_installed() {
                    upgrade_pkg(prefix, &rec.name, &mut app.status).await;
                    app.reload();
                }
            }
            KeyCode::Char('s') => {
                match snapshot::create(prefix, "manual from tui", &[]) {
                    Ok(m) => app.status = format!("snapshot {} created", m.id),
                    Err(e) => app.status = format!("snapshot failed: {e}"),
                }
                app.reload();
            }
            KeyCode::Char('g') => {
                if let Some(group) = app.selected_group() {
                    install_group(prefix, &group, &mut app.status).await;
                    app.reload();
                }
            }
            KeyCode::Char('z') => {
                if app.tab == Tab::Snapshots
                    && let Some(i) = app.snapshot_state.selected()
                    && app.snapshots.get(i).is_some()
                {
                    app.overlay = Overlay::ConfirmRestore(i);
                }
            }
            _ => {}
        }
    }
}

fn switch_tab(app: &mut App, tab: Tab) {
    app.tab = tab;
    app.clamp_selection();
}

fn try_prefix_lock(app: &mut App, prefix: &std::path::Path) -> Option<crate::lock::PrefixLock> {
    match crate::lock::PrefixLock::try_acquire(prefix) {
        Ok(Some(lock)) => Some(lock),
        Ok(None) => {
            app.status = "prefix busy: another maca command is running".into();
            None
        }
        Err(e) => {
            app.status = format!("cannot lock prefix: {e}");
            None
        }
    }
}

async fn confirm_yes(app: &mut App, prefix: &std::path::Path) {
    let overlay = app.overlay;
    app.overlay = Overlay::None;
    let Some(_lock) = try_prefix_lock(app, prefix) else {
        return;
    };
    match overlay {
        Overlay::ConfirmRemove(i) => {
            if let Some(rec) = app.visible_installed().get(i).cloned() {
                remove_pkg(prefix, &rec.name, &mut app.status);
                app.reload();
            }
        }
        Overlay::ConfirmRestore(i) => {
            if let Some(id) = app.snapshots.get(i).map(|s| s.id.clone()) {
                restore_snap(prefix, &id, &mut app.status);
                app.reload();
            }
        }
        Overlay::Help | Overlay::None => {}
    }
}

fn toggle_pin_on_disk(app: &mut App, name: &str, version: &str) {
    let Ok(_state) = state::state_lock() else {
        app.status = "cannot lock pin file".into();
        return;
    };
    let pins = match state::load_pins() {
        Ok(pins) => pins,
        Err(e) => {
            app.status = format!("pins unreadable: {e}");
            return;
        }
    };
    let (next, message) = toggle_pin(&pins, name, version);
    match state::save_pins(&next) {
        Ok(()) => {
            app.pins = next;
            app.status = message;
        }
        Err(e) => app.status = format!("pin failed: {e}"),
    }
}

fn restore_snap(prefix: &std::path::Path, id: &str, status: &mut String) {
    let snap = match snapshot_before(prefix, "restore") {
        Ok(s) => s,
        Err(msg) => {
            *status = msg;
            return;
        }
    };
    *status = match snapshot::restore(id, prefix) {
        Ok(_) => format!("restored {id} (snapshot {})", snap.id),
        Err(e) => format!("restore failed: {e}; {}", roll_back(prefix, &snap)),
    };
}

/// Snapshot the prefix before a TUI action, as the CLI commands do. The caller
/// holds the prefix lock for this action only.
fn snapshot_before(
    prefix: &std::path::Path,
    what: &str,
) -> std::result::Result<crate::snapshot::SnapshotManifest, String> {
    crate::snapshot::create(prefix, &format!("tui: before {what}"), &[])
        .map_err(|e| format!("{what} not started: snapshot failed: {e}"))
}

/// Undo a failed action; returns the text to append to the status line.
fn roll_back(prefix: &std::path::Path, snap: &crate::snapshot::SnapshotManifest) -> String {
    match crate::snapshot::restore(&snap.id, prefix) {
        Ok(_) => format!("rolled back to snapshot {}", snap.id),
        Err(e) => format!("rollback also failed: {e}"),
    }
}

async fn install_pkg(prefix: &std::path::Path, name: &str, backend: &str, status: &mut String) {
    let snap = match snapshot_before(prefix, "install") {
        Ok(s) => s,
        Err(msg) => {
            *status = msg;
            return;
        }
    };
    let opts = crate::backend::InstallOptions {
        dry_run: false,
        prefix: prefix.to_path_buf(),
        ..Default::default()
    };
    let result = crate::backend::install_with_backend(backend, name, &opts).await;
    *status = match result {
        Ok(out) => format!("installed {} {}", out.name, out.version),
        Err(e) => format!("install failed: {e}; {}", roll_back(prefix, &snap)),
    };
}

fn remove_pkg(prefix: &std::path::Path, name: &str, status: &mut String) {
    let snap = match snapshot_before(prefix, "remove") {
        Ok(s) => s,
        Err(msg) => {
            *status = msg;
            return;
        }
    };
    *status = match crate::backend::remove_with_prefix(prefix, name) {
        Ok(()) => format!("removed {name} (snapshot {})", snap.id),
        Err(e) => format!("remove failed: {e}; {}", roll_back(prefix, &snap)),
    };
}

/// Lock-ordering contract (guarded by `upgrade_takes_the_lock_after_the_check`):
/// the network check runs first with no lock; the prefix lock is taken only
/// afterwards, here, and `event_loop` must not take it for `u` (flock is not
/// re-entrant, so that would always report "prefix busy").
async fn upgrade_pkg(prefix: &std::path::Path, name: &str, status: &mut String) {
    // Version check first, with no lock held (it is a network call).
    let planned = match crate::backend::plan_upgrade(name).await {
        Ok(Ok(p)) => p,
        Ok(Err(msg)) => {
            *status = msg;
            return;
        }
        Err(e) => {
            *status = format!("upgrade check failed: {e}");
            return;
        }
    };
    // Lock only around the state change; the plan is re-validated under it.
    let _lock = match crate::lock::PrefixLock::try_acquire(prefix) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            *status = "prefix busy: another maca command is running".into();
            return;
        }
        Err(e) => {
            *status = format!("cannot lock prefix: {e}");
            return;
        }
    };
    let snap = match snapshot_before(prefix, "upgrade") {
        Ok(s) => s,
        Err(msg) => {
            *status = msg;
            return;
        }
    };
    *status = match crate::backend::apply_planned_upgrade(prefix, &planned).await {
        Ok(msg) => msg,
        Err(e) => format!("upgrade failed: {e}; {}", roll_back(prefix, &snap)),
    };
}

async fn install_group(prefix: &std::path::Path, group: &Group, status: &mut String) {
    let snap = match snapshot_before(prefix, "group install") {
        Ok(s) => s,
        Err(msg) => {
            *status = msg;
            return;
        }
    };
    let mut installed = 0;
    let mut failed = 0;
    for entry in &group.packages {
        let backend = entry.backend.as_deref().unwrap_or("auto");
        let opts = crate::backend::InstallOptions {
            dry_run: false,
            prefix: prefix.to_path_buf(),
            kind: entry.kind,
            ..Default::default()
        };
        match crate::backend::install_with_backend(backend, &entry.name, &opts).await {
            Ok(_) => installed += 1,
            Err(e) => {
                failed += 1;
                tracing::warn!("group {}: {} failed: {e}", group.name, entry.name);
            }
        }
    }
    *status = if failed > 0 {
        format!(
            "group {}: {failed} of {} failed; {}",
            group.name,
            installed + failed,
            roll_back(prefix, &snap)
        )
    } else {
        format!("group {}: {installed} installed", group.name)
    };
}

fn draw(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(f.area());

    let titles: Vec<Line> = Tab::ALL.iter().map(|t| Line::from(t.title())).collect();
    let tabs = Tabs::new(titles)
        .select(Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0))
        .block(Block::default().borders(Borders::ALL).title(" maca "))
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        );
    f.render_widget(tabs, chunks[0]);

    let (list_area, detail_area) = list_and_detail(chunks[1]);
    match app.tab {
        Tab::Installed => draw_installed(f, app, list_area),
        Tab::Search => draw_search(f, app, list_area),
        Tab::Groups => draw_groups(f, app, list_area),
        Tab::Snapshots => draw_snapshots(f, app, list_area),
    }
    draw_detail(f, app, detail_area);

    let help = match app.tab {
        Tab::Installed => "j/k move · f kind · p pin · u upgrade · r remove · s snapshot · ? help",
        Tab::Search => "/ edit query · Enter search · i install · j/k move · ? help",
        Tab::Groups => "↑/↓ move · g install group · ? help",
        Tab::Snapshots => "↑/↓ move · z restore · ? help",
    };
    let status = Paragraph::new(Line::from(vec![
        Span::styled(
            format!(" {}", app.status),
            Style::default().fg(status_color(&app.status)),
        ),
        Span::raw("  ·  "),
        Span::styled(help, Style::default().fg(Color::DarkGray)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(status, chunks[2]);

    if app.overlay != Overlay::None {
        draw_overlay(f, app);
    }

    if app.input_mode {
        let popup = centered_rect(60, 3, f.area());
        f.render_widget(Clear, popup);
        let input = Paragraph::new(app.search_input.as_str()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" search (Enter to run, Esc to cancel) "),
        );
        f.render_widget(input, popup);
    }
}

fn list_and_detail(area: Rect) -> (Rect, Rect) {
    if area.width >= 60 {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(area);
        (chunks[0], chunks[1])
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(area);
        (chunks[0], chunks[1])
    }
}

fn status_color(status: &str) -> Color {
    let lower = status.to_ascii_lowercase();
    if lower.contains("fail")
        || lower.contains("rollback")
        || lower.contains("unreadable")
        || lower.contains("busy")
    {
        Color::Red
    } else if lower.contains("searching") {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn draw_detail(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let body = match app.tab {
        Tab::Installed => app.selected_installed().map(|r| {
            let pinned = app.pins.keys().any(|k| k.eq_ignore_ascii_case(&r.name));
            format!(
                "{}\nversion {}\nbackend {}\nkind {}\npinned {}\n{}",
                r.name,
                r.version,
                r.backend,
                r.kind.as_str(),
                if pinned { "yes" } else { "no" },
                r.cellar.display()
            )
        }),
        Tab::Search => app.selected_search().map(|p| {
            format!(
                "{}\nversion {}\nbackend {}\n{}\n{}",
                p.name, p.version, p.backend, p.homepage, p.description
            )
        }),
        Tab::Groups => app.selected_group().map(|g| {
            let members: Vec<String> = g
                .packages
                .iter()
                .map(|e| match &e.version {
                    Some(v) => format!("{} {v}", e.name),
                    None => e.name.clone(),
                })
                .collect();
            format!(
                "{}\n{}\n{}",
                g.name,
                g.description,
                if members.is_empty() {
                    "no members".into()
                } else {
                    members.join("\n")
                }
            )
        }),
        Tab::Snapshots => app.snapshot_state.selected().map(|i| {
            let snap = &app.snapshots[i];
            let mut text = format!(
                "{}\n{}\n{} packages",
                snap.id,
                snap.reason,
                snap.packages.len()
            );
            if let Some(older) = app.snapshots.get(i + 1) {
                text.push('\n');
                text.push_str(&snapshot_change_summary(older, snap));
            }
            text
        }),
    };
    let paragraph = Paragraph::new(body.unwrap_or_else(|| "nothing selected".into()))
        .block(Block::default().borders(Borders::ALL).title(" detail "));
    f.render_widget(paragraph, area);
}

fn draw_overlay(f: &mut ratatui::Frame, app: &App) {
    let (title, body) = match app.overlay {
        Overlay::Help => (
            " help (q or ? closes) ",
            "1-4 tabs   j/k move   q quit\n/ search   i install   u upgrade\nr remove (confirm y/n)   z restore (confirm y/n)\nf kind filter   p pin toggle   s snapshot   g group install\nwhile this overlay is open, q does not quit"
                .to_string(),
        ),
        Overlay::ConfirmRemove(i) => {
            let name = app
                .visible_installed()
                .get(i)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| "package".into());
            (
                " confirm remove ",
                format!("remove {name}?\ny removes it   n, Esc, or q cancels"),
            )
        }
        Overlay::ConfirmRestore(i) => {
            let id = app
                .snapshots
                .get(i)
                .map(|s| s.id.as_str())
                .unwrap_or("snapshot");
            (
                " confirm restore ",
                format!("restore {id}?\ny restores it   n, Esc, or q cancels"),
            )
        }
        Overlay::None => return,
    };
    let popup = centered_rect(70, 10, f.area());
    f.render_widget(Clear, popup);
    let paragraph = Paragraph::new(body).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(paragraph, popup);
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn draw_installed(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let visible = app.visible_installed();
    let items: Vec<ListItem> = visible
        .iter()
        .map(|r| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<24}", r.name), Style::default().fg(Color::Cyan)),
                Span::raw(format!("{:<12}", r.version)),
                Span::raw(format!("{:<8}", r.backend)),
                Span::raw(format!("{:<6}", r.kind.as_str())),
                Span::styled(
                    r.cellar.display().to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            " installed ({}) ",
            kind_filter_label(app.kind_filter)
        )))
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        );
    f.render_stateful_widget(list, area, &mut app.installed_state.clone());
}

fn draw_search(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .search_results
        .iter()
        .map(|p| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<24}", p.name), Style::default().fg(Color::Cyan)),
                Span::raw(format!("{:<12}", p.version)),
                Span::raw(format!("{:<8}", p.backend)),
                Span::raw(p.description.chars().take(60).collect::<String>()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" search results "),
        )
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        );
    f.render_stateful_widget(list, area, &mut app.search_state.clone());
}

fn draw_groups(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .groups
        .iter()
        .map(|g| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<20}", g.name), Style::default().fg(Color::Cyan)),
                Span::raw(format!("{:>3} packages", g.packages.len())),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" groups "))
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        );
    f.render_stateful_widget(list, area, &mut app.group_state.clone());
}

fn draw_snapshots(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .snapshots
        .iter()
        .map(|s| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<28}", s.id), Style::default().fg(Color::Cyan)),
                Span::raw(format!("{:>3} pkgs", s.packages.len())),
                Span::raw(format!(" {}", s.reason)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" snapshots "))
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .bg(Color::DarkGray),
        );
    f.render_stateful_widget(list, area, &mut app.snapshot_state.clone());
}

/// Kind is re-exported for the ledger display.
pub fn kind_label(kind: Kind) -> &'static str {
    kind.as_str()
}

/// `q` closes help and confirmation instead of quitting the TUI.
pub fn overlay_consumes_quit(overlay: Overlay) -> bool {
    !matches!(overlay, Overlay::None)
}

pub fn cycle_kind_filter(filter: KindFilter) -> KindFilter {
    match filter {
        KindFilter::All => KindFilter::App,
        KindFilter::App => KindFilter::Plugin,
        KindFilter::Plugin => KindFilter::Cli,
        KindFilter::Cli => KindFilter::Library,
        KindFilter::Library => KindFilter::Other,
        KindFilter::Other => KindFilter::All,
    }
}

pub fn kind_filter_label(filter: KindFilter) -> &'static str {
    match filter {
        KindFilter::All => "all",
        KindFilter::App => "app",
        KindFilter::Plugin => "plugin",
        KindFilter::Cli => "cli",
        KindFilter::Library => "library",
        KindFilter::Other => "other",
    }
}

pub fn kind_allowed(filter: KindFilter, kind: Kind) -> bool {
    match filter {
        KindFilter::All => true,
        KindFilter::App => kind == Kind::App,
        KindFilter::Plugin => kind == Kind::Plugin,
        KindFilter::Cli => kind == Kind::Cli,
        KindFilter::Library => kind == Kind::Library,
        KindFilter::Other => kind == Kind::Other,
    }
}

/// Keep packages whose name or description contains `query`, then rank them.
pub fn filter_search(query: &str, packages: &[Package]) -> Vec<Package> {
    let q = query.trim().to_ascii_lowercase();
    let matched: Vec<Package> = if q.is_empty() {
        packages.to_vec()
    } else {
        packages
            .iter()
            .filter(|p| {
                p.name.to_ascii_lowercase().contains(&q)
                    || p.description.to_ascii_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    };
    crate::insight::rank_packages(query, matched)
}

/// Insert a pin, or remove it when the name is already pinned (case-insensitive).
pub fn toggle_pin(
    pins: &BTreeMap<String, String>,
    name: &str,
    version: &str,
) -> (BTreeMap<String, String>, String) {
    let mut next = pins.clone();
    if let Some(key) = next.keys().find(|k| k.eq_ignore_ascii_case(name)).cloned() {
        next.remove(&key);
        (next, format!("unpinned {name}"))
    } else {
        next.insert(name.to_string(), version.to_string());
        (next, format!("pinned {name} at {version}"))
    }
}

/// Added, removed, and version-changed packages between two snapshots.
pub fn snapshot_change_summary(
    older: &snapshot::SnapshotManifest,
    newer: &snapshot::SnapshotManifest,
) -> String {
    let delta = crate::insight::diff_snapshots(older, newer);
    format!(
        "since {}: added {} removed {} changed {}",
        older.id,
        list_or_none(&delta.added),
        list_or_none(&delta.removed),
        if delta.changed.is_empty() {
            "none".into()
        } else {
            delta
                .changed
                .iter()
                .map(|c| format!("{} {}->{}", c.name, c.from, c.to))
                .collect::<Vec<_>>()
                .join(", ")
        }
    )
}

fn list_or_none(names: &[String]) -> String {
    if names.is_empty() {
        "none".into()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_titles_unique() {
        let titles: Vec<&str> = Tab::ALL.iter().map(|t| t.title()).collect();
        let mut sorted = titles.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(titles.len(), sorted.len());
    }

    #[test]
    fn upgrade_takes_the_lock_after_the_check() {
        // Source-level guard: the TUI has no test seam for the network check.
        let src = include_str!("tui.rs");
        let body = &src[src.find("async fn upgrade_pkg(").unwrap()..];
        let body = &body[..body.find("\nfn ").unwrap_or(body.len())];
        let check = body.find("plan_upgrade(").unwrap();
        let lock = body.find("try_acquire(").unwrap();
        let apply = body.find("apply_planned_upgrade(").unwrap();
        assert!(check < lock && lock < apply);
        let mutating = src.find("let mutating = matches!").unwrap();
        let line = &src[mutating..][..src[mutating..].find(';').unwrap()];
        assert!(!line.contains("'u'"), "event_loop must not lock for `u`");
    }

    #[test]
    fn kind_label_matches_kind() {
        assert_eq!(kind_label(Kind::Cli), "cli");
        assert_eq!(kind_label(Kind::App), "app");
    }

    #[test]
    fn kind_filter_cycles_and_hides_other_kinds() {
        let mut filter = KindFilter::All;
        filter = cycle_kind_filter(filter);
        assert_eq!(filter, KindFilter::App);
        assert!(kind_allowed(filter, Kind::App));
        assert!(!kind_allowed(filter, Kind::Cli));
        for _ in 0..5 {
            filter = cycle_kind_filter(filter);
        }
        assert_eq!(filter, KindFilter::All);
        assert!(kind_allowed(filter, Kind::Library));
    }

    #[test]
    fn search_preview_ranks_exact_name_first() {
        let packages = vec![
            Package {
                name: "other".into(),
                version: "1".into(),
                description: "a jq clone".into(),
                homepage: String::new(),
                backend: "custom".into(),
                tags: vec![],
            },
            Package {
                name: "jq".into(),
                version: "1".into(),
                description: "command".into(),
                homepage: String::new(),
                backend: "brew".into(),
                tags: vec![],
            },
            Package {
                name: "jq-front".into(),
                version: "1".into(),
                description: "prefix".into(),
                homepage: String::new(),
                backend: "aqua".into(),
                tags: vec![],
            },
        ];
        let shown = filter_search("jq", &packages);
        let names: Vec<_> = shown.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["jq", "jq-front", "other"]);
    }

    #[test]
    fn pin_toggle_inserts_then_removes_ignoring_case() {
        let pins = BTreeMap::new();
        let (pins, message) = toggle_pin(&pins, "jq", "1.7");
        assert_eq!(message, "pinned jq at 1.7");
        assert_eq!(pins.get("jq").map(String::as_str), Some("1.7"));
        let (pins, message) = toggle_pin(&pins, "JQ", "1.8");
        assert_eq!(message, "unpinned JQ");
        assert!(pins.is_empty());
    }

    #[test]
    fn help_and_confirm_consume_quit() {
        assert!(!overlay_consumes_quit(Overlay::None));
        assert!(overlay_consumes_quit(Overlay::Help));
        assert!(overlay_consumes_quit(Overlay::ConfirmRemove(0)));
        assert!(overlay_consumes_quit(Overlay::ConfirmRestore(1)));
    }

    #[test]
    fn snapshot_detail_uses_shipped_diff() {
        let older = snapshot::SnapshotManifest {
            id: "old".into(),
            created_at: String::new(),
            reason: String::new(),
            prefix: std::path::PathBuf::from("/prefix"),
            packages: vec![snapshot::SnapshotPackage {
                name: "jq".into(),
                version: "1.7".into(),
                backend: "brew".into(),
                cellar: std::path::PathBuf::from("/cellar/jq"),
            }],
            bin_links: vec![],
            ledger: serde_json::json!({}),
            kegs: None,
            selected: None,
        };
        let mut newer = older.clone();
        newer.id = "new".into();
        newer.packages[0].version = "1.8".into();
        newer.packages.push(snapshot::SnapshotPackage {
            name: "rg".into(),
            version: "14".into(),
            backend: "github".into(),
            cellar: std::path::PathBuf::from("/cellar/rg"),
        });
        let text = snapshot_change_summary(&older, &newer);
        assert!(text.contains("jq 1.7->1.8"), "{text}");
        assert!(text.contains("rg"), "{text}");
    }
}
