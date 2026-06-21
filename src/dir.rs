// Directory browser: a fast, two-pane file navigator. Left pane is the entry
// list for the current directory; right pane previews the selection (child
// listing for folders, head of the file for text, dimensions for images,
// metadata otherwise). Enter opens a file in its viewer and returns here.

use crate::media::ImagePane;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use image::DynamicImage;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

// Palette, shared in spirit with the document viewer.
const DIR_C: Color = Color::Rgb(96, 165, 250);
const IMG_C: Color = Color::Rgb(196, 160, 250);
const VID_C: Color = Color::Rgb(244, 114, 182);
const PDF_C: Color = Color::Rgb(248, 113, 113);
const SHEET_C: Color = Color::Rgb(74, 222, 128);
const DOC_C: Color = Color::Rgb(252, 211, 77);
const CODE_C: Color = Color::Rgb(134, 239, 172);
const ARC_C: Color = Color::Rgb(251, 146, 60);
const OTHER_C: Color = Color::Rgb(205, 205, 215);
const DIM_C: Color = Color::Rgb(120, 120, 132);
const ACCENT: Color = Color::Rgb(125, 211, 252);

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Dir,
    Markdown,
    Sheet,
    Image,
    Pdf,
    Video,
    Doc,
    Code,
    Archive,
    Audio,
    Other,
}

impl Kind {
    fn color(self) -> Color {
        match self {
            Kind::Dir => DIR_C,
            Kind::Image => IMG_C,
            Kind::Video | Kind::Audio => VID_C,
            Kind::Pdf => PDF_C,
            Kind::Sheet => SHEET_C,
            Kind::Markdown | Kind::Doc => DOC_C,
            Kind::Code => CODE_C,
            Kind::Archive => ARC_C,
            Kind::Other => OTHER_C,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Dir => "Directory",
            Kind::Markdown => "Markdown",
            Kind::Sheet => "Spreadsheet",
            Kind::Image => "Image",
            Kind::Pdf => "PDF",
            Kind::Video => "Video",
            Kind::Doc => "Document",
            Kind::Code => "Source",
            Kind::Archive => "Archive",
            Kind::Audio => "Audio",
            Kind::Other => "File",
        }
    }

    /// Single-column glyph; degrades to plain ASCII-safe Unicode.
    fn glyph(self) -> &'static str {
        match self {
            Kind::Dir => "▸",
            Kind::Image => "▦",
            Kind::Video => "▶",
            Kind::Audio => "♪",
            Kind::Pdf => "▤",
            Kind::Sheet => "▤",
            Kind::Markdown | Kind::Doc => "▢",
            Kind::Code => "◇",
            Kind::Archive => "▣",
            Kind::Other => "·",
        }
    }
}

struct Entry {
    name: String,
    path: PathBuf,
    kind: Kind,
    size: u64,
    modified: Option<SystemTime>,
}

enum Mode {
    Browse,
    Filter,
}

/// What the preview pane is currently showing.
enum Pv {
    Text,  // styled lines in `preview`
    Image, // pixels in `pane`, caption in `caption`
}

struct App {
    cwd: PathBuf,
    all: Vec<Entry>,
    view: Vec<usize>, // indices into `all` matching the filter
    state: ListState,
    filter: String,
    mode: Mode,
    show_hidden: bool,
    viewport_h: u16,
    status: Option<String>,
    preview: Vec<Line<'static>>,
    preview_for: Option<PathBuf>,
    pv: Pv,
    caption: String,
    pane: Option<ImagePane>,
    img_cache: Vec<(PathBuf, DynamicImage)>,
}

enum Action {
    Quit,
    Open(PathBuf),
}

pub fn run(start: String) -> io::Result<()> {
    let cwd = fs::canonicalize(&start).unwrap_or_else(|_| PathBuf::from(&start));
    // Probe the graphics protocol once, before any alternate screen. If the
    // terminal can't do pixels, previews fall back to text/metadata.
    let pane = ImagePane::new().ok();
    let mut app = App {
        cwd,
        all: Vec::new(),
        view: Vec::new(),
        state: ListState::default(),
        filter: String::new(),
        mode: Mode::Browse,
        show_hidden: false,
        viewport_h: 0,
        status: None,
        preview: Vec::new(),
        preview_for: None,
        pv: Pv::Text,
        caption: String::new(),
        pane,
        img_cache: Vec::new(),
    };
    app.load();

    loop {
        let mut term = ratatui::init();
        let action = app.main_loop(&mut term);
        ratatui::restore();
        match action {
            Ok(Action::Quit) => return Ok(()),
            Ok(Action::Open(path)) => {
                crate::open_interactive(&path.to_string_lossy());
                app.preview_for = None; // force a redraw-time recompute
            }
            Err(e) => return Err(e),
        }
    }
}

