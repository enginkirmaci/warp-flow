//! The artifacts screen: a run's documents, in tabs, edited in place.
//!
//! The issue folder is the protocol (ADR-0009), so this screen is a text editor
//! over the files the engine and a person share. Three rules keep it honest:
//!
//! - a tab is the file's **whole text**, front matter included, so saving never
//!   rewrites a document nobody touched;
//! - the only writer is [`write_artifact`] — atomic, and refused while the
//!   engine drives the run, because it owns the folder then;
//! - an unsaved draft survives a tab switch: the tab keeps a mark until it is
//!   saved or the run changes, and nothing is a
//!   utosaved behind a person's back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, FocusHandle, FontWeight, IntoElement, Render, ScrollHandle,
    SharedString, Window, div, px,
};
use warpui_kit_core::A11y;
use warpui_kit_core::controls::{self, BtnKind};
use warpui_kit_markdown::{LiveEditor, LiveEditorState};

use crate::host::Host;
use crate::store;
use crate::theme;

use super::{ScreenRun, placeholder, write_artifact};

/// One tab: a file in the issue folder, and whether a person may change it.
struct Tab {
    path: PathBuf,
    /// What the tab strip shows.
    label: String,
    /// `spec.md` and the task files are a person's; `run.md` is the engine's
    /// record, `workflow.toml` has its own editor, a question is answered on
    /// the run screen.
    editable: bool,
    /// Why the tab is read-only, shown in one line under the strip.
    lock_note: Option<&'static str>,
}

/// The screen's data, as read from the folder off the render path.
#[derive(Default)]
struct Loaded {
    /// Every tab's text as last read from disk (or written by this screen).
    files: BTreeMap<PathBuf, String>,
    /// Edits that are not saved yet, per path.
    drafts: BTreeMap<PathBuf, String>,
}

/// The artifacts screen: the issue folder's own documents.
pub struct ArtifactsScreen {
    host: Arc<Host>,
    run: Option<ScreenRun>,
    tabs: Vec<Tab>,
    active: usize,
    loaded: Loaded,
    /// The run and tab the editor currently holds, so a poll that changed
    /// nothing else does not reload it under the cursor.
    shown: Option<(PathBuf, PathBuf)>,
    /// The one document surface: editable for a person's file, read-only for
    /// the engine's. Created in render, the way the run screen's read-only
    /// documents are, because the kit's editor state needs a `Window`.
    editor: Option<Entity<LiveEditorState>>,
    /// What the last save said, in the screen's own words.
    status: Option<String>,
    /// What went wrong, in the engine's or the filesystem's words.
    error: Option<String>,
    loading: bool,
    scroll: ScrollHandle,
    focus: FocusHandle,
}

impl ArtifactsScreen {
    /// A screen with no run.
    pub fn new(host: Arc<Host>, cx: &mut Context<Self>) -> Self {
        Self {
            host,
            run: None,
            tabs: Vec::new(),
            active: 0,
            loaded: Loaded::default(),
            shown: None,
            editor: None,
            status: None,
            error: None,
            loading: false,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
        }
    }

    /// Show `run`: read the folder when it is a run this screen has not loaded,
    /// or when the run's driving state changed the permissions.
    pub fn show(&mut self, run: ScreenRun, cx: &mut Context<Self>) {
        let same_run = self.run.as_ref().is_some_and(|shown| {
            shown.run.dir == run.run.dir && shown.run.key.repo == run.run.key.repo
        });
        let driving_changed = self
            .run
            .as_ref()
            .is_some_and(|shown| shown.driving != run.driving);
        self.run = Some(run);
        if same_run && !driving_changed {
            return;
        }
        self.loaded = Loaded::default();
        self.shown = None;
        self.status = None;
        self.error = None;
        self.active = 0;
        self.load(cx);
    }

