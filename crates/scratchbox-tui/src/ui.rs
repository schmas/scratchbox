//! Rendering. Reads app state, never changes it.

use edtui::{EditorTheme, EditorView};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};

use scratchbox_core::{Format, WorkspaceHealth};

use crate::app::{App, Conflict, Focus, Help};
use crate::keys::{self, Chord, Command, HelpSection};
use crate::syntax;

/// Wide enough for a timestamped name plus its format tag, narrow enough to leave the
/// editor the room that matters.
const LIST_WIDTH: u16 = 34;

/// The smallest body the keybindings panel will draw into.
///
/// Below this it would be all border and no list, so nothing is drawn at all. Public because
/// the test that renders at the edge of it has to ask rather than restate — the two drifting
/// apart is how a panel ends up drawn at a size it cannot fit.
pub const HELP_MIN_BODY_W: u16 = 21;
pub const HELP_MIN_BODY_H: u16 = 6;

/// The panel's preferred floor, never applied past the frame's own width.
const HELP_MIN_W: u16 = 30;
/// A ceiling, so the list does not stretch into unreadable lines on a wide terminal.
const HELP_MAX_W: u16 = 90;
/// The share of the frame it prefers, so it reads as a panel over the panes.
const HELP_PCT: u16 = 70;
/// Columns left uncovered each side, so the panes stay visible around it.
const HELP_MARGIN: u16 = 4;
/// Between the left border and the caption column.
const HELP_GUTTER: u16 = 3;
/// Between the caption column and the descriptions.
const HELP_KEY_GAP: u16 = 2;
/// A blank column before the right border.
const HELP_PAD_R: u16 = 1;

pub fn render(frame: &mut Frame, app: &mut App) {
    let [body, status] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    let [list, editor] =
        Layout::horizontal([Constraint::Length(LIST_WIDTH), Constraint::Min(20)]).areas(body);

    render_list(frame, app, list);
    render_editor(frame, app, editor);
    render_status(frame, app, status);

    // One modal per frame, in the order that owns the keyboard. An `if / else if` rather than
    // three `if`s: the last thing written to a cell wins, so two modals composed together
    // would put the one that does *not* own the keyboard on top.
    if app.pending_delete().is_some() {
        render_delete_prompt(frame, app, body);
    } else if let Some(conflict) = app.conflict() {
        render_conflict_prompt(frame, app, conflict, body);
    } else if let Some(help) = app.help() {
        render_help(frame, help, &keys::help_sections(), body);
    }
}

