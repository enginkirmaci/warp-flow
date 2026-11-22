//! The artifacts tab: a run's documents, in the 240 file rail beside the
//! document at the 760 reading measure.
//!
//! The issue folder is the protocol (ADR-0009), so this screen is the one place
//! a person reads what the engine wrote and, while the engine is not driving,
//! edits it. Three rules keep it honest:
//!
//! - a tab is the file's **whole text**, front matter included, so saving never
//!   rewrites a document nobody touched;
//! - the only writer is [`write_artifact`] — atomic, and refused while the
//!   engine drives the run, because it owns the folder then;
//! - an unsaved draft survives a file switch: the rail row keeps a mark until it
//!   is saved or the run changes, and nothing is autosaved behind a person's
//!   back.
//!
//! The design makes this tab a reader ("the engine is the only writer while it
//! drives"). The write path stays — no existing feature is removed — and the
//! read-only case is said in the head and in the lock line: the engine drives
//! the run, or the file itself is the engine's record.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    AnyElement, ClipboardItem, Context, Entity, FocusHandle, FontWeight, IntoElement, Render,
    ScrollHandle, SharedString, Window, div, px,
};
use warpui_kit_core::controls::{self, BtnKind};
use warpui_kit_core::icon;
use warpui_kit_core::motion;
use warpui_kit_core::{A11y as _, shell};
use warpui_kit_markdown::{LiveEditor, LiveEditorState};

use crate::host::Host;
use crate::theme;

use super::{ScreenRun, placeholder, write_artifact};

/// The rail's head height: `.scratch/<run>` over the file count.
const RAIL_HEAD_HEIGHT: f32 = 40.;

/// One file row's height, as §7 draws it.
const FILE_ROW_HEIGHT: f32 = 58.;

/// One tab: a file in the issue folder, and whether a person may change it.
struct Tab {
    path: PathBuf,
    /// What the strip shows; the probe reads it back, so it keeps the file's
    /// role name rather than its path.
    label: String,
    /// The rail row's one-line meaning.
    description: String,
    /// `spec.md` and the task files are a person's; `run.md` is the engine's
    /// record, `workflow.toml` has its own editor, a question is answered on
    /// the run screen.
    editable: bool,
    /// Why the tab is read-only, shown in one line under the head.
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

/// The artifacts tab: the issue folder's own documents.
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
    /// What the last save (or copy) said, in the screen's own words.
    status: Option<String>,
    /// What went wrong, in the engine's or the filesystem's words.
    error: Option<String>,
    loading: bool,
    /// The document's scroll position.
    scroll: ScrollHandle,
    /// The file rail's own scroll position, kept across a poll.
    rail_scroll: ScrollHandle,
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
            rail_scroll: ScrollHandle::new(),
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
        self.rail_scroll = ScrollHandle::new();
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