impl App {
    /// Read the current directory into `all`, then apply the filter.
    fn load(&mut self) {
        self.all.clear();
        if let Ok(rd) = fs::read_dir(&self.cwd) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().into_owned();
                let path = ent.path();
                let meta = ent.metadata().ok();
                let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let kind = if is_dir { Kind::Dir } else { classify(&path) };
                self.all.push(Entry {
                    name,
                    path,
                    kind,
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    modified: meta.and_then(|m| m.modified().ok()),
                });
            }
        }
        // Directories first, then case-insensitive by name.
        self.all.sort_by(|a, b| {
            let ad = a.kind == Kind::Dir;
            let bd = b.kind == Kind::Dir;
            bd.cmp(&ad)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        self.refilter();
    }

    /// Rebuild `view` from `all` honoring hidden + fuzzy filter.
    fn refilter(&mut self) {
        let q = self.filter.to_lowercase();
        self.view = self
            .all
            .iter()
            .enumerate()
            .filter(|(_, e)| self.show_hidden || !e.name.starts_with('.'))
            .filter(|(_, e)| q.is_empty() || subsequence(&q, &e.name.to_lowercase()))
            .map(|(i, _)| i)
            .collect();
        let sel = if self.view.is_empty() {
            None
        } else {
            Some(self.state.selected().unwrap_or(0).min(self.view.len() - 1))
        };
        self.state.select(sel);
        self.preview_for = None;
    }

    fn selected(&self) -> Option<&Entry> {
        let i = self.state.selected()?;
        self.all.get(*self.view.get(i)?)
    }

    fn move_sel(&mut self, delta: isize) {
        if self.view.is_empty() {
            return;
        }
        let n = self.view.len() as isize;
        let cur = self.state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, n - 1);
        self.state.select(Some(next as usize));
    }

    fn enter_dir(&mut self, path: PathBuf) {
        self.cwd = path;
        self.filter.clear();
        self.mode = Mode::Browse;
        self.state.select(Some(0));
        self.status = None;
        self.load();
    }

    fn go_parent(&mut self) {
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            let from = self
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
            self.enter_dir(parent);
            // Land on the directory we came out of.
            if let Some(name) = from {
                if let Some(pos) = self.view.iter().position(|&i| self.all[i].name == name) {
                    self.state.select(Some(pos));
                }
            }
        }
    }

    fn activate(&mut self) -> Option<Action> {
        let e = self.selected()?;
        if e.kind == Kind::Dir {
            let p = e.path.clone();
            self.enter_dir(p);
            None
        } else {
            Some(Action::Open(e.path.clone()))
        }
    }

    fn main_loop(&mut self, term: &mut DefaultTerminal) -> io::Result<Action> {
        let mut dirty = true;
        loop {
            // Recompute the preview when the selection changed.
            let cur = self.selected().map(|e| e.path.clone());
            if cur != self.preview_for {
                self.build_preview();
                self.preview_for = cur;
                dirty = true;
            }
            if dirty {
                term.draw(|f| self.render(f))?;
                dirty = false;
            }
            if event::poll(Duration::from_millis(1000))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        dirty = true;
                        if let Some(action) = self.handle_key(key.code) {
                            return Ok(action);
                        }
                    }
                    Event::Resize(..) => dirty = true,
                    _ => {}
                }
            }
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> Option<Action> {
        if let Mode::Filter = self.mode {
            match code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.mode = Mode::Browse;
                    self.refilter();
                }
                KeyCode::Enter => self.mode = Mode::Browse,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.refilter();
                }
                KeyCode::Down => self.move_sel(1),
                KeyCode::Up => self.move_sel(-1),
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.refilter();
                }
                _ => {}
            }
            return None;
        }

        let half = (self.viewport_h / 2).max(1) as isize;
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return Some(Action::Quit),
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('d') | KeyCode::PageDown => self.move_sel(half),
            KeyCode::Char('u') | KeyCode::PageUp => self.move_sel(-half),
            KeyCode::Char('g') | KeyCode::Home => self.state.select(Some(0)),
            KeyCode::Char('G') | KeyCode::End => {
                if !self.view.is_empty() {
                    self.state.select(Some(self.view.len() - 1));
                }
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => return self.activate(),
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => self.go_parent(),
            KeyCode::Char('/') => {
                self.mode = Mode::Filter;
                self.filter.clear();
                self.refilter();
            }
            KeyCode::Char('.') => {
                self.show_hidden = !self.show_hidden;
                self.refilter();
            }
            _ => {}
        }
        None
    }

    /// Decode-with-cache for pixel previews; None if it fails.
    fn cached_image(
        &mut self,
        path: &Path,
        make: impl FnOnce() -> Result<DynamicImage, String>,
    ) -> Option<DynamicImage> {
        if let Some((_, img)) = self.img_cache.iter().find(|(p, _)| p == path) {
            return Some(img.clone());
        }
        match make() {
            Ok(img) => {
                self.img_cache.push((path.to_path_buf(), img.clone()));
                if self.img_cache.len() > 8 {
                    self.img_cache.remove(0);
                }
                Some(img)
            }
            Err(_) => None,
        }
    }

    /// Try to show `path` as pixels in the preview pane. Returns false if the
    /// terminal has no graphics or the decode failed (caller shows text).
    fn show_pixels(&mut self, path: &Path, kind: Kind, caption: String) -> bool {
        if self.pane.is_none() {
            return false;
        }
        let p = path.to_path_buf();
        let img = match kind {
            Kind::Image => self.cached_image(path, || {
                image::ImageReader::open(&p)
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.decode().map_err(|e| e.to_string()))
            }),
            Kind::Pdf => {
                let s = p.to_string_lossy().into_owned();
                self.cached_image(path, || crate::pdf::poster(&s))
            }
            Kind::Video => {
                let s = p.to_string_lossy().into_owned();
                self.cached_image(path, || crate::video::poster(&s))
            }
            _ => None,
        };
        match (img, self.pane.as_mut()) {
            (Some(img), Some(pane)) => {
                pane.set(img);
                self.pv = Pv::Image;
                self.caption = caption;
                true
            }
            _ => false,
        }
    }

    fn build_preview(&mut self) {
        self.preview.clear();
        self.pv = Pv::Text;
        let Some(e) = self.selected() else { return };
        // Snapshot what we need so `self` is free to mutate below.
        let name = e.name.clone();
        let kind = e.kind;
        let size = e.size;
        let modified = e.modified;
        let path = e.path.clone();

        // Caption / header: name, type · size · modified.
        let mut meta = kind.label().to_string();
        if kind != Kind::Dir {
            meta.push_str(&format!("  ·  {}", human_size(size)));
        }
        if let Some(m) = modified {
            meta.push_str(&format!("  ·  {}", rel_time(m)));
        }

        // Pixel previews first — fall through to text on failure.
        match kind {
            Kind::Image | Kind::Pdf | Kind::Video => {
                let extra = match kind {
                    Kind::Image => image::image_dimensions(&path)
                        .map(|(w, h)| format!("  ·  {w}×{h}"))
                        .unwrap_or_default(),
                    Kind::Video => "  ·  Enter to play".into(),
                    Kind::Pdf => "  ·  page 1".into(),
                    _ => String::new(),
                };
                if self.show_pixels(&path, kind, format!("{name}   {meta}{extra}")) {
                    return;
                }
            }
            _ => {}
        }

        // Text header.
        self.preview.push(Line::from(Span::styled(
            name,
            Style::default()
                .fg(kind.color())
                .add_modifier(Modifier::BOLD),
        )));
        self.preview
            .push(Line::from(Span::styled(meta, Style::default().fg(DIM_C))));
        self.preview.push(Line::from(""));

        match kind {
            Kind::Dir => self.preview_dir(&path),
            Kind::Markdown => self.preview_markdown(read_capped(&path)),
            Kind::Doc => {
                // .docx → markdown; other docs fall back to metadata.
                match crate::docx::to_markdown(&path.to_string_lossy()) {
                    Ok(src) => self.preview_markdown(src),
                    Err(_) => self.preview.push(no_preview()),
                }
            }
            Kind::Sheet => self.preview_text_head(&path, OTHER_C),
            _ => self.preview_text_head(&path, OTHER_C),
        }
    }

    fn preview_dir(&mut self, path: &Path) {
        let mut kids: Vec<(String, bool)> = match fs::read_dir(path) {
            Ok(rd) => rd
                .flatten()
                .map(|c| {
                    let n = c.file_name().to_string_lossy().into_owned();
                    (n, c.path().is_dir())
                })
                .filter(|(n, _)| self.show_hidden || !n.starts_with('.'))
                .collect(),
            Err(_) => {
                self.preview.push(no_preview());
                return;
            }
        };
        kids.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });
        let total = kids.len();
        if total == 0 {
            self.preview.push(Line::from(Span::styled(
                "empty",
                Style::default().fg(DIM_C),
            )));
            return;
        }
        self.preview.insert(
            2,
            Line::from(Span::styled(
                format!("{total} items"),
                Style::default().fg(DIM_C),
            )),
        );
        for (n, d) in kids.into_iter().take(300) {
            let (c, suffix) = if d { (DIR_C, "/") } else { (OTHER_C, "") };
            self.preview.push(Line::from(Span::styled(
                format!("{n}{suffix}"),
                Style::default().fg(c),
            )));
        }
    }

    fn preview_markdown(&mut self, src: String) {
        let width = preview_text_width();
        let (lines, _, _) = crate::markdown::Rendered::build(&src).layout(width);
        self.preview.extend(lines.into_iter().take(600));
    }

    fn preview_text_head(&mut self, path: &Path, color: Color) {
        if let Some(text) = head_text(path, 64 * 1024, 500) {
            for l in text.lines() {
                self.preview.push(Line::from(Span::styled(
                    l.to_string(),
                    Style::default().fg(color),
                )));
            }
        } else {
            self.preview.push(no_preview());
        }
    }

    fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        let rows = Layout::default()
            .constraints([
                Constraint::Length(1), // breadcrumb
                Constraint::Min(0),    // body
                Constraint::Length(1), // status
            ])
            .split(area);

        self.render_crumb(f, rows[0]);

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(rows[1]);
        self.render_list(f, cols[0]);
        self.render_preview(f, cols[1]);

        self.render_status(f, rows[2]);
    }

    fn render_crumb(&self, f: &mut Frame, area: Rect) {
        let shown = pretty_path(&self.cwd);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(
                    shown,
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
            ])),
            area,
        );
    }

    fn render_list(&mut self, f: &mut Frame, area: Rect) {
        self.viewport_h = area.height.saturating_sub(2);
        let inner_w = area.width.saturating_sub(4) as usize; // borders + glyph
        let size_w = 8usize;
        let name_w = inner_w.saturating_sub(size_w + 1).max(4);

        let items: Vec<ListItem> = self
            .view
            .iter()
            .map(|&i| {
                let e = &self.all[i];
                let name = truncate(&e.name, name_w);
                let size = if e.kind == Kind::Dir {
                    String::new()
                } else {
                    human_size(e.size)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{} ", e.kind.glyph()),
                        Style::default().fg(e.kind.color()),
                    ),
                    Span::styled(
                        format!("{name:<name_w$}"),
                        Style::default().fg(e.kind.color()),
                    ),
                    Span::styled(format!(" {size:>size_w$}"), Style::default().fg(DIM_C)),
                ]))
            })
            .collect();

        let count = self.view.len();
        let title = format!(" {count} ");
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED));
        f.render_stateful_widget(list, area, &mut self.state);
    }

    fn render_preview(&mut self, f: &mut Frame, area: Rect) {
        match self.pv {
            Pv::Image => {
                let title = format!(
                    " {} ",
                    truncate(&self.caption, area.width.saturating_sub(4) as usize)
                );
                let block = Block::default().borders(Borders::ALL).title(title);
                let inner = block.inner(area);
                f.render_widget(block, area);
                if let Some(pane) = self.pane.as_mut() {
                    pane.render(f, inner);
                }
            }
            Pv::Text => {
                let block = Block::default().borders(Borders::ALL).title(" Preview ");
                let inner_h = area.height.saturating_sub(2) as usize;
                let text: Vec<Line> = self.preview.iter().take(inner_h).cloned().collect();
                f.render_widget(Paragraph::new(Text::from(text)).block(block), area);
            }
        }
    }

    fn render_status(&self, f: &mut Frame, area: Rect) {
        let txt = if let Mode::Filter = self.mode {
            format!(" /{}    [Enter] keep  [Esc] clear", self.filter)
        } else if let Some(s) = &self.status {
            format!(" {s}")
        } else {
            let hidden = if self.show_hidden { "shown" } else { "hidden" };
            format!(
                " [j/k] move  [Enter/l] open  [h] up  [/] filter  [.] dotfiles ({hidden})  [q] quit"
            )
        };
        let color = if let Mode::Filter = self.mode {
            Color::Rgb(252, 211, 77)
        } else {
            DIM_C
        };
        f.render_widget(
            Paragraph::new(Line::from(txt)).style(Style::default().fg(color)),
            area,
        );
    }
}