fn render_list(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .notes()
        .iter()
        .map(|note| {
            ListItem::new(Line::from(vec![
                Span::styled(format_tag(note.format), Style::new().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::raw(note.id.as_str().to_owned()),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(pane_block("Notes", app.focus() == Focus::List))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));

    let mut state = ListState::default();
    state.select(app.selected_index());
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_editor(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus() == Focus::Editor;
    let title = app
        .selected()
        .map(|id| id.as_str().to_owned())
        .unwrap_or_else(|| "no note".to_owned());

    // From the note in the buffer rather than the selection, so the two cannot disagree —
    // and derived per frame rather than stored, which is what makes the D10 rename a no-op
    // here: it leaves the extension alone, so the syntax comes out the same.
    let format = app
        .editor()
        .loaded()
        .map_or(Format::PlainText, |id| Format::from_name(id.as_str()));

    let block = pane_block(&title, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // A scratchpad, not an IDE: no line numbers, and long lines wrap rather than scroll
    // sideways.
    let theme = EditorTheme::default()
        .base(Style::default())
        .hide_status_line()
        .block(Block::default());
    frame.render_widget(
        EditorView::new(app.editor_mut().state_mut())
            .theme(theme)
            .syntax_highlighter(Some(syntax::highlighter(format))),
        inner,
    );
}

/// The status line, in priority order.
///
/// A conflict outranks everything: it is a question, and the app is not accepting anything
/// else until it is answered. An unavailable workspace outranks the ordinary status because
/// it changes what every subsequent keystroke means.
///
/// The keybindings panel's hints come last of all, below both. This is the one line that
/// carries a refused quit and the in-memory-only warning, and a panel that replaced it would
/// hide the message a user needs before pressing quit a second time.
fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let (text, color) = match (app.conflict(), app.health()) {
        (Some(_), _) => (
            "external change — [k]eep mine · [t]ake theirs · ^Q quit".to_owned(),
            Color::Yellow,
        ),
        (None, WorkspaceHealth::Missing | WorkspaceHealth::ReadOnly) => (
            "workspace unavailable — edits are in memory only".to_owned(),
            Color::Yellow,
        ),
        (None, WorkspaceHealth::Ok) => (
            match (app.status(), app.help()) {
                (Some(status), _) => status.to_owned(),
                (None, Some(help)) if help.searching() => filter_prompt(),
                (None, Some(_)) => help_hints(),
                (None, None) => status_hints(),
            },
            Color::DarkGray,
        ),
    };
    frame.render_widget(Paragraph::new(text).style(Style::new().fg(color)), area);
}

/// What the status line says while the panel is open.
///
/// Led by the way out, because this line truncates from the right and being unable to leave a
/// modal is the worst thing it could fail to say. `^Q` comes from the keymap; the panel's own
/// four keys are the ones its `This panel` section lists.
fn help_hints() -> String {
    let quit = hint_key(Command::Quit).unwrap_or_default();
    format!("esc close   ↑/↓ select   ⏎ run   / search   {quit} quit")
}

/// The status line while the filter prompt is up.
///
/// The query itself is drawn on the panel's bottom border, not here: this line can be holding
/// a message about the note, and the text a user is typing must not depend on that being empty.
///
/// No quit hint: inside the prompt `q` is a character. The ways out are the two named here,
/// plus `^Q`, which stays live everywhere.
fn filter_prompt() -> String {
    "⏎ done   esc clear".to_owned()
}

/// One status-line hint: a command, or two commands that share a label.
enum Hint {
    One(Command, &'static str),
    /// Printed with the shared caption prefix once — `alt-↑/↓`, not `alt-↑ alt-↓`.
    Pair(Command, Command, &'static str),
}

/// What the status line advertises, in the order it prints.
///
/// Not every binding: the plain arrows are left out, because a line this narrow is better
/// spent on the keys a user cannot guess.
const HINTS: &[Hint] = &[
    Hint::One(Command::NewNote, "new"),
    Hint::One(Command::RequestDelete, "delete"),
    Hint::Pair(Command::MoveNoteUp, Command::MoveNoteDown, "reorder"),
    Hint::One(Command::ToggleFocus, "switch pane"),
    Hint::One(Command::OpenHelp, "help"),
    Hint::One(Command::Quit, "quit"),
];

/// The default status line's key hints, taken from the keymap.
///
/// Deliberately not the panel's prose. This line abbreviates (`new`, not `new note`), merges
/// the two reorder bindings into one hint, prints only the first chord of a binding that has
/// two, and omits the plain arrows — four transforms, past the point where one string can
/// serve both widths. The keys and the commands still come from the table, so a rebind moves
/// this line with it.
fn status_hints() -> String {
    HINTS
        .iter()
        .filter_map(hint_text)
        .collect::<Vec<_>>()
        .join("   ")
}

fn hint_text(hint: &Hint) -> Option<String> {
    match hint {
        Hint::One(command, label) => Some(format!("{} {label}", hint_key(*command)?)),
        Hint::Pair(first, second, label) => {
            let keys = merge_captions(&hint_key(*first)?, &hint_key(*second)?);
            Some(format!("{keys} {label}"))
        }
    }
}

/// How the first chord of `command`'s binding is printed.
///
/// `None` for a command the keymap declares no chord for, which cannot be advertised as a
/// key. The test asserting this line byte for byte is what keeps that from passing quietly.
fn hint_key(command: Command) -> Option<String> {
    keys::binding(command)?.chords.first().map(Chord::caption)
}

/// `alt-↑` and `alt-↓` as `alt-↑/↓`: the shared prefix printed once, because two full
/// captions side by side read as two unrelated keys rather than one pair.
fn merge_captions(first: &str, second: &str) -> String {
    let shared: usize = first
        .chars()
        .zip(second.chars())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum();
    format!("{first}/{}", &second[shared..])
}

fn render_delete_prompt(frame: &mut Frame, app: &App, area: Rect) {
    let Some(id) = app.pending_delete() else {
        return;
    };

    let prompt = Paragraph::new(vec![
        Line::from(format!("Move {id} to the trash?")),
        Line::from(""),
        Line::from(Span::styled(
            "y to confirm, any other key to cancel",
            Style::new().fg(Color::DarkGray),
        )),
    ])
    .wrap(Wrap { trim: true })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Confirm ")
            .border_style(Style::new().fg(Color::Yellow)),
    );

    let area = centered(area, 60, 5);
    frame.render_widget(Clear, area);
    frame.render_widget(prompt, area);
}

/// The external-change prompt.
///
/// On screen as a panel rather than only in the status line because the editor stops
/// accepting keys while it is up. A user who typed and saw nothing happen needs to be told
/// why in the place they are already looking.
fn render_conflict_prompt(frame: &mut Frame, app: &App, conflict: Conflict, area: Rect) {
    let name = app
        .editor()
        .loaded()
        .map(|id| id.as_str().to_owned())
        .unwrap_or_else(|| "this note".to_owned());

    let what = match conflict {
        Conflict::Changed => format!("{name} changed on disk while you had unsaved edits."),
        Conflict::Deleted => format!("{name} was deleted while you had unsaved edits."),
    };
    let keep = match conflict {
        Conflict::Changed => "k  keep mine (write my buffer over theirs)",
        Conflict::Deleted => "k  keep mine (write my buffer back to disk)",
    };
    let take = match conflict {
        Conflict::Changed => "t  take theirs (discard my edits)",
        Conflict::Deleted => "t  take theirs (let the note go)",
    };

    let prompt = Paragraph::new(vec![
        Line::from(what),
        Line::from(""),
        Line::from(keep),
        Line::from(take),
        Line::from(Span::styled(
            "^Q  quit without saving",
            Style::new().fg(Color::DarkGray),
        )),
    ])
    .wrap(Wrap { trim: true })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" External change ")
            .border_style(Style::new().fg(Color::Yellow)),
    );

    let area = centered(area, 62, 8);
    frame.render_widget(Clear, area);
    frame.render_widget(prompt, area);
}

/// The keybindings panel: a floating box over both panes.
///
/// Takes the panel's state and its rows rather than the whole app, which is what makes it
/// obvious that nothing here writes any of it: `render` holds a `&mut App` for the editor
/// widget, so a `&App` parameter would prove nothing.
fn render_help(frame: &mut Frame, help: &Help, sections: &[HelpSection], area: Rect) {
    // First, and in the same rect the formulas below use: a panel drawn into a body this
    // small would be border with nothing inside it.
    if area.width < HELP_MIN_BODY_W || area.height < HELP_MIN_BODY_H {
        return;
    }

    // Measured from the whole list, always. A box that resized on every keystroke while the
    // user typed a filter would be unreadable — so the geometry comes from `sections` and only
    // the contents come from the filtered set below.
    let (caption_w, content_w) = help_metrics(sections);
    let rect = help_rect(content_w, line_count(sections), area);

    let shown = keys::filter(sections, help.query());
    let rows = keys::help_row_count(&shown);

    let inner_w = rect.width.saturating_sub(2);
    let visible = usize::from(rect.height.saturating_sub(2));
    let lines = if shown.is_empty() {
        vec![no_match_line(help.query(), caption_w, inner_w)]
    } else {
        help_body(&shown, caption_w, inner_w)
    };

    let offset = help_scroll_to(&lines, help.cursor(), help.offset(), visible);
    // Required, not defensive: the box is sized from the whole list, so a filtered body can
    // be far shorter than the window it is drawn into, and slicing past the end would panic
    // inside the draw closure — taking the process, and the unsaved buffer, with it.
    let end = (offset + visible).min(lines.len());

    let selected = lines
        .get(offset..end)
        .unwrap_or_default()
        .iter()
        .map(|line| line.to_line(help.cursor(), inner_w))
        .collect::<Vec<_>>();

    let readout = format!(
        " {} of {rows} ",
        if rows > 0 { help.cursor() + 1 } else { 0 }
    );
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Cyan))
        .title_top(" Keybindings ")
        .title_bottom(Line::from(readout).right_aligned());

    // On the panel's own border rather than in the status line, because the status line
    // belongs to messages about the note — and one of those left over from earlier in the
    // session would otherwise hide the only echo of what the user is typing.
    if help.searching() {
        block = block.title_bottom(Line::from(format!(" Filter: {}▊ ", help.query())));
    }

    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(selected).block(block), rect);

    if lines.len() > visible {
        // Inset by one row, so the track runs beside the body and leaves the corners to the
        // border — the bottom one carries the readout.
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None);
        let mut state = ScrollbarState::new(lines.len().saturating_sub(visible)).position(offset);
        frame.render_stateful_widget(scrollbar, rect.inner(Margin::new(0, 1)), &mut state);
    }
}