    /// Read the folder on the background executor and adopt the tabs.
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.run.clone() else {
            return;
        };
        let issue = run.issue().to_path_buf();
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move { read_folder(&issue) })
                .await;
            let _ = this.update(cx, |this, cx| this.adopt(read, cx));
        })
        .detach();
    }

    fn adopt(&mut self, read: ReadFolder, cx: &mut Context<Self>) {
        self.loading = false;
        self.error = read.error.clone();
        self.loaded.files = read.files.clone();
        self.tabs = tabs_for(&read, self.run.as_ref());
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        self.shown = None;
        cx.notify();
    }

    /// The editor for the active tab, created on first need and replaced when
    /// the tab changes. The outgoing tab's text is kept as its draft, so a
    /// switch never loses an edit.
    fn ensure_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<LiveEditorState>> {
        let run = self.run.clone()?;
        let tab = self.tabs.get(self.active)?;
        let key = (run.run.dir.clone(), tab.path.clone());
        let editable = tab.editable && !run.driving;
        if self.shown.as_ref() == Some(&key)
            && let Some(editor) = self.editor.clone()
        {
            let _ = editable;
            return Some(editor);
        }
        // keep what the person typed in the tab they are leaving
        if let (Some((_, previous)), Some(editor)) = (self.shown.clone(), self.editor.clone()) {
            let value = editor.read(cx).value().to_string();
            if self.loaded.files.get(&previous) != Some(&value) {
                self.loaded.drafts.insert(previous, value);
            }
        }
        let text = self
            .loaded
            .drafts
            .get(&key.1)
            .cloned()
            .or_else(|| self.loaded.files.get(&key.1).cloned())
            .unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut state = LiveEditorState::new(window, cx);
            state.set_read_only(!editable, cx);
            state.set_value(SharedString::from(text), window, cx);
            state
        });
        cx.observe(&editor, |_, _, cx| cx.notify()).detach();
        self.editor = Some(editor.clone());
        self.shown = Some(key);
        Some(editor)
    }

    /// Switch tabs. The outgoing tab's draft is kept by `ensure_editor` on the
    /// next frame.
    fn select(&mut self, index: usize, _cx: &mut Context<Self>) {
        if index == self.active || index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.status = None;
        self.error = None;
    }

    /// The one write: the active tab's text, through the screen seam.
    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(run) = self.run.clone() else {
            return;
        };
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let path = tab.path.clone();
        let text = editor.read(cx).value().to_string();
        let store_run = Arc::clone(&run.run);
        let host = Arc::clone(&self.host);
        self.status = None;
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(
                    async move { write_artifact(&host, &store_run, &path, &text).map(|()| text) },
                )
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(text) => {
                    if let Some(path) = this.tabs.get(this.active).map(|tab| tab.path.clone()) {
                        this.loaded.files.insert(path.clone(), text);
                        this.loaded.drafts.remove(&path);
                    }
                    this.status = Some(format!("Saved {}", now_word()));
                    cx.notify();
                }
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The active tab is dirty: its text differs from what is on disk.
    fn dirty(&self, cx: &Context<Self>) -> bool {
        let Some(path) = self.tabs.get(self.active).map(|tab| tab.path.clone()) else {
            return false;
        };
        let Some(editor) = self.editor.as_ref() else {
            return false;
        };
        match self.loaded.files.get(&path) {
            Some(file) => editor.read(cx).value().as_ref() != file,
            None => false,
        }
    }

    // ---- rendering ---------------------------------------------------------

    fn render_strip(&self, cx: &Context<Self>) -> AnyElement {
        let path = self.tabs.get(self.active).map(|tab| tab.path.clone());
        let mut tabs_row = div()
            .id("flow-artifacts-tabs")
            .debug_selector(|| "flow-artifacts-tabs".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(4.))
            .child(
                div()
                    .px(px(2.))
                    .text_size(px(theme::font_micro()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text_muted())
                    .child("FILES"),
            );
        for (index, tab) in self.tabs.iter().enumerate() {
            let selected = index == self.active;
            let drafted = self
                .loaded
                .drafts
                .get(&tab.path)
                .is_some_and(|draft| self.loaded.files.get(&tab.path) != Some(draft));
            let selector = format!("flow-artifact-tab-{index}");
            let label = tab.label.clone();
            tabs_row = tabs_row.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .a11y_button(format!("Open {label}"))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .h(px(26.))
                    .px(px(9.))
                    .rounded(px(6.))
                    .when(selected, |style| style.bg(theme::raised()))
                    .text_size(px(theme::font_meta()))
                    .text_color(if selected {
                        theme::text()
                    } else {
                        theme::text_dim()
                    })
                    .hover(|style| style.text_color(theme::text()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select(index, cx);
                        cx.notify();
                    }))
                    .child(label)
                    .when(!tab.editable, |row| {
                        row.child(
                            div()
                                .text_color(theme::text_muted())
                                .text_size(px(theme::font_micro()))
                                .child("read"),
                        )
                    })
                    .when(drafted, |row| {
                        row.child(
                            div()
                                .size(px(5.))
                                .rounded_full()
                                .bg(theme::accent())
                                .into_any_element(),
                        )
                    }),
            );
        }

        let mut actions = div()
            .id("flow-artifacts-actions")
            .flex()
            .items_center()
            .gap(px(8.))
            .min_h(px(26.));
        if let Some(path) = &path
            && self.dirty(cx)
        {
            actions = actions.child(
                div()
                    .debug_selector(|| "flow-artifacts-dirty".into())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(format!("unsaved · {}", file_name(path))),
            );
            actions = actions.child(
                // the kit's button takes an element id, not a debug selector, so
                // the named wrapper is what a probe clicks
                div()
                    .debug_selector(|| "flow-artifacts-save".into())
                    .child(controls::button(
                        "flow-artifacts-save-button",
                        "Save",
                        BtnKind::Primary(theme::on_accent()),
                        theme::accent(),
                        theme::font_control(),
                        true,
                        cx.listener(|this, _, _, cx| this.save(cx)),
                    )),
            );
        }
        if let Some(status) = &self.status {
            actions = actions.child(
                div()
                    .debug_selector(|| "flow-artifacts-status".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::ADD)
                    .child(status.clone()),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(tabs_row)
            .child(actions)
            .into_any_element()
    }

    fn render_lock_note(&self) -> Option<AnyElement> {
        let tab = self.tabs.get(self.active)?;
        let driving = self.run.as_ref().is_some_and(|run| run.driving);
        let note = if tab.editable && driving {
            Some(
                "Warp Flow is driving this run; its folder is the engine's until it stops at a step boundary.",
            )
        } else {
            tab.lock_note
        }?;
        Some(
            div()
                .debug_selector(|| "flow-artifacts-lock".into())
                .text_size(px(theme::font_meta()))
                .text_color(theme::text_dim())
                .child(note)
                .into_any_element(),
        )
    }
}

