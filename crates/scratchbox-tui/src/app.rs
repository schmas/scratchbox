//! Application state.
//!
//! Deliberately free of terminal types so the whole of it can be driven from a test with
//! no TTY: rendering reads this state, it never owns it.

use std::collections::HashMap;

use scratchbox_core::note::NoteMeta;
use scratchbox_core::order::{self, OrderStore};
use scratchbox_core::{Format, NoteId, Result, Store, StoreEvent, reconcile};

use crate::editor::EditorPane;

/// Which pane the keyboard is talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Editor,
}

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

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn quit(&mut self) {
        self.quit = true;
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

    pub fn select_next(&mut self) -> Result<()> {
        self.select_by_offset(1)
    }

    pub fn select_previous(&mut self) -> Result<()> {
        self.select_by_offset(-1)
    }

    fn select_by_offset(&mut self, offset: isize) -> Result<()> {
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
    /// The editor buffer is deliberately untouched here. Only the code that knows whether
    /// the buffer has unsaved work can decide what to do with an external edit, and this is
    /// not that code — reloading from here would discard whatever the user has typed.
    pub fn apply_store_event(&mut self, event: &StoreEvent) -> Result<()> {
        match event {
            StoreEvent::Rescan
            | StoreEvent::Created(_)
            | StoreEvent::Removed(_)
            | StoreEvent::Renamed { .. } => self.refresh(),
            // A change to a note's contents moves nothing in the list.
            StoreEvent::Modified(_) => Ok(()),
        }
    }
}
