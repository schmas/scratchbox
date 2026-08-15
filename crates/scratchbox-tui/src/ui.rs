//! Rendering. Reads app state, never changes it.

use edtui::{EditorTheme, EditorView};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use scratchbox_core::{Format, WorkspaceHealth};

use crate::app::{App, Conflict, Focus};

/// Wide enough for a timestamped name plus its format tag, narrow enough to leave the
/// editor the room that matters.
const LIST_WIDTH: u16 = 34;

pub fn render(frame: &mut Frame, app: &mut App) {
    let [body, status] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());
    let [list, editor] =
        Layout::horizontal([Constraint::Length(LIST_WIDTH), Constraint::Min(20)]).areas(body);

    render_list(frame, app, list);
    render_editor(frame, app, editor);
    render_status(frame, app, status);

    if app.pending_delete().is_some() {
        render_delete_prompt(frame, app, body);
    } else if let Some(conflict) = app.conflict() {
        render_conflict_prompt(frame, app, conflict, body);
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
        EditorView::new(app.editor_mut().state_mut()).theme(theme),
        inner,
    );
}

/// The status line, in priority order.
///
/// A conflict outranks everything: it is a question, and the app is not accepting anything
/// else until it is answered. An unavailable workspace outranks the ordinary status because
/// it changes what every subsequent keystroke means.
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
            match app.status() {
                Some(status) => status.to_owned(),
                None => {
                    "^N new   ^D delete   alt-↑/↓ reorder   tab switch pane   ^Q quit".to_owned()
                }
            },
            Color::DarkGray,
        ),
    };
    frame.render_widget(Paragraph::new(text).style(Style::new().fg(color)), area);
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