/// One line of the panel's body.
///
/// A section heading and the blank line after it carry no row, which is what lets the cursor
/// step over bindings while the view scrolls over lines.
struct HelpLine {
    text: String,
    /// Where the caption ends, so the description can be dimmed on its own.
    caption_end: usize,
    row: Option<usize>,
    /// A row `⏎` will not run. Dimmed, so doing nothing reads as an answer rather than a bug.
    dim: bool,
}

impl HelpLine {
    fn to_line(&self, cursor: usize, width: u16) -> Line<'static> {
        if self.row == Some(cursor) {
            // Built from unstyled text and reversed as one span: a span carrying its own
            // colour would end the bar in the middle of the row.
            let mut text = self.text.clone();
            let padding = usize::from(width).saturating_sub(text.chars().count());
            text.push_str(&" ".repeat(padding));
            return Line::from(Span::styled(
                text,
                Style::new().add_modifier(Modifier::REVERSED),
            ));
        }
        if self.row.is_none() {
            let color = if self.dim {
                Color::DarkGray
            } else {
                Color::Cyan
            };
            return Line::from(Span::styled(self.text.clone(), Style::new().fg(color)));
        }

        let muted = Style::new().fg(Color::DarkGray);
        let caption = self.text[..self.caption_end].to_owned();
        let desc = self.text[self.caption_end..].to_owned();
        Line::from(vec![
            Span::styled(caption, if self.dim { muted } else { Style::new() }),
            Span::styled(desc, muted),
        ])
    }
}