    /// Switch files. The outgoing tab's draft is kept by `ensure_editor` on the
    /// next frame.
    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index == self.active || index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.status = None;
        self.error = None;
        cx.notify();
    }

    /// Put the active document on the clipboard, as the screen's one reading
    /// action (§7's only control besides the write path the app keeps).
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.tabs.get(self.active).map(|tab| tab.path.clone()) else {
            return;
        };
        let text = self
            .editor
            .as_ref()
            .map(|editor| editor.read(cx).value().to_string())
            .or_else(|| self.loaded.files.get(&path).cloned())
            .unwrap_or_default();
        if text.is_empty() {
            self.status = None;
            self.error = Some(format!("{} has nothing to copy yet.", file_name(&path)));
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.status = Some(format!("Copied {}", file_name(&path)));
            self.error = None;
        }
        cx.notify();
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

    /// Whether any tab holds an unsaved draft, for the rail's marks.
    fn drafted(&self, tab: &Tab) -> bool {
        self.loaded
            .drafts
            .get(&tab.path)
            .is_some_and(|draft| self.loaded.files.get(&tab.path) != Some(draft))
    }

    // ---- rendering ---------------------------------------------------------

    /// The 240 file rail: a 40 head, then one 58 row per document.
    fn render_rail(&self, run: &ScreenRun, cx: &mut Context<Self>) -> AnyElement {
        let folder = folder_label(run);
        let mut rows = div()
            .debug_selector(|| "flow-artifacts-rows".into())
            .flex()
            .flex_col()
            .w_full();
        for (index, tab) in self.tabs.iter().enumerate() {
            rows = rows.child(self.render_rail_row(index, tab, cx));
        }
        div()
            .id("flow-artifacts-rail")
            .debug_selector(|| "flow-artifacts-rail".into())
            .flex_shrink_0()
            .w(px(theme::scaled(theme::ARTIFACTS_RAIL_WIDTH)))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::surface())
            .border_r_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(theme::scaled(RAIL_HEAD_HEIGHT)))
                    .px(px(16.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(theme::hairline())
                    .child(
                        div()
                            .debug_selector(|| "flow-artifacts-folder".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .text_color(theme::text_dim())
                            .child(folder),
                    )
                    .child(
                        div()
                            .debug_selector(|| "flow-artifacts-count".into())
                            .flex_shrink_0()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .text_color(theme::text_muted())
                            .child(self.tabs.len().to_string()),
                    ),
            )
            .child(
                div()
                    .id("flow-artifacts-tabs")
                    .debug_selector(|| "flow-artifacts-tabs".into())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.rail_scroll)
                    .child(rows),
            )
            .into_any_element()
    }

    /// One file row: the file glyph, its name in mono and the one-line meaning.
    fn render_rail_row(&self, index: usize, tab: &Tab, cx: &mut Context<Self>) -> AnyElement {
        let selected = index == self.active;
        let drafted = self.drafted(tab);
        let name = file_name(&tab.path);
        let description = tab.description.clone();
        let selector = format!("flow-artifact-tab-{index}");
        let a11y = format!("{}: {description}", tab.label);
        let ink = if selected {
            theme::accent_ink()
        } else {
            theme::text_muted()
        };
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .a11y_tab(a11y, selected)
            .cursor_pointer()
            .w_full()
            .h(px(theme::scaled(FILE_ROW_HEIGHT)))
            .pl(px(13.))
            .pr(px(10.))
            .py(px(8.))
            .flex()
            .items_center()
            .gap(px(10.))
            .border_l_3()
            .border_color(if selected {
                theme::accent()
            } else {
                theme::surface()
            })
            .when(selected, |row| row.bg(theme::raised()))
            .when(!selected, |row| row.hover(|style| style.bg(theme::panel())))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.select(index, cx);
            }))
            .child(icon("icons/icon-file.svg", px(theme::icon_tiny()), ink))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .flex_1()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(theme::code_font())
                                    .text_size(px(theme::font_mono_meta()))
                                    .text_color(if selected {
                                        theme::text()
                                    } else {
                                        theme::text_dim()
                                    })
                                    .child(name),
                            )
                            .when(drafted, |row| {
                                row.child(
                                    div()
                                        .size(px(5.))
                                        .rounded_full()
                                        .bg(theme::accent())
                                        .into_any_element(),
                                )
                            }),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_dim())
                            .child(description),
                    ),
            )
            .into_any_element()
    }

    /// The document's head: the file at 15/600 with Copy, then the folder
    /// path, the meaning and who owns the file.
    fn render_doc_head(&self, run: &ScreenRun, tab: &Tab, cx: &mut Context<Self>) -> AnyElement {
        let name = file_name(&tab.path);
        let folder = folder_label(run);
        let relative = format!("{folder}/{name}");
        let owner = if tab.editable {
            "the engine is the only writer while it drives"
        } else {
            "the engine writes this file"
        };
        div()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .pt(px(16.))
            .pb(px(12.))
            .border_b_1()
            .border_color(theme::hairline())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .debug_selector(|| "flow-artifacts-file-name".into())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::font_name()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child(name),
                    )
                    .child(self.render_copy_button(cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(
                        div()
                            .debug_selector(|| "flow-artifacts-path".into())
                            .font_family(theme::code_font())
                            .child(relative),
                    )
                    .child(div().text_color(theme::line_strong()).child("·"))
                    .child(
                        div()
                            .text_color(theme::text_dim())
                            .child(tab.description.clone()),
                    )
                    .child(div().text_color(theme::line_strong()).child("·"))
                    .child(div().child(owner)),
            )
            .into_any_element()
    }

    /// Copy, the head's one reading action (§7).
    fn render_copy_button(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("flow-artifacts-copy")
            .debug_selector(|| "flow-artifacts-copy".into())
            .a11y_button("Copy the document")
            .flex_shrink_0()
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(26.))
            .px(px(8.))
            .rounded(px(6.))
            .text_size(px(theme::font_control()))
            .text_color(theme::accent_ink())
            .hover(|style| style.bg(theme::raised()))
            .on_click(cx.listener(|this, _, _, cx| this.copy(cx)))
            .child(icon(
                "icons/icon-copy.svg",
                px(theme::icon_small()),
                theme::accent_ink(),
            ))
            .child("Copy")
            .into_any_element()
    }

    /// The line under the head: the draft, the write, the save note, the
    /// refusal, and the read-only reason when there is one. `None` when the
    /// line would be empty, so the document starts directly under the head.
    fn render_actions(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let mut row = div()
            .id("flow-artifacts-actions")
            .debug_selector(|| "flow-artifacts-actions".into())
            .flex_shrink_0()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(8.))
            .min_h(px(26.))
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .pt(px(8.));
        let mut any = false;
        if self.dirty(cx)
            && let Some(path) = self.tabs.get(self.active).map(|tab| tab.path.clone())
        {
            any = true;
            row = row.child(
                div()
                    .debug_selector(|| "flow-artifacts-dirty".into())
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child(format!("unsaved · {}", file_name(&path))),
            );
            row = row.child(
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
            any = true;
            row = row.child(
                div()
                    .debug_selector(|| "flow-artifacts-status".into())
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::ADD)
                    .child(status.clone()),
            );
        }
        if let Some(error) = &self.error {
            any = true;
            row = row.child(
                div()
                    .debug_selector(|| "flow-artifacts-error".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::DANGER)
                    .child(error.clone()),
            );
        }
        if let Some(note) = self.lock_note() {
            any = true;
            row = row.child(
                div()
                    .debug_selector(|| "flow-artifacts-lock".into())
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .child(note),
            );
        }
        if self.loading {
            any = true;
            row = row.child(
                div()
                    .debug_selector(|| "flow-artifacts-loading".into())
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(motion::spinner(
                        "flow-artifacts-spinner",
                        "icons/icon-spinner.svg",
                        px(theme::icon_tiny()),
                        theme::accent_ink(),
                    ))
                    .child(
                        div()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .child("Reading the issue folder…"),
                    ),
            );
        }
        any.then(|| row.into_any_element())
    }

    /// Why the open document cannot be edited right now, in one line.
    fn lock_note(&self) -> Option<&'static str> {
        let tab = self.tabs.get(self.active)?;
        let driving = self.run.as_ref().is_some_and(|run| run.driving);
        if tab.editable && driving {
            Some(
                "Read-only: Warp Flow is driving this run; its folder is the engine's until it stops at a step boundary.",
            )
        } else {
            tab.lock_note
        }
    }

    /// The document body: the editor for a person's file while the engine is
    /// idle, the kit's read-only markdown for the engine's, and monospace for a
    /// file that is not prose.
    fn render_body(
        &mut self,
        tab: &Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editable = tab.editable && !self.run.as_ref().is_some_and(|run| run.driving);
        let path = tab.path.clone();
        let editor = self.ensure_editor(window, cx);
        let content = if tab.editable || is_markdown(&path) {
            if editable {
                match editor.as_ref() {
                    Some(editor) => LiveEditor::new(editor)
                        .aria_label(SharedString::from(format!("{} contents", file_name(&path))))
                        .text_size(px(theme::font_body()))
                        .surface(theme::surface())
                        .into_any_element(),
                    None => note_line("There is nothing to read yet."),
                }
            } else {
                match editor.as_ref() {
                    Some(editor) => warpui_kit_markdown::render_read_only_document(editor, cx),
                    None => note_line("There is nothing to read yet."),
                }
            }
        } else {
            // a `.toml` document is not prose: monospace, as it is on disk
            div()
                .w_full()
                .rounded(px(6.))
                .border_1()
                .border_color(theme::hairline())
                .bg(theme::well())
                .p(px(12.))
                .font_family(theme::code_font())
                .text_size(px(theme::font_mono_meta()))
                .text_color(theme::text_dim())
                .child(
                    editor
                        .as_ref()
                        .map(|editor| editor.read(cx).value().to_string())
                        .unwrap_or_default(),
                )
                .into_any_element()
        };
        // The editable editor fills a sized parent and scrolls its own text
        // (the kit's editor is a `size_full` surface); a read-only document is
        // content-sized and the body's scroller moves it. So the 760 measure
        // wrapper takes the height only in the editable case.
        let measure = div()
            .debug_selector(|| "flow-artifacts-measure".into())
            .w_full()
            .max_w(px(theme::scaled(shell::READING_WIDTH)));
        let measure = if editable {
            measure.h_full().child(content)
        } else {
            measure.flex().flex_col().child(content)
        };
        div()
            .id("flow-artifacts-body")
            .debug_selector(|| "flow-artifacts-body".into())
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(theme::scaled(20.)))
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .pb(px(theme::scaled(28.)))
            .child(measure)
            .into_any_element()
    }

    /// The no-file state: what is true and what writes the folder next.
    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut column = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.))
            .max_w(px(theme::scaled(440.)))
            .child(icon(
                "icons/icon-file.svg",
                px(theme::scaled(28.)),
                theme::text_muted(),
            ))
            .child(
                div()
                    .text_size(px(theme::font_heading()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child("No artifacts yet"),
            )
            .child(
                div()
                    .text_center()
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .child(
                        "The engine writes the issue's documents when the run is triaged, then spec.md, \
                         the task files and run.md as steps run. The folder stays the engine's while it \
                         drives.",
                    ),
            );
        if self.loading {
            column = column.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(motion::spinner(
                        "flow-artifacts-empty-spinner",
                        "icons/icon-spinner.svg",
                        px(theme::icon_tiny()),
                        theme::accent_ink(),
                    ))
                    .child(
                        div()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .child("Reading the issue folder…"),
                    ),
            );
        }
        div()
            .id("flow-artifacts-empty")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .p(px(theme::scaled(24.)))
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.focus, cx);
            }))
            .child(column)
            .into_any_element()
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
            return placeholder("artifacts").into_any_element();
        };
        let Some(tab) = self.tabs.get(self.active).map(|tab| Tab {
            path: tab.path.clone(),
            label: tab.label.clone(),
            description: tab.description.clone(),
            editable: tab.editable,
            lock_note: tab.lock_note,
        }) else {
            return div()
                .id("flow-screen-artifacts")
                .debug_selector(|| "flow-screen-artifacts".into())
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .bg(theme::surface())
                .track_focus(&self.focus)
                .children(self.render_actions(cx))
                .child(self.render_empty(cx))
                .into_any_element();
        };
        let rail = self.render_rail(&run, cx);
        let head = self.render_doc_head(&run, &tab, cx);
        let actions = self.render_actions(cx);
        let body = self.render_body(&tab, window, cx);
        div()
            .id("flow-screen-artifacts")
            .debug_selector(|| "flow-screen-artifacts".into())
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .bg(theme::surface())
            .track_focus(&self.focus)
            .child(rail)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .h_full()
                    .child(head)
                    .children(actions)
                    .child(body),
            )
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