impl ArtifactsScreen {
    /// The document surface, for a probe that needs to type into it.
    #[doc(hidden)]
    pub fn editor_for_probe(&self) -> Option<Entity<LiveEditorState>> {
        self.editor.clone()
    }

    /// The active tab's label, for a probe that checks the strip.
    #[doc(hidden)]
    pub fn active_tab_for_probe(&self) -> Option<String> {
        self.tabs.get(self.active).map(|tab| tab.label.clone())
    }

    /// Every tab's label, in strip order.
    #[doc(hidden)]
    pub fn tabs_for_probe(&self) -> Vec<String> {
        self.tabs.iter().map(|tab| tab.label.clone()).collect()
    }
}

impl Render for ArtifactsScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(run) = self.run.clone() else {
            return placeholder("artifacts");
        };
        let Some(tab) = self.tabs.get(self.active).map(|tab| Tab {
            path: tab.path.clone(),
            label: tab.label.clone(),
            editable: tab.editable,
            lock_note: tab.lock_note,
        }) else {
            return div()
                .id("flow-screen-artifacts")
                .debug_selector(|| "flow-screen-artifacts".into())
                .flex()
                .flex_1()
                .flex_col()
                .gap(px(8.))
                .p(px(theme::scaled(theme::BOARD_PADDING)))
                .child(note_line(if self.loading {
                    "Reading the issue folder…"
                } else {
                    "This run has no documents yet. A run writes its spec when it starts."
                }))
                .into_any_element();
        };
        let editable = tab.editable && !run.driving;
        let path = tab.path.clone();
        let editor = self.ensure_editor(window, cx);

        let mut head = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(theme::scaled(theme::BOARD_PADDING)))
            .pt(px(16.))
            .pb(px(10.))
            .border_b_1()
            .border_color(theme::border_light())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .debug_selector(|| "flow-screen-artifacts".into())
                            .text_size(px(theme::font_list_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(run.title().to_owned()),
                    )
                    .child(
                        div()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .text_color(theme::text_muted())
                            .child(file_name(&path)),
                    ),
            )
            .child(self.render_strip(cx));
        if let Some(note) = self.render_lock_note() {
            head = head.child(note);
        }
        if let Some(error) = &self.error {
            head = head.child(
                div()
                    .debug_selector(|| "flow-artifacts-error".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::DANGER)
                    .child(error.clone()),
            );
        }

        let body = if tab.editable || is_markdown(&path) {
            div()
                .id("flow-artifacts-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .p(px(theme::scaled(theme::BOARD_PADDING)))
                .child(if editable {
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(
                            LiveEditor::new(editor.as_ref().expect("the editor just ensured"))
                                .aria_label(SharedString::from(format!(
                                    "{} contents",
                                    file_name(&path)
                                )))
                                .text_size(px(theme::font_body()))
                                .surface(theme::surface())
                                .into_any_element(),
                        )
                        .into_any_element()
                } else {
                    match editor.as_ref() {
                        Some(editor) => {
                            warpui_kit_markdown::render_read_only_document_in_card(editor, cx)
                        }
                        None => note_line("There is nothing to read yet."),
                    }
                })
        } else {
            // a `.toml` document is not prose: monospace, as it is on disk
            div()
                .id("flow-artifacts-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .p(px(theme::scaled(theme::BOARD_PADDING)))
                .child(
                    div()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(theme::hairline())
                        .bg(theme::panel())
                        .p(px(12.))
                        .font_family(theme::code_font())
                        .text_size(px(theme::font_mono_meta()))
                        .text_color(theme::text_dim())
                        .child(
                            editor
                                .as_ref()
                                .map(|editor| editor.read(cx).value().to_string())
                                .unwrap_or_default(),
                        ),
                )
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::surface())
            .track_focus(&self.focus)
            .child(head)
            .child(body)
            .into_any_element()
    }
}