/// The caption column's width, and the width of the widest line the body will produce.
fn help_metrics(sections: &[HelpSection]) -> (u16, u16) {
    let caption_w = sections
        .iter()
        .flat_map(|section| &section.rows)
        .map(|row| width_of(row.caption))
        .max()
        .unwrap_or(0);
    let desc_w = sections
        .iter()
        .flat_map(|section| &section.rows)
        .map(|row| width_of(row.desc))
        .max()
        .unwrap_or(0);
    let title_w = sections
        .iter()
        .map(|section| width_of(section.title))
        .max()
        .unwrap_or(0);

    let column = HELP_GUTTER
        .saturating_add(caption_w)
        .saturating_add(HELP_KEY_GAP);
    // A heading is `── Title ` at the description column, so a long title can be the widest
    // thing in the box.
    let content_w = column
        .saturating_add(desc_w)
        .max(column.saturating_add(title_w).saturating_add(4));
    (caption_w, content_w)
}

/// The panel's box, centered on `area`.
///
/// Total for every `u16`: the panel is the one thing on screen that is sized by arithmetic
/// rather than by a layout, and an underflow here panics inside the draw closure.
pub fn help_rect(content_w: u16, body_len: u16, area: Rect) -> Rect {
    let mut rect = centered(
        area,
        help_width(content_w, area.width),
        help_height(body_len, area.height),
    );
    // A box has to be able to hold its own border. Nothing is drawn at a size like this —
    // the guard in `render_help` stops long before — but the arithmetic stays total on its
    // own terms rather than by relying on that guard.
    rect.width = rect.width.max(2);
    rect.height = rect.height.max(2);
    rect
}

fn help_width(content_w: u16, frame_w: u16) -> u16 {
    // Two borders and the blank column before the right one: the box must never come out
    // tighter than the text it holds.
    let share = u16::try_from(u32::from(frame_w) * u32::from(HELP_PCT) / 100).unwrap_or(u16::MAX);
    let preferred = content_w.saturating_add(2 + HELP_PAD_R).max(share);

    let mut limit = frame_w.min(HELP_MAX_W);
    // The margin is only reserved when the frame can spare it: on a narrow terminal the
    // panel is worth more than the sliver of pane beside it.
    if frame_w >= HELP_MIN_W + 2 * HELP_MARGIN {
        limit = limit.min(frame_w - 2 * HELP_MARGIN);
    }

    // The ceiling wins. A `clamp` would raise the floor to meet the ceiling when the two
    // invert, which on a twenty-column terminal hands back a thirty-wide panel.
    preferred.max(HELP_MIN_W.min(frame_w)).min(limit).max(2)
}