/// The files for what the folder holds, in §7's order: the person's documents
/// first, then the engine's record and the graph.
fn tabs_for(read: &ReadFolder, run: Option<&ScreenRun>) -> Vec<Tab> {
    let mut tabs = Vec::new();
    let issue = run.map(|run| run.issue().to_path_buf());
    let tasks = run.map_or(0, |run| run.run.task_rows.len());
    if let Some(issue) = &issue {
        tabs.push(Tab {
            path: issue.join("spec.md"),
            label: "Spec".to_owned(),
            description: "the ticket, as triaged".to_owned(),
            editable: true,
            lock_note: None,
        });
    }
    for path in read.files.keys() {
        match path.file_name().and_then(|name| name.to_str()) {
            Some("tasks.md") => tabs.push(Tab {
                path: path.clone(),
                label: "Tasks".to_owned(),
                description: match tasks {
                    0 => "the task list, in order".to_owned(),
                    1 => "1 task, in order".to_owned(),
                    other => format!("{other} tasks, in order"),
                },
                editable: true,
                lock_note: None,
            }),
            Some(name) if path.parent().is_some_and(|dir| dir.ends_with("issues")) => {
                let number = name.split('-').next().unwrap_or(name);
                tabs.push(Tab {
                    path: path.clone(),
                    label: format!("Task {number}"),
                    description: "the task, as split".to_owned(),
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
            description: "the engine's own record".to_owned(),
            editable: false,
            lock_note: Some(
                "Read-only: `run.md` is the engine's record of what happened; this screen never writes it.",
            ),
        });
        tabs.push(Tab {
            path: issue.join("workflow.toml"),
            label: "Workflow file".to_owned(),
            description: "the revision this run reads".to_owned(),
            editable: false,
            lock_note: Some(
                "Read-only here: `workflow.toml` is edited in Repos › Workflow, which appends the next revision on save.",
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
                description: "the open question".to_owned(),
                editable: false,
                lock_note: Some(
                    "Read-only: a question is answered on the Run tab, which writes the answer the engine reads.",
                ),
            });
        }
    }
    // a spec tab with no file yet is still the first tab: the first save
    // creates it
    tabs
}

/// The folder as the rail's head names it: `.scratch/<run-id>`.
fn folder_label(run: &ScreenRun) -> String {
    let name = run
        .issue()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| run.issue().display().to_string());
    let parent = run
        .issue()
        .parent()
        .and_then(|parent| parent.file_name())
        .map(|name| name.to_string_lossy().into_owned());
    match parent.as_deref() {
        Some(".scratch") => format!(".scratch/{name}"),
        _ => name,
    }
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
