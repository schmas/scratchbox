//! Application state.
//!
//! Free of terminal types beyond the key events the editor consumes, so the whole of it can
//! be driven from a test with no TTY: rendering reads this state, it never owns it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::KeyEvent;
use scratchbox_core::note::NoteMeta;
use scratchbox_core::order::{self, OrderStore};
use scratchbox_core::{Format, NoteId, Result, Store, StoreEvent, WorkspaceHealth, reconcile};

use crate::editor::EditorPane;
use crate::{keys, save};

/// How long the buffer sits untouched before it is written.
///
/// Deliberately not the watcher's debounce. This one is about how soon the user's work is
/// safe; that one is about how much filesystem noise to swallow. A single shared number
/// would have to be wrong for one of them.
pub const IDLE_SAVE: Duration = Duration::from_millis(300);

/// How often an unavailable workspace is prodded to see whether it came back.
const HEALTH_RECHECK: Duration = Duration::from_secs(5);

/// What the terminal is assumed to be until it says otherwise.
///
/// `App::new` runs before there is a terminal to ask — and under `--bench-first-frame` there
/// never is one. The same size the off-screen tests use, so a headless run behaves like the
/// one they observe.
const ASSUMED_SIZE: (u16, u16) = (120, 40);

/// Which pane the keyboard is talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Editor,
}

/// An external change to the open note that only the user can decide about.
///
/// An enum rather than a flag, because the two cases resolve differently and because a
/// bool invites code that falls back out of the conflicted state by accident — which would
/// let autosave overwrite the very change this state exists to protect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// The note changed on disk while the buffer held unsaved work.
    Changed,
    /// The note was deleted on disk while the buffer held unsaved work.
    Deleted,
}

/// The keybindings panel's state while it is open.
///
/// An `Option<Help>` on `App` rather than a [`Focus`] variant: `Focus` means "which pane owns
/// the arrows", and this owns the whole keyboard — which is what the two existing prompts
/// model as an `Option` too.
#[derive(Default)]
pub struct Help {
    /// Index into the rows currently listed, not into the keymap: a filter changes what is
    /// listed, and the cursor addresses what the user can see.
    cursor: usize,
    /// The first visible body line. Lines and rows are different counts — a section heading
    /// is a line with no row — which is what lets the cursor step over bindings while the
    /// view scrolls over lines.
    ///
    /// Written by the handlers. Rendering reads it and clamps its own copy, never this one.
    offset: usize,
    /// The filter query, and whether keys are going into it rather than moving the cursor.
    query: String,
    searching: bool,
}

impl Help {
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn offset(&self) -> usize {
        self.offset
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn searching(&self) -> bool {
        self.searching
    }

    /// The sections the panel is currently listing.
    ///
    /// One walk, indexed by the cursor, the renderer, and `⏎` alike: two of them filtering
    /// separately would disagree the moment either was edited.
    pub fn listed(&self) -> Vec<keys::HelpSection> {
        keys::filter(&keys::help_sections(), &self.query)
    }

    /// Pull the cursor and the first visible line back inside the list.
    ///
    /// Both belong to the handlers — rendering only reads them. The window height used here
    /// is an upper bound: the frame, less its status line and the panel's two borders. The
    /// real window is never larger, so this can only hold the offset short of the end rather
    /// than leave it past one.
    fn clamp(&mut self, size: (u16, u16)) {
        let sections = self.listed();
        let last_row = keys::help_row_count(&sections).saturating_sub(1);
        let lines = keys::help_line_count(&sections);
        let window = usize::from(size.1).saturating_sub(3).max(1);

        self.cursor = self.cursor.min(last_row);
        self.offset = self.offset.min(lines.saturating_sub(window));
    }
}

/// How long the panel's filter query may get.
///
/// Longer than any caption or description, so it can never be the reason a search finds
/// nothing — and short enough that the prompt cannot push its own hints off the status line.
pub const HELP_QUERY_MAX: usize = 40;

pub struct App {
    store: Box<dyn Store>,
    order: OrderStore,

    /// Notes in display order.
    notes: Vec<NoteMeta>,
    /// Selection is held by name, never by index. A list that shifts underneath — an
    /// external create, a rescan — would otherwise silently move the user to a different
    /// note, which is the classic bug in this shape of UI.
    selected: Option<NoteId>,