// ---- reading the folder ----------------------------------------------------

/// What reading the issue folder produced.
#[derive(Default)]
struct ReadFolder {
    files: BTreeMap<PathBuf, String>,
    error: Option<String>,
}

/// Read every document the screen shows. Pure filesystem work: it runs on the
/// background executor, never in a frame.
fn read_folder(issue: &Path) -> ReadFolder {
    let mut read = ReadFolder::default();
    let mut read_one = |path: PathBuf| match std::fs::read_to_string(&path) {
        Ok(text) => {
            read.files.insert(path, text);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            read.error
                .get_or_insert_with(|| format!("{}: {error}", path.display()));
        }
    };
    for name in ["spec.md", "tasks.md", "run.md", "workflow.toml"] {
        read_one(issue.join(name));
    }
    for (dir, extension) in [("issues", "md"), ("questions", "md")] {
        let folder = issue.join(dir);
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some(extension))
            .collect();
        paths.sort();
        for path in paths {
            read_one(path);
        }
    }
    read
}

/// The tabs for what the folder holds: the person's documents first, then the
/// engine's record and the graph.
fn tabs_for(read: &ReadFolder, run: Option<&ScreenRun>) -> Vec<Tab> {
    let mut tabs = Vec::new();
    let spec = Path::new("spec.md");
    let issue = run.map(|run| run.issue().to_path_buf());
    if let Some(issue) = &issue {
        tabs.push(Tab {
            path: issue.join(spec),
            label: "Spec".to_owned(),
            editable: true,
            lock_note: None,
        });
    }
    for path in read.files.keys() {
        match path.file_name().and_then(|name| name.to_str()) {
            Some("tasks.md") => tabs.push(Tab {
                path: path.clone(),
                label: "Tasks".to_owned(),
                editable: true,
                lock_note: None,
            }),
            Some(name) if path.parent().is_some_and(|dir| dir.ends_with("issues")) => {
                let number = name.split('-').next().unwrap_or(name);
                tabs.push(Tab {
                    path: path.clone(),
                    label: format!("Task {number}"),
                    editable: true,
                    lock_note: None,
                });
            }
            _ => {}
        }
    }
    if let Some(issue) = &issue {
        tabs.push(Tab {
            path: issue.join("run.md"),
            label: "Run record".to_owned(),
            editable: false,
            lock_note: Some(
                "`run.md` is the engine's record of what happened; it is written by the engine, never by this screen.",
            ),
        });
        tabs.push(Tab {
            path: issue.join("workflow.toml"),
            label: "Workflow file".to_owned(),
            editable: false,
            lock_note: Some(
                "`workflow.toml` has its own editor — the Workflow screen, which bumps the revision on save.",
            ),
        });
    }
    for path in read.files.keys() {
        if path.parent().is_some_and(|dir| dir.ends_with("questions")) {
            let number = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            tabs.push(Tab {
                path: path.clone(),
                label: format!("Q{number}"),
                editable: false,
                lock_note: Some(
                    "A question is answered on the run screen, which writes the answer the engine reads.",
                ),
            });
        }
    }
    // a spec tab with no file yet is still the first tab: the first save
    // creates it
    tabs
}

/// The last path segment.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Whether a file renders as markdown.
fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("md")
}

/// The clock, for the save line: `14:03`.
fn now_word() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let seconds = now % 86_400;
    format!("{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60)
}

/// A quiet one-line note.
fn note_line(text: &'static str) -> AnyElement {
    div()
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_dim())
        .child(text)
        .into_any_element()
}

// keep the `store` import honest: the screen names its run by the store's type
const _: fn(&store::Run) -> &Path = |run| &run.dir;