/// Plain newline-separated listing for non-interactive use (piped output).
pub fn dump(path: &str) -> String {
    let mut names: Vec<String> = match fs::read_dir(path) {
        Ok(rd) => rd
            .flatten()
            .map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() {
                    format!("{n}/")
                } else {
                    n
                }
            })
            .collect(),
        Err(e) => return format!("vellum: {path}: {e}\n"),
    };
    names.sort_by_key(|n| n.to_lowercase());
    let mut out = names.join("\n");
    out.push('\n');
    out
}

/// Classify a non-directory path by extension for coloring + preview.
fn classify(path: &Path) -> Kind {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "md" | "markdown" | "mdx" => Kind::Markdown,
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" | "csv" | "tsv" => Kind::Sheet,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "tif" | "ico" | "svg" => {
            Kind::Image
        }
        "pdf" => Kind::Pdf,
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => Kind::Video,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" => Kind::Audio,
        "docx" | "doc" | "rtf" | "odt" | "pptx" | "ppt" => Kind::Doc,
        "zip" | "gz" | "tar" | "tgz" | "bz2" | "xz" | "7z" | "rar" | "zst" => Kind::Archive,
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cc" | "cpp" | "hpp" | "go"
        | "java" | "rb" | "sh" | "bash" | "zsh" | "fish" | "toml" | "json" | "yaml" | "yml"
        | "html" | "htm" | "css" | "scss" | "lua" | "sql" | "php" | "swift" | "kt" | "ex"
        | "exs" | "ml" | "hs" | "clj" | "vim" | "ini" | "cfg" | "conf" | "xml" => Kind::Code,
        _ => Kind::Other,
    }
}