fn help_height(body_len: u16, frame_h: u16) -> u16 {
    // Two rows of frame left around it, and never fewer than one body row.
    let ceiling = frame_h.saturating_sub(2).max(3);
    body_len.saturating_add(2).min(ceiling).min(frame_h.max(2))
}

/// Lay the sections out: a heading per section, its rows, and a blank line between sections.
fn help_body(sections: &[HelpSection], caption_w: u16, inner_w: u16) -> Vec<HelpLine> {
    let column = usize::from(HELP_GUTTER + caption_w + HELP_KEY_GAP);
    let mut lines = Vec::new();
    let mut index = 0;

    for (position, section) in sections.iter().enumerate() {
        if position > 0 {
            lines.push(HelpLine {
                text: String::new(),
                caption_end: 0,
                row: None,
                dim: false,
            });
        }

        let heading = format!("{:column$}── {} ", "", section.title);
        let rule = usize::from(inner_w).saturating_sub(heading.chars().count());
        lines.push(HelpLine {
            text: format!("{heading}{}", "─".repeat(rule)),
            caption_end: 0,
            row: None,
            dim: false,
        });

        for row in &section.rows {
            let caption = format!(
                "{:gutter$}{:>caption_w$}",
                "",
                row.caption,
                gutter = usize::from(HELP_GUTTER),
                caption_w = usize::from(caption_w),
            );
            let caption_end = caption.len();
            let gap = " ".repeat(usize::from(HELP_KEY_GAP));
            lines.push(HelpLine {
                text: format!("{caption}{gap}{}", row.desc),
                caption_end,
                row: Some(index),
                dim: row.run.is_none(),
            });
            index += 1;
        }
    }
    lines
}

/// The body when the query matches nothing.
///
/// One line in a box still sized for twenty — which is exactly the case the bounded slice in
/// `render_help` exists for.
fn no_match_line(query: &str, caption_w: u16, inner_w: u16) -> HelpLine {
    let text = format!("no match for \"{query}\"");
    let column = usize::from(HELP_GUTTER + caption_w + HELP_KEY_GAP);
    // Lined up with the descriptions when the message fits there, and flush left when it does
    // not: the box is sized for the whole list, but a narrow terminal makes that indent the
    // difference between a legible message and one truncated to its first letter.
    let indent = if column + text.chars().count() <= usize::from(inner_w) {
        column
    } else {
        0
    };

    HelpLine {
        text: format!("{:indent$}{text}", ""),
        caption_end: 0,
        row: None,
        dim: true,
    }
}

/// The first visible line, chosen so the cursor's row is on screen.
///
/// Starts from the stored offset, so a view the user scrolled deliberately survives a
/// keystroke that did not need to move it. Scrolling up keeps walking back over the lines
/// that carry no row, which brings a section's heading into view with its first binding.
fn help_scroll_to(lines: &[HelpLine], cursor: usize, offset: usize, visible: usize) -> usize {
    let Some(at) = lines.iter().position(|line| line.row == Some(cursor)) else {
        return 0;
    };

    let mut offset = offset.min(lines.len().saturating_sub(1));
    if at < offset {
        offset = at;
        while offset > 0 && lines[offset - 1].row.is_none() {
            offset -= 1;
        }
    }
    if visible > 0 && at >= offset + visible {
        offset = at + 1 - visible;
    }
    offset.min(lines.len().saturating_sub(visible.max(1)))
}

fn line_count(sections: &[HelpSection]) -> u16 {
    u16::try_from(keys::help_line_count(sections)).unwrap_or(u16::MAX)
}

fn width_of(text: &str) -> u16 {
    u16::try_from(text.chars().count()).unwrap_or(u16::MAX)
}

fn pane_block(title: &str, focused: bool) -> Block<'static> {
    let border = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(format!(" {title} "))
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn format_tag(format: Format) -> &'static str {
    match format {
        Format::Markdown => "md  ",
        Format::Json => "json",
        Format::PlainText => "txt ",
        Format::Java => "java",
        Format::TypeScript => "ts  ",
        Format::JavaScript => "js  ",
        Format::Css => "css ",
        Format::Html => "html",
    }
}