    focus: Focus,
    editor: EditorPane,
    /// A delete waiting on confirmation.
    pending_delete: Option<NoteId>,
    /// When the idle buffer is due to be written, if it is waiting.
    save_deadline: Option<Instant>,
    /// An external change waiting on the user.
    conflict: Option<Conflict>,
    /// The keybindings panel, while it is open.
    help: Option<Help>,
    /// The terminal as last reported, for the handlers that need a window height.
    last_size: (u16, u16),
    /// Last known state of the workspace, and when to look again once it is bad.
    health: WorkspaceHealth,
    health_recheck: Option<Instant>,
    /// A quit that was refused because the save failed. The next one goes through.
    quit_forced: bool,
    status: Option<String>,
    quit: bool,
}

impl App {
    pub fn new(store: Box<dyn Store>, order: OrderStore) -> Result<Self> {
        let mut app = Self {
            store,
            order,
            notes: Vec::new(),
            selected: None,
            focus: Focus::Editor,
            editor: EditorPane::new(),
            pending_delete: None,
            save_deadline: None,
            conflict: None,
            help: None,
            last_size: ASSUMED_SIZE,
            health: WorkspaceHealth::Ok,
            health_recheck: None,
            quit_forced: false,
            status: None,
            quit: false,
        };
        app.refresh()?;
        app.open_selected()?;
        Ok(app)
    }

    /// Start reflecting changes made outside the app.
    ///
    /// Called after the first frame: the watcher costs about 140ms to start on macOS, and
    /// that is 140ms the user would otherwise spend looking at nothing.
    pub fn start_watching(&mut self) -> Result<()> {
        self.store.start_watching()
    }

    pub fn notes(&self) -> &[NoteMeta] {
        &self.notes
    }

    pub fn selected(&self) -> Option<&NoteId> {
        self.selected.as_ref()
    }