/// Wrap width for rendered-markdown previews, from the terminal size.
fn preview_text_width() -> usize {
    let cols = crossterm::terminal::size().map(|(c, _)| c).unwrap_or(80);
    ((cols as usize * 58 / 100).saturating_sub(2)).max(20)
}

/// Read up to `max_bytes` of a file as a lossy String (for markdown source).
fn read_capped(path: &Path) -> String {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let mut buf = vec![0u8; 256 * 1024];
    let n = f.read(&mut buf).unwrap_or(0);
    buf.truncate(n);
    String::from_utf8_lossy(&buf).into_owned()
}

fn no_preview() -> Line<'static> {
    Line::from(Span::styled("No preview", Style::default().fg(DIM_C)))
}

/// Read the head of a file as text, or None if it looks binary / unreadable.
fn head_text(path: &Path, max_bytes: usize, max_lines: usize) -> Option<String> {
    let mut f = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; max_bytes];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    if buf.contains(&0) {
        return None; // NUL byte → binary
    }
    let s = String::from_utf8_lossy(&buf);
    Some(s.lines().take(max_lines).collect::<Vec<_>>().join("\n"))
}

/// Is `needle` a subsequence of `haystack`? (cheap fuzzy match)
fn subsequence(needle: &str, haystack: &str) -> bool {
    let mut h = haystack.chars();
    needle.chars().all(|nc| h.any(|hc| hc == nc))
}

fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < 4 {
        f /= 1024.0;
        i += 1;
    }
    format!("{f:.1}{}", U[i])
}

fn rel_time(t: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 86_400 * 30 => format!("{}d ago", s / 86_400),
        s if s < 86_400 * 365 => format!("{}mo ago", s / (86_400 * 30)),
        s => format!("{}y ago", s / (86_400 * 365)),
    }
}

/// Home-relative, `~`-prefixed path for the breadcrumb.
fn pretty_path(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if let Some(rest) = s.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    s
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}