    /// Index of the selection, for the list widget only. Never stored.
    pub fn selected_index(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.notes.iter().position(|note| &note.id == selected)
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn editor(&self) -> &EditorPane {
        &self.editor
    }

    pub fn editor_mut(&mut self) -> &mut EditorPane {
        &mut self.editor
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn set_status(&mut self, status: String) {
        self.status = Some(status);
    }

    pub fn pending_delete(&self) -> Option<&NoteId> {
        self.pending_delete.as_ref()
    }

    /// The unresolved external change on the open note, if there is one.
    pub fn conflict(&self) -> Option<Conflict> {
        self.conflict
    }

    /// Whether the workspace is still somewhere the app can write.
    pub fn health(&self) -> WorkspaceHealth {
        self.health
    }

    // --- the keybindings panel -------------------------------------------------------

    /// The panel's state, while it is open.
    pub fn help(&self) -> Option<&Help> {
        self.help.as_ref()
    }

    /// Whether keys are currently going into the panel's filter.
    pub fn help_searching(&self) -> bool {
        self.help.as_ref().is_some_and(Help::searching)
    }

    /// What `⏎` would run: the selected row's command, if it has one.
    ///
    /// `None` for a row that describes a prompt, for quit, and for the panel's own key — the
    /// three cases where running the row from here would be wrong rather than merely useless.
    /// It walks the same list the panel draws, so the row that runs is the highlighted one
    /// even under a filter.
    pub fn help_selection(&self) -> Option<keys::Command> {
        let help = self.help.as_ref()?;
        let sections = help.listed();
        let row = sections
            .iter()
            .flat_map(|section| &section.rows)
            .nth(help.cursor())?;
        row.run
    }

    pub fn open_help(&mut self) {
        self.help = Some(Help::default());
    }

    pub fn close_help(&mut self) {
        self.help = None;
    }

    /// Move the panel's cursor, without wrapping.
    ///
    /// A list this short is easier to read when its ends hold still: a cursor that jumped
    /// from the last binding back to the first would look like the panel lost its place.
    pub fn help_move(&mut self, delta: isize) {
        self.with_help(|help| help.cursor = help.cursor.saturating_add_signed(delta));
    }

    pub fn help_to(&mut self, row: usize) {
        self.with_help(|help| help.cursor = row);
    }

    pub fn help_to_end(&mut self) {
        self.help_to(usize::MAX);
    }

    /// Move the panel's view without moving the cursor.
    pub fn help_scroll(&mut self, delta: isize) {
        self.with_help(|help| help.offset = help.offset.saturating_add_signed(delta));
    }

    /// Start filtering the list.
    pub fn help_search(&mut self) {
        self.with_help(|help| help.searching = true);
    }

    /// Leave the filter prompt, keeping the query.
    pub fn help_search_commit(&mut self) {
        self.with_help(|help| help.searching = false);
    }

    /// Leave the filter prompt and clear the query.
    ///
    /// One `esc` gives the whole list back, a second closes the panel: a user who has typed
    /// themselves into a corner should not have to decide which of the two they meant.
    pub fn help_search_cancel(&mut self) {
        self.with_help(|help| {
            help.searching = false;
            help.query.clear();
            help.offset = 0;
        });
    }

    pub fn help_type(&mut self, c: char) {
        self.with_help(|help| {
            if help.query.chars().count() < HELP_QUERY_MAX {
                help.query.push(c);
            }
            // The view is measured in lines of a list that just changed underneath it, so
            // keeping it would scroll to a place that no longer means anything.
            help.offset = 0;
        });
    }

    pub fn help_backspace(&mut self) {
        self.with_help(|help| {
            help.query.pop();
            help.offset = 0;
        });
    }

    /// Change the panel's state and put the cursor and view back inside what it now lists.
    ///
    /// Every mutator goes through here, because the indices go stale for the same reason each
    /// time: the list under them got shorter — by a filter, or by the window changing.
    fn with_help(&mut self, change: impl FnOnce(&mut Help)) {
        let size = self.last_size;
        if let Some(help) = self.help.as_mut() {
            change(help);
            help.clamp(size);
        }
    }

    /// The terminal as last reported.
    pub fn last_size(&self) -> (u16, u16) {
        self.last_size
    }

    /// Record the terminal's size.
    ///
    /// The panel's handlers need a window height and there is nowhere else to get one: the
    /// loop discarded every resize before this. A stale size costs at most one frame, because
    /// rendering clamps again against the box it actually drew.
    pub fn set_size(&mut self, width: u16, height: u16) {
        self.last_size = (width, height);
        let size = self.last_size;
        if let Some(help) = self.help.as_mut() {
            help.clamp(size);
        }
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Persist the buffer and leave.
    ///
    /// A failed save does not quit the first time: the user is about to lose work and this
    /// is the only moment they can be told. A second attempt goes through regardless, so a
    /// workspace that will never accept a write cannot trap them inside the app.
    ///
    /// Quitting while a conflict is unresolved leaves the external change on disk and drops
    /// the buffer. That is a choice the user made by pressing quit rather than answering,
    /// and refusing to exit until they answer would be worse.
    pub fn quit(&mut self) -> Result<()> {
        if self.quit_forced || self.conflict.is_some() {
            self.quit = true;
            return Ok(());
        }
        if let Err(error) = self.save_now() {
            self.quit_forced = true;
            self.status = Some(format!("{error} — press ^Q again to quit anyway"));
            return Ok(());
        }
        self.quit = true;
        Ok(())
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::List => Focus::Editor,
            Focus::Editor => Focus::List,
        };
    }

    /// Re-read the workspace and rebuild the display order.
    ///
    /// The selection survives if the note still exists; otherwise it falls to whatever now
    /// occupies its place, which keeps the cursor near where the user left it.
    pub fn refresh(&mut self) -> Result<()> {
        let previous_index = self.selected_index();

        let disk = self.store.list()?;
        let ordered = reconcile(&self.order.load(), &disk);

        let mut by_id: HashMap<NoteId, NoteMeta> = disk
            .into_iter()
            .map(|note| (note.id.clone(), note))
            .collect();
        self.notes = ordered
            .into_iter()
            .filter_map(|id| by_id.remove(&id))
            .collect();

        let still_there = self
            .selected
            .as_ref()
            .is_some_and(|id| self.notes.iter().any(|note| &note.id == id));
        if !still_there {
            let fallback = previous_index
                .unwrap_or(0)
                .min(self.notes.len().saturating_sub(1));
            self.selected = self.notes.get(fallback).map(|note| note.id.clone());
        }
        Ok(())
    }

    /// Load the selected note into the editor, if it is not already there.
    pub fn open_selected(&mut self) -> Result<()> {
        let Some(id) = self.selected.clone() else {
            self.editor.clear();
            return Ok(());
        };
        if self.editor.loaded() == Some(&id) {
            return Ok(());
        }

        match self.store.read(&id) {
            Ok(content) => self.editor.load(id, &content),
            // A note that cannot be read is still a note: show why rather than an empty
            // pane that looks like an empty note.
            Err(error) => {
                self.editor.clear();
                self.status = Some(format!("cannot open {id}: {error}"));
            }
        }
        Ok(())
    }

    // --- editing and autosave --------------------------------------------------------

    /// Hand a keystroke to the editor and restart the idle countdown.
    ///
    /// One deadline moved forward on each key, not a timer per keystroke: fifty characters
    /// typed in a burst leave one pending save behind them, not fifty.
    pub fn edit(&mut self, key: KeyEvent) {
        self.editor.on_key(key);
        self.arm_save();
    }

    /// When the loop should wake up on its own, if at all.
    ///
    /// `None` is the normal answer. An idle scratchpad with nothing pending has no reason
    /// to wake up and discover that it is still idle.
    pub fn wake_at(&self) -> Option<Instant> {
        match (self.save_deadline, self.health_recheck) {
            (Some(save), Some(health)) => Some(save.min(health)),
            (save, health) => save.or(health),
        }
    }

    /// The loop woke up on its own: a save may be due, or a broken workspace may be back.
    pub fn on_tick(&mut self) -> Result<()> {
        let now = Instant::now();
        if self.health_recheck.is_some_and(|at| now >= at) {
            self.recheck_health()?;
        }
        if self.save_deadline.is_some_and(|at| now >= at) {
            self.save_now()?;
        }
        Ok(())
    }

    /// Write the buffer, if there is anything to write and anywhere to write it.
    pub fn save_now(&mut self) -> Result<()> {
        self.save_deadline = None;
        if self.conflict.is_some() || !self.editor.is_dirty() {
            return Ok(());
        }
        if !self.check_health() {
            return Ok(());
        }

        match save::save(self.store.as_ref(), &self.order, &mut self.editor) {
            Ok(save::Outcome::Renamed { from, to }) => {
                // The note the user is looking at is the one that just changed name; the
                // selection follows it rather than staying on a name nothing answers to.
                if self.selected.as_ref() == Some(&from) {
                    self.selected = Some(to);
                }
                self.refresh()
            }
            Ok(_) => Ok(()),
            Err(error) => {
                // A workspace that disappeared mid-save explains itself far better as the
                // degraded banner than as a one-off error the user has to interpret.
                self.check_health();
                Err(error)
            }
        }
    }

    /// Give the buffer its quiet period before it is written.
    fn arm_save(&mut self) {
        if self.conflict.is_some() || self.health != WorkspaceHealth::Ok {
            return;
        }
        // Cleared rather than left armed when the buffer is clean: undoing back to what is
        // already on disk should cancel the pending write, not perform it.
        self.save_deadline = self.editor.is_dirty().then(|| Instant::now() + IDLE_SAVE);
    }

    /// Look at the workspace, entering or leaving the degraded state as needed.
    ///
    /// Returns whether it is safe to write.
    fn check_health(&mut self) -> bool {
        let health = self.store.health();
        let usable = health == WorkspaceHealth::Ok;
        if health == self.health {
            return usable;
        }
        self.health = health;

        if usable {
            self.health_recheck = None;
        } else {
            // Not retried every 300ms: an autosave loop against a dead mount burns CPU and
            // has nothing new to say each time round.
            self.save_deadline = None;
            self.health_recheck = Some(Instant::now() + HEALTH_RECHECK);
        }
        usable
    }

    /// Prod a workspace that was unavailable, and flush the buffer if it came back.
    fn recheck_health(&mut self) -> Result<()> {
        if self.check_health() {
            // Everything typed while it was gone has never reached the disk.
            return self.save_now();
        }
        self.health_recheck = Some(Instant::now() + HEALTH_RECHECK);
        Ok(())
    }

    /// Keep the buffer and write it, re-creating the note if it was deleted underneath.
    pub fn keep_mine(&mut self) -> Result<()> {
        self.conflict = None;
        self.save_now()?;
        self.refresh()
    }

    /// Discard the buffer and take whatever is on disk now.
    pub fn take_theirs(&mut self) -> Result<()> {
        self.conflict = None;
        let Some(id) = self.editor.loaded().cloned() else {
            return Ok(());
        };

        match self.store.read(&id) {
            Ok(disk) => {
                tracing::debug!(id = ?id.as_str(), bytes = disk.len(), "buffer reloaded from disk");
                self.editor.reload(&disk);
                Ok(())
            }
            // Taking theirs when theirs is a deletion means letting the note go.
            Err(_) => {
                self.editor.clear();
                self.selected = None;
                self.refresh()?;
                self.open_selected()
            }
        }
    }

    fn enter_conflict(&mut self, conflict: Conflict) {
        // `warn`, because reaching here means the app is about to stop autosaving and hand a
        // decision to the user. In a log read after the fact it is the line that explains why
        // nothing was written for the rest of the session.
        tracing::warn!(
            id = ?self.editor.loaded().map(NoteId::as_str),
            kind = ?conflict,
            "conflict entered"
        );
        self.conflict = Some(conflict);
        // Two modals on screen at once means the user answers the wrong question. The panel
        // is only ever open because somebody is reading it, so it is the one that gives way.
        self.help = None;
        // The decisive half of the policy. Autosave that kept running from here would write
        // the buffer straight over the external change, which is the loss the conflicted
        // state exists to prevent.
        self.save_deadline = None;
    }

    // --- navigation ------------------------------------------------------------------

    pub fn select_next(&mut self) -> Result<()> {
        self.select_by_offset(1)
    }

    pub fn select_previous(&mut self) -> Result<()> {
        self.select_by_offset(-1)
    }

    fn select_by_offset(&mut self, offset: isize) -> Result<()> {
        // Switching notes reloads the buffer, so doing it with a conflict unresolved would
        // throw away the work the conflict is protecting.
        if self.conflict.is_some() {
            return Ok(());
        }
        self.save_now()?;

        let Some(current) = self.selected_index() else {
            return Ok(());
        };
        let target = current
            .saturating_add_signed(offset)
            .min(self.notes.len() - 1);
        self.selected = self.notes.get(target).map(|note| note.id.clone());
        self.open_selected()
    }

    /// Create an empty note and open it.
    pub fn create_note(&mut self) -> Result<()> {
        if self.conflict.is_some() {
            return Ok(());
        }
        // The note being left behind is about to be replaced in the buffer.
        self.save_now()?;

        let id = self.store.create(Format::Markdown)?;

        // A new note belongs at the top, and saying so in the manifest is what makes that
        // survive a restart rather than depending on it also being the newest file.
        let ids: Vec<NoteId> = std::iter::once(id.clone())
            .chain(self.notes.iter().map(|note| note.id.clone()))
            .collect();
        self.order.save(&ids)?;

        self.refresh()?;
        self.selected = Some(id);
        self.focus = Focus::Editor;
        self.open_selected()
    }

    /// Ask before deleting. Notes are cheap to make and annoying to lose.
    pub fn request_delete(&mut self) {
        self.pending_delete = self.selected.clone();
    }

    pub fn cancel_delete(&mut self) {
        self.pending_delete = None;
    }

    /// Move the confirmed note to the trash.
    pub fn confirm_delete(&mut self) -> Result<()> {
        let Some(id) = self.pending_delete.take() else {
            return Ok(());
        };
        // Whatever was pending for this note dies with it. Writing a note on its way to the
        // trash would only put it back.
        self.save_deadline = None;
        self.store.delete(&id)?;
        self.status = Some(format!("{id} moved to the trash"));

        self.selected = None;
        self.refresh()?;
        self.open_selected()
    }

    pub fn move_selection_up(&mut self) -> Result<()> {
        self.reorder(-1)
    }

    pub fn move_selection_down(&mut self) -> Result<()> {
        self.reorder(1)
    }

    fn reorder(&mut self, offset: isize) -> Result<()> {
        let Some(index) = self.selected_index() else {
            return Ok(());
        };

        let mut ids: Vec<NoteId> = self.notes.iter().map(|note| note.id.clone()).collect();
        if offset < 0 {
            order::move_up(&mut ids, index);
        } else {
            order::move_down(&mut ids, index);
        }

        // Persisted immediately: a reorder is a deliberate act, and losing it to a crash
        // would be more surprising than the cost of one small write.
        self.order.save(&ids)?;
        self.refresh()
    }

    /// React to a change made outside the app.
    ///
    /// Everything the app does to the workspace itself is suppressed before it reaches
    /// here, so an event that arrives is somebody else's: another instance, a text editor,
    /// a sync daemon, or the user in Finder.
    pub fn apply_store_event(&mut self, event: &StoreEvent) -> Result<()> {
        let open = self.editor.loaded().cloned();

        // An external rename moves the note, not its contents. Following it keeps the old
        // name's disappearance from reading as a delete.
        if let StoreEvent::Renamed { from, to } = event
            && open.as_ref() == Some(from)
        {
            self.editor.rename(to.clone());
            self.selected = Some(to.clone());
        }

        match event {
            // A change to a note's contents moves nothing in the list.
            StoreEvent::Modified(_) => {}
            _ => self.refresh()?,
        }

        let touches_open = match event {
            // No path to check against, so the open note is checked directly. See
            // `reconcile_open_note` for why that is cheap enough.
            StoreEvent::Rescan => true,
            StoreEvent::Created(id) | StoreEvent::Modified(id) | StoreEvent::Removed(id) => {
                open.as_ref() == Some(id)
            }
            // Already handled: the name moved and the contents are current.
            StoreEvent::Renamed { .. } => false,
        };
        if touches_open {
            self.reconcile_open_note()?;
        }
        Ok(())
    }

    /// Decide what an external change means for the buffer.
    ///
    /// The only place allowed to replace what the user is looking at, and it refuses to do
    /// so whenever there is unsaved work. An external edit is never worth a lost keystroke,
    /// so the choice goes to the user and autosave stops until they make it.
    ///
    /// `Rescan` reaches here too. It carries no path, so the open note is read and compared
    /// rather than guessed at, which turns an event with no information into exactly the
    /// same three outcomes as a `Modified` for the open note. One file read, and `Rescan`
    /// is rare by construction.
    fn reconcile_open_note(&mut self) -> Result<()> {
        // Already asked. Asking again would only redraw the same question.
        if self.conflict.is_some() {
            return Ok(());
        }
        let Some(id) = self.editor.loaded().cloned() else {
            return Ok(());
        };

        match self.store.read(&id) {
            Ok(disk) => {
                // Byte-identical to what was last read or written: our own save echoing
                // back, or a rescan that found nothing new. Nothing to reconcile.
                if disk == self.editor.baseline() {
                    return Ok(());
                }
                if self.editor.is_dirty() {
                    self.enter_conflict(Conflict::Changed);
                } else {
                    tracing::debug!(
                        id = ?id.as_str(),
                        bytes = disk.len(),
                        "buffer reloaded from disk"
                    );
                    self.editor.reload(&disk);
                }
                Ok(())
            }
            Err(_) if self.editor.is_dirty() => {
                self.enter_conflict(Conflict::Deleted);
                // The list has already dropped the note, but the buffer still holds it.
                // Keeping the selection on it means the two are talking about the same
                // note while the user decides.
                self.selected = Some(id);
                Ok(())
            }
            // Nothing to lose: the buffer held nothing new. Either the note is gone, and
            // the list has already moved on without it, or it survives but can no longer be
            // read — and saying so beats leaving its old contents on screen unexplained.
            Err(error) => {
                if self.notes.iter().any(|note| note.id == id) {
                    self.status = Some(format!("cannot reload {id}: {error}"));
                    return Ok(());
                }
                self.open_selected()
            }
        }
    }
}
