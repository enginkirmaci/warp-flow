//! The Run tab's body: the one card the run's state calls for (a question, a
//! block, a pause, or a run that has not started), the workflow as one
//! tasks-across steps-down matrix, the task list, the attempt history, the live
//! engine log and the harness sessions. The head, the tabs and the 44 strip
//! belong to the detail pane ([`crate::ui::detail`]); this file draws what sits
//! under them.
//!
//! The card order is the design's: state, workflow, tasks, attempts, log. One
//! column at `shell::READING_WIDTH`, every card `surface` on a 1 px hairline,
//! and every spinner paired with the word it stands for.

use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, FontWeight, KeyDownEvent, SharedString, Window, div, px, relative,
};
use warp_flow_core::artifact::Status;
use warp_flow_core::graph::StepKind;
use warp_flow_core::run::DEFAULT_MAX_ATTEMPTS;
use warpui_kit_core::controls::{self, BtnKind};
use warpui_kit_core::tooltip::tooltip_text;
use warpui_kit_core::{A11y, icon, motion, shell};
use warpui_kit_markdown::LiveEditorState;

use crate::host::{AnswerOutcome, Phase, RunSnapshot};
use crate::store::{
    self, LaneView, LogGroup, Question, Run, RunRow, StepState, StepView, TaskRow, outcome_state,
};
use crate::theme;
use crate::ui::actions::{self, Flag, Primary};
use crate::ui::window::{FlowApp, RunTab, attempt_word, ensure_document, tag};

/// The per-run state the Run tab prepares before it draws: the folder the
/// host's verbs take, the key the document slot compares, and the open
/// question. It is copied from the scan (cheap fields, plus the question only
/// while one is open) so the frame can borrow the run from the snapshot and
/// still mutate this window's state, instead of cloning a whole `Run` every
/// frame.
pub(crate) struct RunPrep {
    pub dir: PathBuf,
    pub key: crate::store::RunKey,
    pub question: Option<Question>,
}

impl FlowApp {
    // ---- entry points ------------------------------------------------------

    /// Close the run pane: the same thing `Escape` does, in the one place a
    /// probe can also call.
    pub(crate) fn close_run_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.screen = None;
        self.selected_run = None;
        self.run_ui.reset();
        window.focus(&self.board_focus, cx);
        cx.notify();
    }

    /// The run pane's own keys. Escape goes back; nothing here is a
    /// window-level handler.
    fn handle_run_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" {
            self.close_run_screen(window, cx);
            cx.stop_propagation();
            return;
        }
        if let Some(direction) = warpui_kit_core::pane_nav::direction(event)
            && self
                .pane_nav
                .move_from(&self.run_focus, direction, window, cx)
        {
            cx.stop_propagation();
        }
    }

    /// Write one answer through the host's single writer, then let the next
    /// scan move the run. The engine owns every other transition.
    fn submit_answer(
        &mut self,
        question: Question,
        answer: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = answer.trim().to_owned();
        if answer.is_empty() {
            self.run_ui.answer_error = Some("Pick an option or write an answer.".to_owned());
            cx.notify();
            return;
        }
        let Some(issue) = self.run_ui.issue.clone() else {
            self.run_ui.answer_error =
                Some("The run screen lost its issue folder; reopen the run.".to_owned());
            cx.notify();
            return;
        };
        // The host owns the one file the app writes, so the run screen and the
        // door share a writer and the engine owns every other transition. A
        // run this window was driving to its question continues on the same
        // click: the answer is what the engine waited for. The pick stays in
        // `run_multi` until the scan closes the card, so the picked option
        // keeps its fill for the frame in between.
        match self.host.answer(&issue, &question.path, &answer) {
            Ok(AnswerOutcome::Resumed | AnswerOutcome::Recorded) => {
                self.run_ui.answer_error = None;
                self.answer_field
                    .update(cx, |state, cx| state.set_value("", window, cx));
                cx.notify();
            }
            Err(error) => {
                self.run_ui.answer_error = Some(error);
                cx.notify();
            }
        }
    }

    /// The answer a card's `Answer` sends: the ticked options for a `multi`
    /// question, the written text when there is one, else the picked option.
    fn answer_from_card(
        &mut self,
        question: Question,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let typed = self.answer_field.read(cx).value().trim().to_owned();
        let picked: Vec<String> = self
            .run_ui
            .multi
            .iter()
            .filter(|choice| question.options.iter().any(|option| option == *choice))
            .cloned()
            .collect();
        let answer = if question.kind == "multi" && !picked.is_empty() {
            picked.join(", ")
        } else if !typed.is_empty() {
            typed
        } else if question.kind == "approve" {
            self.run_ui.multi.first().cloned().unwrap_or_default()
        } else {
            picked.first().cloned().unwrap_or_default()
        };
        if answer.trim().is_empty() {
            self.run_ui.answer_error = Some("Pick an option or write an answer.".to_owned());
            cx.notify();
            return;
        }
        self.run_ui.abandon_confirm = None;
        self.submit_answer(question, answer, window, cx);
    }

    // ---- driving the run ---------------------------------------------------

    /// Start a fresh run for the open screen's issue.
    pub(crate) fn start_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.start(&issue) {
            Ok(()) => self.run_ui.action_error = None,
            Err(error) => self.run_ui.action_error = Some(error),
        }
        cx.notify();
    }

    /// Ask the host to pause the open run at its next step boundary.
    pub(crate) fn pause_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.pause(&issue) {
            Ok(()) => self.run_ui.action_error = None,
            Err(error) => self.run_ui.action_error = Some(error),
        }
        cx.notify();
    }

    /// Continue the open run from its record, or take over a run another
    /// process is driving: the same takeover `warp-flow-cli run` does.
    pub(crate) fn resume_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.resume(&issue) {
            Ok(()) => self.run_ui.action_error = None,
            Err(error) => self.run_ui.action_error = Some(error),
        }
        cx.notify();
    }

    /// Retry the step a blocked run stopped on. The host has no separate retry
    /// verb: resuming the record re-enters the blocked step, and the engine
    /// spends a fresh attempt budget there. The blocked card's footer states
    /// that in words so `Retry gate` is not a promise the engine cannot keep.
    pub(crate) fn retry_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.resume(&issue) {
            Ok(()) => self.run_ui.action_error = None,
            Err(error) => self.run_ui.action_error = Some(error),
        }
        cx.notify();
    }

    /// Abandon the open run at its next step boundary, after the confirmation.
    pub(crate) fn abandon_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        self.run_ui.abandon_confirm = None;
        match self.host.abandon(&issue) {
            Ok(()) => self.run_ui.action_error = None,
            Err(error) => self.run_ui.action_error = Some(error),
        }
        cx.notify();
    }

    /// Tick or untick one `multi` choice.
    fn toggle_multi(&mut self, label: String) {
        if let Some(index) = self.run_ui.multi.iter().position(|choice| choice == &label) {
            self.run_ui.multi.remove(index);
        } else {
            self.run_ui.multi.push(label);
        }
    }

    // ---- rendering ---------------------------------------------------------

    /// The Run tab's body: what the engine is doing, what it waits on, and the
    /// attempts and sessions it recorded.
    /// Prepare the Run tab for `prep`: the host's key, the pick reset and the
    /// question's rendered document. Everything here mutates this window's
    /// state, so it runs before the frame borrows the run to draw it.
    pub(crate) fn prepare_run_body(
        &mut self,
        prep: RunPrep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The host keys its runs by the issue folder; every action below uses
        // this one, which the scan resolved before the frame.
        // The card closed, or the pane moved to another run: the pick the last
        // card held has no meaning on the next one. A frame on the same open
        // question keeps its ticks.
        let switched = self.run_ui.issue.as_deref() != Some(prep.dir.as_path());
        if switched || prep.question.is_none() {
            self.run_ui.multi.clear();
        }
        self.run_ui.issue = Some(prep.dir);
        if let Some(question) = &prep.question {
            // Only the body is the editor's source; the path is not content.
            ensure_document(
                &mut self.run_ui.question,
                &prep.key,
                question_markdown(&question.body),
                window,
                cx,
            );
        } else {
            self.run_ui.question = None;
        }
    }

    /// The Run tab's body, borrowed from the snapshot: the scan's reading of
    /// the run, with nothing copied.
    pub(crate) fn render_run_body(
        &self,
        run: &Run,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let host = self.host_snapshot.run(&run.dir).cloned();
        let phase = host.as_ref().map(|host_run| host_run.phase);
        let question = run.question.as_ref();
        let question_doc = self
            .run_ui
            .question
            .as_ref()
            .map(crate::ui::window::DocSlot::editor);

        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(theme::scaled(theme::RUN_GAP)))
            .w_full()
            .max_w(px(theme::scaled(shell::READING_WIDTH)))
            .mx_auto();

        for error in run_error_lines(self.run_ui.action_error.as_deref(), host.as_ref()) {
            column = column.child(action_error_line(&error));
        }

        let issue_on_top =
            question.is_none() && !run.has_run && !is_blocked(run, phase) && !is_paused(run, phase);

        if let Some(card) =
            self.render_state_card(run, host.as_ref(), question, question_doc, window, cx)
        {
            column = column.child(card);
        }

        if !run.steps.is_empty() {
            column =
                column.child(self.render_workflow_card(run, phase, self.reading_width(window)));
        } else {
            if let Some(note) = &run.workflow_note {
                column = column.child(workflow_note_row(note));
            }
            if !issue_on_top {
                column = column.child(self.render_issue_card(run, phase));
            }
        }

        column = column
            .child(self.render_tasks_card(run))
            .child(self.render_attempts_card(run, cx));
        if let Some(log) = self.render_engine_log(run, host.as_ref()) {
            column = column.child(log);
        }
        column = column.child(self.render_sessions_card(run));

        let body = div()
            .id("flow-run-scroll")
            .debug_selector(|| "flow-run-scroll".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.run_scroll)
            .track_focus(&self.run_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_run_key(event, window, cx);
            }))
            .px(px(theme::scaled(shell::DETAIL_PADDING)))
            .pt(px(theme::scaled(shell::DETAIL_PADDING)))
            .pb(px(theme::scaled(shell::DETAIL_PADDING)))
            .child(column);

        div()
            .id("flow-run")
            .debug_selector(|| "flow-run".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::panel())
            .child(body)
            .into_any_element()
    }

    /// How wide the run body actually is, in the live window: the reading
    /// measure when there is room, less when the list and padding take it.
    ///
    /// The matrix used the constant 760 even in a 1100 px window, where the
    /// body is 544: its columns were sized for a measure it did not have, so
    /// the last task columns were clipped instead of fitting.
    fn reading_width(&self, window: &Window) -> f32 {
        let window = f32::from(window.bounds().size.width);
        let rail = theme::scaled(shell::RAIL_WIDTH);
        let list = if self.list_hidden {
            0.
        } else {
            shell::clamp_list_width(self.list_width)
        };
        let padding = 2. * theme::scaled(shell::DETAIL_PADDING);
        (window - rail - list - padding).clamp(320., theme::scaled(shell::READING_WIDTH))
    }

    // ---- the state's one card ---------------------------------------------

    /// The card the run's state calls for, in the design's order: a waiting run
    /// gets its question, a blocked run its block, a paused run its note, and a
    /// run that has never started its issue.
    fn render_state_card(
        &self,
        run: &Run,
        host: Option<&RunSnapshot>,
        question: Option<&Question>,
        question_doc: Option<&Entity<LiveEditorState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let phase = host.map(|host_run| host_run.phase);
        if let Some(question) = question {
            // A terminal run that still has an open question cannot act on the
            // answer: its worker is gone. Drawing an answerable card would ask
            // a person to decide something nothing is waiting for.
            if !run.status.is_terminal() {
                return Some(self.render_question_card(run, question, question_doc, window, cx));
            }
        }
        if is_blocked(run, phase) {
            return Some(self.render_blocked_card(run, host));
        }
        if is_paused(run, phase) {
            return Some(paused_note(host));
        }
        if !run.has_run {
            return Some(self.render_issue_card(run, phase));
        }
        None
    }

    /// The question card: the run's one filled primary lives here, so the head
    /// offers nothing while a question is open.
    fn render_question_card(
        &self,
        run: &Run,
        question: &Question,
        body: Option<&Entity<LiveEditorState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let picked = picked_option(question, &self.run_ui.multi);
        let typed = self.answer_field.read(cx).value().to_string();
        let ticked = self
            .run_ui
            .multi
            .iter()
            .any(|choice| question.options.iter().any(|option| option == choice));
        let answer_enabled = picked.is_some() || ticked || !typed.trim().is_empty();

        let mut card = div()
            .id("flow-run-question-card")
            .debug_selector(|| "flow-run-question".into())
            .flex()
            .flex_col()
            .w_full()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::CHANGES)
            .bg(theme::surface())
            .px(px(14.))
            .py(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon(
                        "icons/icon-shield.svg",
                        px(theme::icon_small()),
                        theme::CHANGES,
                    ))
                    .child(
                        div()
                            .text_size(px(theme::font_body()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .child("The engine is waiting on you"),
                    )
                    .child(tag(question.kind.clone()))
                    .child(div().flex_1())
                    .child(head_mono(run.key.id.clone())),
            )
            .child(
                div()
                    .mt(px(8.))
                    .text_size(px(theme::font_body()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(relative(1.43))
                    .text_color(theme::text())
                    .child(question.title.clone()),
            );

        if let Some(body) = body {
            card = card.child(
                div()
                    .mt(px(6.))
                    .text_size(px(theme::font_meta()))
                    .line_height(relative(1.43))
                    .text_color(theme::text_muted())
                    .child(warpui_kit_core::text_menu::attach(
                        "flow-run-question-menu",
                        warpui_kit_markdown::render_read_only_document_in_card(body, cx),
                        warpui_kit_core::text_menu::focus(cx),
                        Some("Ctrl C".into()),
                    )),
            );
        }

        let mut options = div().mt(px(2.)).flex().flex_col().gap(px(8.)).w_full();
        let mut has_options = false;
        match question.kind.as_str() {
            "approve" => {
                has_options = true;
                for (label, id, choice) in [
                    ("Yes", "flow-answer-yes", "yes"),
                    ("No", "flow-answer-no", "no"),
                ] {
                    let picked_here = self.run_ui.multi.iter().any(|pick| pick == choice);
                    let question = question.clone();
                    let answer = choice.to_owned();
                    let on_click = cx.listener(
                        move |this, _, window: &mut Window, cx: &mut Context<Self>| {
                            this.run_ui.abandon_confirm = None;
                            this.run_ui.multi.clear();
                            this.run_ui.multi.push(answer.clone());
                            this.submit_answer(question.clone(), answer.clone(), window, cx);
                        },
                    );
                    options = options.child(option_row(
                        id.to_owned(),
                        label,
                        picked_here,
                        false,
                        on_click,
                    ));
                }
            }
            "select" if !question.options.is_empty() => {
                has_options = true;
                for (index, option) in question.options.iter().enumerate() {
                    let picked_here = picked == Some(option.as_str());
                    let question = question.clone();
                    let choice = option.clone();
                    let on_click = cx.listener(
                        move |this, _, window: &mut Window, cx: &mut Context<Self>| {
                            this.run_ui.abandon_confirm = None;
                            this.run_ui.multi.clear();
                            this.run_ui.multi.push(choice.clone());
                            this.submit_answer(question.clone(), choice.clone(), window, cx);
                        },
                    );
                    options = options.child(option_row(
                        format!("flow-answer-option-{index}"),
                        option,
                        picked_here,
                        false,
                        on_click,
                    ));
                }
            }
            "multi" if !question.options.is_empty() => {
                has_options = true;
                for (index, option) in question.options.iter().enumerate() {
                    let picked_here = self.run_ui.multi.contains(option);
                    let choice = option.clone();
                    let on_click = cx.listener(
                        move |this, _, _window: &mut Window, cx: &mut Context<Self>| {
                            this.run_ui.abandon_confirm = None;
                            this.toggle_multi(choice.clone());
                            cx.notify();
                        },
                    );
                    options = options.child(option_row(
                        format!("flow-answer-option-{index}"),
                        option,
                        picked_here,
                        true,
                        on_click,
                    ));
                }
            }
            _ => {}
        }
        if has_options {
            card = card.child(options);
        }

        let question_for_answer = question.clone();
        let answer = probed(
            "flow-answer-submit",
            primary_button(
                "flow-answer-submit-button".to_owned(),
                "Answer",
                Some("icons/icon-send.svg"),
                answer_enabled,
                cx.listener(
                    move |this, _, window: &mut Window, cx: &mut Context<Self>| {
                        if answer_enabled {
                            this.answer_from_card(question_for_answer.clone(), window, cx);
                        }
                    },
                ),
            ),
        );

        let armed = self.run_ui.abandon_confirm.as_ref() == Some(&run.key);
        let key = run.key.clone();
        let issue = run.dir.clone();
        let abandon = ghost_button(
            "flow-run-card-abandon".to_owned(),
            if armed {
                "Click again to abandon"
            } else {
                "Abandon"
            },
            Some("icons/icon-close.svg"),
            armed,
            cx.listener(move |this, _, _, cx| {
                if this.run_ui.abandon_confirm.as_ref() == Some(&key) {
                    this.abandon_run(issue.clone(), cx);
                } else {
                    this.run_ui.abandon_confirm = Some(key.clone());
                    cx.notify();
                }
            }),
        );

        card = card.child(
            div()
                .mt(px(10.))
                .flex()
                .items_end()
                .gap(px(8.))
                .child(div().flex_1().min_w_0().child(warpui_kit_core::field::text(
                    "flow-answer-field",
                    &self.answer_field,
                    window,
                    cx,
                )))
                .child(answer)
                .child(abandon),
        );
        if let Some(error) = &self.run_ui.answer_error {
            card = card.child(
                div()
                    .debug_selector(|| "flow-answer-error".into())
                    .mt(px(8.))
                    .text_size(px(theme::font_sub()))
                    .text_color(theme::DANGER)
                    .child(error.clone()),
            );
        }
        card.child(
            div()
                .mt(px(10.))
                .text_size(px(theme::font_sub()))
                .line_height(relative(1.43))
                .text_color(theme::text_muted())
                .child(
                    "Answering resumes the run at this step's boundary; nothing else moves until \
                     then.",
                ),
        )
        .into_any_element()
    }

    /// The blocked card: the failing step, why it stopped, the last failing
    /// line and the budget the retry spends. Its controls are the head's
    /// primary (a gate's Retry gate, a harness step's Retry step) and Abandon.
    fn render_blocked_card(&self, run: &Run, host: Option<&RunSnapshot>) -> AnyElement {
        let failed = actions::failed_step(run);
        let head_right = failed.map(step_label).unwrap_or_else(|| run.key.id.clone());
        let headline = match failed {
            Some(step) => format!("The {} failed and the run cannot advance", step_label(step)),
            None => "The run is blocked and cannot advance".to_owned(),
        };
        let why = match failed {
            Some(step) => {
                let mut why = format!(
                    "The run stopped at the {} step. The engine's last failing line is below; \
                     the repair file in the issue folder has the detail.",
                    step.id
                );
                if let Some(note) = &run.stop_reason {
                    why.push(' ');
                    why.push_str(note);
                }
                why
            }
            None => {
                let mut why = "The run stopped with no failing step in its record.".to_owned();
                if let Some(note) = &run.stop_reason {
                    why.push(' ');
                    why.push_str(note);
                }
                why
            }
        };
        let spent = failed
            .map(|step| {
                run.rows
                    .iter()
                    .filter(|row| row.step == step.id)
                    .count()
                    .min(DEFAULT_MAX_ATTEMPTS as usize)
            })
            .unwrap_or(0);
        let budget = DEFAULT_MAX_ATTEMPTS;

        let mut card = div()
            .id("flow-run-blocked-card")
            .debug_selector(|| "flow-run-blocked".into())
            .flex()
            .flex_col()
            .w_full()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::DANGER)
            .bg(theme::surface())
            .px(px(14.))
            .py(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(icon(
                        "icons/icon-close.svg",
                        px(theme::icon_small()),
                        theme::DANGER,
                    ))
                    .child(
                        div()
                            .text_size(px(theme::font_body()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::DANGER)
                            .child("Blocked"),
                    )
                    .child(div().flex_1())
                    .child(head_mono(head_right)),
            )
            .child(
                div()
                    .mt(px(8.))
                    .text_size(px(theme::font_body()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(relative(1.43))
                    .text_color(theme::text())
                    .child(headline),
            )
            .child(
                div()
                    .mt(px(6.))
                    .text_size(px(theme::font_sub()))
                    .line_height(relative(1.43))
                    .text_color(theme::text_muted())
                    .child(why),
            );

        // A run that stopped without a failing step has no repair to quote:
        // its reason is the sentence the engine wrote into `run.md`, and it is
        // the only one that is still true after the window restarts. The live
        // log is the fallback, never the lead: its newest failure-shaped line
        // can be an old attempt's, not this stop's.
        let line = if failed.is_none() {
            run.stop_reason
                .clone()
                .or_else(|| host_failure_line(host))
                .or_else(|| failure_line(run, None))
        } else {
            host_failure_line(host).or_else(|| failure_line(run, failed))
        };
        if let Some(line) = line {
            card = card.child(
                div()
                    .mt(px(10.))
                    .rounded(px(6.))
                    .bg(theme::well())
                    .px(px(16.))
                    .py(px(10.))
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .line_height(relative(1.6))
                    .text_color(theme::text_dim())
                    .child(line),
            );
        }

        // The footer names the same verb the head shows: a harness block must
        // not read "Retry gate", and a run with no failing step continues.
        let retry_word = actions::plan(run, host.map(|host| host.phase))
            .primary
            .map_or("Resume", Primary::word);
        let footer = match failed {
            Some(_) => mono_sentence(
                &format!(
                    "{retry_word} starts the step again with a fresh {budget}-attempt budget \
                     ({spent} recorded); Abandon writes "
                ),
                "status: abandoned",
                " without running it.",
            ),
            None => mono_sentence(
                &format!("{retry_word} continues the run from its record; Abandon writes "),
                "status: abandoned",
                " without running it.",
            ),
        };
        card.child(div().mt(px(10.)).child(footer))
            .into_any_element()
    }

    /// "The issue" card, where the design puts it: instead of the workflow card
    /// for a run with no steps, and above it for a run that has not started.
    fn render_issue_card(&self, run: &Run, phase: Option<Phase>) -> AnyElement {
        let mut trailing = Vec::new();
        if let Some(busy) = phase.and_then(|phase| busy_word("flow-run-issue-spinner", phase)) {
            trailing.push(busy);
        }
        trailing.push(quiet_pill("icons/icon-box.svg", run.status.as_str()));

        let card = card()
            .id("flow-run-issue-card")
            .debug_selector(|| "flow-run-issue".into())
            .child(card_head_path(
                "The issue",
                head_mono(format!(".scratch/{}/spec.md", run.key.id)),
                trailing,
            ));
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .px(px(16.))
            .pt(px(8.))
            .pb(px(12.));
        if let Some(paragraph) = first_paragraph(&run.spec) {
            body = body.child(
                div()
                    .text_size(px(theme::font_body()))
                    .line_height(relative(1.43))
                    .text_color(theme::text_dim())
                    .child(paragraph),
            );
        }
        if run.has_run {
            body = body.child(
                div()
                    .text_size(px(theme::font_sub()))
                    .line_height(relative(1.43))
                    .text_color(theme::text_muted())
                    .child("The run's record names no steps yet."),
            );
        } else {
            body = body.child(mono_sentence(
                "No step has run. Starting writes ",
                "run.md",
                " and the first band here; the queue also picks a ready run up on its own.",
            ));
        }
        card.child(body).into_any_element()
    }

    // ---- the workflow card -------------------------------------------------

    /// The workflow as one matrix: the run's tasks across as columns, its steps
    /// down as rows, and one compact state cell where a `per_task` step meets a
    /// task. A step that runs once carries its single pill instead of cells.
    fn render_workflow_card(&self, run: &Run, phase: Option<Phase>, available: f32) -> AnyElement {
        let columns = matrix_columns(run);
        let column_width = matrix_column_width(columns.len(), available);
        let mut head = Vec::new();
        if let Some(busy) = phase.and_then(|phase| busy_word("flow-run-workflow-spinner", phase)) {
            head.push(busy);
        }
        let mut meta = div().flex().items_center().gap(px(6.));
        if !run.workflow.trim().is_empty() && run.workflow.trim() != "—" {
            meta = meta.child(head_mono(run.workflow.clone())).child(dot());
        }
        meta = meta.child(head_word(count_word(run.steps.len(), "step")));
        if !columns.is_empty() {
            meta = meta
                .child(dot())
                .child(head_word(count_word(columns.len(), "task")));
        }
        head.push(meta.into_any_element());

        let mut card = card()
            .id("flow-run-workflow-card")
            .debug_selector(|| "flow-run-workflow".into())
            .child(card_head("Workflow", head))
            // The matrix keeps a readable minimum per task column; when the
            // reading column cannot hold them all, it scrolls sideways rather
            // than clipping the last tasks off the card (a run with twenty
            // tasks used to lose seven of them).
            .child(
                div()
                    .id("flow-run-matrix-scroll")
                    .debug_selector(|| "flow-run-matrix-scroll".into())
                    .w_full()
                    .min_w_0()
                    .overflow_x_scroll()
                    .track_scroll(&self.matrix_scroll)
                    .child(
                        div()
                            .min_w(px(matrix_width(columns.len(), column_width)))
                            .w_full()
                            .child(step_matrix(run, &columns, column_width)),
                    ),
            );
        if let Some(note) = &run.workflow_note {
            card = card.child(workflow_note_row(note));
        }
        card.into_any_element()
    }

    // ---- the tasks card ----------------------------------------------------

    fn render_tasks_card(&self, run: &Run) -> AnyElement {
        let card = card()
            .id("flow-tasks-card")
            .debug_selector(|| "flow-tasks".into())
            .child(card_head(
                "Tasks",
                vec![head_word(count_word(run.task_rows.len(), "task"))],
            ));

        if run.task_levels.is_empty() {
            let any_per_task = run.steps.iter().any(|step| step.per_task);
            let mut note = div()
                .px(px(16.))
                .pt(px(8.))
                .pb(px(12.))
                .text_size(px(theme::font_sub()))
                .line_height(relative(1.43))
                .text_color(theme::text_muted());
            if any_per_task {
                note = note.child(mono_sentence(
                    "The plan step writes the tasks. Until ",
                    "tasks.md",
                    " exists there is nothing to track here.",
                ));
            } else {
                note = note.child(
                    "This workflow runs each step once, at the start: it has no per-task steps, \
                     so the run carries no task list.",
                );
            }
            return card.child(note).into_any_element();
        }

        let has_blockers = run.task_rows.iter().any(|task| !task.blocked_by.is_empty());
        let mut levels = div()
            .debug_selector(|| "flow-run-task-strip".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.));
        for (level_index, level) in run.task_levels.iter().enumerate() {
            if level_index > 0 {
                let selector = format!("flow-run-arrow-{level_index}");
                levels = levels.child(
                    div()
                        .id(SharedString::from(selector.clone()))
                        .debug_selector(move || selector.clone())
                        .flex_shrink_0()
                        .child(icon(
                            "icons/icon-arrow-right.svg",
                            px(theme::icon_small()),
                            theme::text_muted(),
                        )),
                );
            }
            let level_selector = format!("flow-run-level-{level_index}");
            let mut group = div()
                .id(SharedString::from(level_selector.clone()))
                .debug_selector(move || level_selector.clone())
                .flex()
                .flex_wrap()
                .items_center()
                .min_w_0()
                .gap(px(6.));
            for (index, number) in level.iter().enumerate() {
                if index > 0 {
                    let selector = format!("flow-run-parallel-{level_index}");
                    group = group.child(
                        div()
                            .debug_selector(move || selector.clone())
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::line_strong())
                            .child("∥"),
                    );
                }
                if let Some(task) = run.task_rows.iter().find(|task| task.number == *number) {
                    group = group.child(task_pair(task));
                } else {
                    group = group.child(task_pair_number(*number));
                }
            }
            levels = levels.child(group);
        }

        let rule = if has_blockers {
            "A blocked task starts only after its blocker has landed. Each step runs once per \
             task, so the merge queue never holds two merges."
        } else {
            "Every task is independent."
        };
        card.child(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .px(px(16.))
                .pt(px(8.))
                .pb(px(12.))
                .child(levels)
                .child(
                    div()
                        .debug_selector(|| "flow-run-task-rule".into())
                        .text_size(px(theme::font_sub()))
                        .line_height(relative(1.43))
                        .text_color(theme::text_muted())
                        .child(rule),
                ),
        )
        .into_any_element()
    }

    // ---- attempt history ---------------------------------------------------

    fn render_attempts_card(&self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let groups = store::group_log(&run.rows);
        let mut rows = div()
            .debug_selector(|| "flow-run-log".into())
            .flex()
            .flex_col();
        if groups.is_empty() {
            rows = rows.child(
                div()
                    .px(px(16.))
                    .py(px(10.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child("No attempts recorded yet."),
            );
        }
        let mut first = true;
        for (group_index, group) in groups.iter().enumerate() {
            let selector = format!("flow-run-log-group-{group_index}");
            let mut block = div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .flex_col();
            for (row_index, row) in group.rows.iter().enumerate() {
                block = block.child(self.attempt_row(
                    run,
                    group,
                    row,
                    group_index,
                    row_index,
                    first,
                    cx,
                ));
                first = false;
            }
            rows = rows.child(block);
        }

        card()
            .id("flow-run-attempts-card")
            .debug_selector(|| "flow-run-attempts".into())
            .child(card_head(
                "Attempt history",
                vec![head_word(attempt_word(run.attempts))],
            ))
            .child(rows)
            .into_any_element()
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one attempt row needs its group and row index for the selectors, and whether a rule sits above it"
    )]
    fn attempt_row(
        &self,
        run: &Run,
        group: &LogGroup,
        row: &RunRow,
        group_index: usize,
        row_index: usize,
        first: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selector = format!("flow-run-log-row-{group_index}-{row_index}");
        let chip_selector = format!("flow-run-attempt-chip-{group_index}-{row_index}");
        let attempt = row.attempt.parse::<u32>().unwrap_or(0);
        let budget = attempt.max(DEFAULT_MAX_ATTEMPTS).max(1);
        let chip = group
            .task
            .parse::<u32>()
            .map(|number| format!("{number:02}"))
            .unwrap_or_else(|_| row.attempt.clone());
        let title = attempt_title(group);
        let outcome = outcome_state(&row.outcome);
        let duration = attempt_duration(run, group, row);

        let mut meta = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(5.))
            .text_size(px(theme::font_meta()))
            .text_color(theme::text_dim());
        if outcome == StepState::Running {
            meta = meta.child(motion::spinner(
                SharedString::from(format!(
                    "flow-run-attempt-spinner-{group_index}-{row_index}"
                )),
                "icons/icon-spinner.svg",
                px(theme::icon_tiny()),
                theme::BEHIND,
            ));
        }
        meta = meta.child(format!(
            "attempt {} of {budget} · {}",
            row.attempt, row.outcome
        ));

        let view_log = div()
            .id(SharedString::from(format!(
                "flow-run-view-log-{group_index}-{row_index}"
            )))
            .debug_selector({
                let selector = format!("flow-run-view-log-{group_index}-{row_index}");
                move || selector.clone()
            })
            .flex_shrink_0()
            .cursor_pointer()
            .text_size(px(theme::font_control()))
            .text_color(theme::accent_ink())
            .hover(|style| style.text_color(theme::text()))
            .tooltip(tooltip_text("Open the run record"))
            .a11y_button("View log")
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_run_tab(RunTab::Artifacts, window, cx);
            }))
            .child("View log");

        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap(px(10.))
            .min_h(px(theme::scaled(theme::ATTEMPT_HEIGHT)))
            .px(px(16.))
            .py(px(6.))
            .when(!first, |row| {
                row.border_t_1().border_color(theme::border_light())
            })
            .child(number_chip(Some(chip_selector), chip, theme::text_dim()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::font_body()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child(title),
            )
            .child(meta)
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(theme::text_muted())
                    .child(clock_word(&row.at).unwrap_or_else(|| row.at.clone())),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(theme::text_muted())
                    .child(duration),
            )
            .child(view_log)
            .into_any_element()
    }

    // ---- the engine log ----------------------------------------------------

    /// The engine's own log while this window has the run, newest last, with
    /// the phase in a word so the motion is never the only signal.
    fn render_engine_log(&self, run: &Run, host: Option<&RunSnapshot>) -> Option<AnyElement> {
        let host_run = host?;
        let phase = host_run.phase;
        let color = phase_color(phase);
        let mut phase_el = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(5.))
            .debug_selector(|| "flow-run-engine-phase".into())
            .font_family(theme::code_font())
            .text_size(px(theme::font_mono_meta()))
            .text_color(color);
        if phase_moves(phase) {
            phase_el = phase_el.child(motion::spinner(
                "flow-run-engine-spinner",
                "icons/icon-spinner.svg",
                px(theme::icon_tiny()),
                color,
            ));
        }
        phase_el = phase_el.child(phase_word(phase));

        let mut lines = div()
            .id("flow-run-engine-log-scroll")
            .flex()
            .flex_col()
            .gap(px(1.))
            .max_h(px(240.))
            .overflow_y_scroll()
            .track_scroll(&self.run_log_scroll)
            .font_family(theme::code_font())
            .text_size(px(theme::font_mono_meta()))
            .line_height(relative(1.6))
            .text_color(theme::text_dim());
        if host_run.log.is_empty() {
            lines = lines.child(
                div()
                    .text_color(theme::text_muted())
                    .child("The engine has not written a line yet."),
            );
        }
        for (index, line) in host_run.log.iter().enumerate() {
            let selector = format!("flow-run-engine-line-{index}");
            lines = lines.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .whitespace_normal()
                    .child(line.clone()),
            );
        }

        Some(
            card()
                .id("flow-run-engine-log-card")
                .debug_selector(|| "flow-run-engine-log".into())
                .child(card_head_path(
                    "Engine log",
                    head_mono(format!(".scratch/{}/run.md", run.key.id)),
                    vec![phase_el.into_any_element()],
                ))
                .child(div().bg(theme::well()).px(px(16.)).py(px(10.)).child(lines))
                .into_any_element(),
        )
    }

    // ---- the harness sessions ----------------------------------------------

    /// What the harness sessions said, the newest last. Each is one markdown
    /// file the harness wrote into `sessions/`, shown with the step it belongs
    /// to, in the order it settled.
    fn render_sessions_card(&self, run: &Run) -> AnyElement {
        let mut card = card()
            .id("flow-run-sessions-card")
            .debug_selector(|| "flow-run-sessions".into())
            .child(card_head_path(
                "Sessions",
                head_mono("sessions/".to_owned()),
                vec![head_word(count_word(run.sessions.len(), "session"))],
            ));
        if run.sessions.is_empty() {
            return card
                .child(
                    div()
                        .px(px(16.))
                        .py(px(10.))
                        .text_size(px(theme::font_sub()))
                        .text_color(theme::text_muted())
                        .child(
                            "No session recorded yet. A harness step writes one when it settles.",
                        ),
                )
                .into_any_element();
        }
        for (index, session) in run.sessions.iter().enumerate() {
            let selector = format!("flow-run-session-{index}");
            let color = session_color(&session.status);
            let mut head = div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(16.))
                .pt(px(8.))
                .pb(px(6.))
                .text_size(px(theme::font_row_meta()))
                .text_color(theme::text_muted());
            head = head.child(
                div()
                    .flex_shrink_0()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child(session.step.clone()),
            );
            if let Some(task) = &session.task {
                head = head.child(head_mono(format!("task {task}")));
            }
            head = head
                .child(head_mono(format!("attempt {}", session.attempt)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(theme::code_font())
                        .text_color(theme::text_muted())
                        .child(format!("{} · {}", session.harness, session.session)),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color)
                        .child(if session.status == "running" {
                            motion::spinner(
                                SharedString::from(format!("{selector}-spinner")),
                                "icons/icon-spinner.svg",
                                px(theme::icon_tiny()),
                                color,
                            )
                        } else {
                            icon(
                                state_icon(outcome_state(&session.status)),
                                px(theme::icon_tiny()),
                                color,
                            )
                            .into_any_element()
                        })
                        .child(session.status.clone()),
                );

            // The transcript's tail: a session can be long, and the run tab is
            // a page of cards, not one document.
            let lines: Vec<&str> = session.text.lines().collect();
            let shown: Vec<&str> = lines
                .iter()
                .rev()
                .take(SESSION_LINES)
                .rev()
                .copied()
                .collect();
            let mut text = div()
                .id(SharedString::from(format!("{selector}-text")))
                .debug_selector({
                    let selector = format!("{selector}-text");
                    move || selector.clone()
                })
                .flex()
                .flex_col()
                .gap(px(1.))
                .max_h(px(240.))
                .overflow_y_scroll()
                .mx(px(16.))
                .mb(px(10.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(6.))
                .bg(theme::well())
                .font_family(theme::code_font())
                .text_size(px(theme::font_mono_meta()))
                .line_height(relative(1.6))
                .text_color(theme::text_dim());
            if lines.len() > shown.len() {
                text = text.child(
                    div()
                        .text_color(theme::text_muted())
                        .child(format!("… {} earlier lines", lines.len() - shown.len())),
                );
            }
            for line in shown {
                text = text.child(div().child(line.to_owned()));
            }

            card = card.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .flex()
                    .flex_col()
                    .when(index > 0, |row| {
                        row.border_t_1().border_color(theme::border_light())
                    })
                    .child(head)
                    .child(text),
            );
        }
        card.into_any_element()
    }

    // ---- the head's action row ---------------------------------------------

    /// The run's head actions: whatever [`crate::ui::actions::plan`] says its
    /// state earns, drawn. The decision is not made here, so the head and the
    /// ⋯ menu cannot disagree about a finished run, a blocked harness step or
    /// a run this window is not driving.
    pub(crate) fn render_run_actions(&self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let host_run = self.host_snapshot.run(&run.dir);
        let plan = actions::plan(run, host_run.map(|host_run| host_run.phase));
        let issue = run.dir.clone();

        let mut row = div()
            .id("flow-run-actions")
            .debug_selector(|| "flow-run-actions".into())
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(px(6.));

        if let Some(primary) = plan.primary {
            let click_issue = issue.clone();
            row = row.child(action_button(
                primary.id(),
                primary.word(),
                primary.filled(),
                Some(primary.icon()),
                true,
                plan.primary_reason,
                cx.listener(move |this, _, _, cx| match primary {
                    Primary::Start => this.start_run(click_issue.clone(), cx),
                    Primary::Resume => this.resume_run(click_issue.clone(), cx),
                    Primary::Retry { .. } => this.retry_run(click_issue.clone(), cx),
                }),
            ));
        }
        if let Some(pause) = plan.pause {
            let pause_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-pause",
                "Pause",
                false,
                None,
                pause.enabled,
                pause.reason,
                cx.listener(move |this, _, _, cx| this.pause_run(pause_issue.clone(), cx)),
            ));
        }
        // A question card owns both the run's one primary and its Abandon; two
        // Abandons on one screen are the same act twice.
        if let Some(abandon) = plan.abandon
            && run.question.is_none()
        {
            row = row.child(self.abandon_control(run, abandon, cx));
        }
        row.into_any_element()
    }

    /// Abandon for a run no worker is driving, behind the same one-step
    /// confirmation the driving case uses.
    fn abandon_control(&self, run: &Run, flag: Flag, cx: &mut Context<Self>) -> AnyElement {
        if self.run_ui.abandon_confirm.as_ref() == Some(&run.key) {
            return self.render_abandon_confirm(&run.dir, cx);
        }
        let key = run.key.clone();
        action_button(
            "flow-run-abandon",
            "Abandon",
            false,
            None,
            flag.enabled,
            flag.reason,
            cx.listener(move |this, _, _, cx| {
                this.run_ui.abandon_confirm = Some(key.clone());
                cx.notify();
            }),
        )
    }

    /// The one-step Abandon confirmation: a small inline row, no scrim.
    fn render_abandon_confirm(&self, issue: &Path, cx: &mut Context<Self>) -> AnyElement {
        let yes_issue = issue.to_path_buf();
        div()
            .id("flow-run-abandon-confirm")
            .debug_selector(|| "flow-run-abandon-confirm".into())
            .flex()
            .items_center()
            .gap(px(5.))
            .child(
                div()
                    .text_size(px(theme::font_label()))
                    .text_color(theme::DANGER)
                    .child("Abandon?"),
            )
            .child(action_button(
                "flow-run-abandon-yes",
                "Yes",
                false,
                None,
                true,
                None,
                cx.listener(move |this, _, _, cx| {
                    this.abandon_run(yes_issue.clone(), cx);
                }),
            ))
            .child(action_button(
                "flow-run-abandon-no",
                "No",
                false,
                None,
                true,
                None,
                cx.listener(|this, _, _, cx| {
                    this.run_ui.abandon_confirm = None;
                    cx.notify();
                }),
            ))
            .into_any_element()
    }
}

// ---- the workflow matrix --------------------------------------------------
//
// The workflow reads as one matrix: the run's tasks across as columns, the
// workflow's steps down as rows. A `per_task` step is the row that crosses
// every task, so it holds one small state square per task instead of a lane
// per task — the template's seven per-task steps over five tasks become seven
// rows of five cells instead of thirty-five near-identical rows.

/// One task column of the matrix: the number a lane matches on and the title
/// the header carries (truncated on screen).
struct MatrixColumn {
    number: u32,
    title: String,
}

/// The matrix's compact density: one 24 px square per (step, task) crossing,
/// in a 26 px row, where the old band was 44 px plus a 36 px lane per task.
const MATRIX_CELL: f32 = 24.;
const MATRIX_ROW_HEIGHT: f32 = 26.;
const MATRIX_HEADER_HEIGHT: f32 = 26.;

/// A task column: wide enough for the header's number and a readable title. A
/// run with one or two tasks gets the whole measure; past that the columns
/// shrink to share it, down to [`MATRIX_COLUMN_MIN`].
const MATRIX_COLUMN_MAX: f32 = 120.;
const MATRIX_COLUMN_MIN: f32 = 30.;
/// Below this width the header drops the title and keeps the task number only.
const MATRIX_TITLE_MIN: f32 = 56.;
/// The gap between two matrix columns, in the header and in every row.
const MATRIX_GAP: f32 = 8.;

/// What the reading column keeps for the step's name, kind and detail line
/// before the task columns take their share. The detail is a gate's command or
/// a step's skill, so a label narrower than this reads `skill c…`.
const MATRIX_LABEL_MIN: f32 = 320.;

/// The card row's own horizontal padding, shared by the header and every row.
const MATRIX_SIDE: f32 = 16.;

/// The matrix's columns: the run's tasks in task-number order, plus any lane
/// that names a task the task files do not carry, so no state is dropped.
fn matrix_columns(run: &Run) -> Vec<MatrixColumn> {
    let mut columns: Vec<MatrixColumn> = Vec::new();
    let mut push = |number: u32, title: &str| {
        if !columns.iter().any(|column| column.number == number) {
            columns.push(MatrixColumn {
                number,
                title: title.to_owned(),
            });
        }
    };
    for task in &run.task_rows {
        push(task.number, &task.title);
    }
    for step in run.steps.iter().filter(|step| step.per_task) {
        for lane in &step.lanes {
            push(lane.number, &lane.title);
        }
    }
    columns.sort_by_key(|column| column.number);
    columns
}

/// The width of one task column: the reading column is the measure, so more
/// tasks shrink the columns down to [`MATRIX_COLUMN_MIN`]. Below that the
/// matrix keeps the minimum and scrolls sideways instead of clipping the last
/// columns — a run with twenty-four tasks used to lose eleven of them.
fn matrix_column_width(tasks: usize, available: f32) -> f32 {
    let min = theme::scaled(MATRIX_COLUMN_MIN);
    let max = theme::scaled(MATRIX_COLUMN_MAX);
    if tasks == 0 {
        return max;
    }
    let gap = theme::scaled(MATRIX_GAP);
    // The label and the side insets come out of the measure; each column then
    // carries one gap of its own, which is why it is subtracted after the
    // division. Leaving it out made every full row overhang.
    let room =
        available.max(0.) - theme::scaled(2. * MATRIX_SIDE) - theme::scaled(MATRIX_LABEL_MIN);
    (room / tasks as f32 - gap).clamp(min, max)
}

/// The width the matrix needs for `tasks` columns: its side padding, the step
/// label's readable minimum and one column plus gap per task. When this is
/// wider than the card, that is the horizontal scroller's content width, not a
/// clipping edge.
fn matrix_width(tasks: usize, column_width: f32) -> f32 {
    if tasks == 0 {
        return 0.;
    }
    theme::scaled(2. * MATRIX_SIDE + MATRIX_LABEL_MIN)
        + tasks as f32 * (column_width + theme::scaled(MATRIX_GAP))
}

/// The lane a step holds for one task, when the task files gave the step one.
fn matrix_lane(step: &StepView, number: u32) -> Option<&LaneView> {
    step.lanes.iter().find(|lane| lane.number == number)
}

/// The matrix's header: the axis word, then one column per task carrying the
/// number the run gave it and its title, truncated once.
fn matrix_head(columns: &[MatrixColumn], column_width: f32) -> AnyElement {
    let mut head = div()
        .debug_selector(|| "flow-run-matrix-head".into())
        .flex()
        .items_center()
        .gap(px(theme::scaled(MATRIX_GAP)))
        .min_h(px(theme::scaled(MATRIX_HEADER_HEIGHT)))
        .px(px(theme::scaled(MATRIX_SIDE)))
        .border_b_1()
        .border_color(theme::border_light())
        .child(
            div()
                .flex_shrink_0()
                .w(px(theme::scaled(MATRIX_LABEL_MIN)))
                .min_w_0()
                .text_size(px(theme::font_label()))
                .text_color(theme::text_muted())
                .child("Step"),
        );
    let titled = column_width >= theme::scaled(MATRIX_TITLE_MIN);
    for column in columns {
        let selector = format!("flow-run-matrix-task-{:02}", column.number);
        let mut cell = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex_shrink_0()
            .w(px(column_width))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(5.))
            .tooltip(tooltip_text(column.title.clone()))
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(theme::text_muted())
                    .child(format!("{:02}", column.number)),
            );
        if titled {
            cell = cell.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::font_row_meta()))
                    .text_color(theme::text_dim())
                    .child(column.title.clone()),
            );
        }
        head = head.child(cell);
    }
    head.into_any_element()
}

/// One step row's left side: the kind glyph, the step's name, its kind word,
/// the marker that says whether it runs once or per task, and the detail line
/// that says what the step does.
fn matrix_step_label(step: &StepView, detail: &str, tasks: usize) -> AnyElement {
    let phrase = match step.kind {
        Some(StepKind::Merge) => "the merge queue",
        Some(StepKind::Cleanup) => "the run's cleanup",
        _ => kind_word(step.kind),
    };
    // A per-task row is marked with the count it crosses, so the row says what
    // the columns are for without a word the narrow reading column cannot fit.
    let marker = match (step.per_task, tasks) {
        (true, 0) => "per task".to_owned(),
        (true, tasks) => format!("×{tasks}"),
        (false, _) => "once".to_owned(),
    };
    // The detail truncates at a narrow measure; the hover text keeps the whole
    // sentence reachable, the way the editor's path line does.
    let full = if detail.is_empty() {
        format!("{} {} · {}", step.id, phrase, marker)
    } else {
        format!("{} {} · {} · {}", step.id, phrase, marker, detail)
    };
    let mut label = div()
        .id(SharedString::from(format!(
            "flow-run-step-label-{}",
            step.id
        )))
        .tooltip(tooltip_text(full))
        // A fixed label column keeps a one-task matrix beside the steps it
        // names instead of parking that task at the card's right edge; when
        // there are more tasks than fit, the label is what the row keeps.
        .flex_shrink_0()
        .w(px(theme::scaled(MATRIX_LABEL_MIN)))
        .min_w_0()
        .overflow_hidden()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .w(px(18.))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .child(icon(
                    kind_icon(step.kind),
                    px(theme::icon_small()),
                    theme::text_muted(),
                )),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(theme::font_name()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(step.id.clone()),
        )
        .child(
            div()
                // The kind word is a category, not an example: when the row is
                // tight the detail below is what gives way, so a row never
                // reads `review har…` or `fmt … ×5`.
                .flex_shrink_0()
                .text_size(px(theme::font_label()))
                .text_color(theme::text_muted())
                .child(phrase),
        )
        .child(
            div()
                .flex_shrink_0()
                .font_family(theme::code_font())
                .text_size(px(theme::font_mono_meta()))
                .text_color(theme::text_muted())
                .child(marker),
        );
    if !detail.is_empty()
        && step.kind != Some(StepKind::Merge)
        && step.kind != Some(StepKind::Cleanup)
    {
        label = label
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child("·"),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_mono_meta()))
                    .text_color(theme::text_dim())
                    .child(detail.to_owned()),
            );
    }
    label.into_any_element()
}

/// One crossing of a `per_task` step and a task: a small square carrying the
/// state's colour and icon, with the state word as its accessibility label. An
/// idle crossing is a quiet well, not a hole; a task the step has no lane for
/// is the same quiet square with a dash.
fn matrix_cell(step: &StepView, column: &MatrixColumn, column_width: f32) -> AnyElement {
    let selector = format!("flow-run-lane-{}-{:02}", step.id, column.number);
    let state_selector = format!("flow-run-state-{}-{:02}", step.id, column.number);
    let state = matrix_lane(step, column.number).map(|lane| lane.state);
    let color = state.map_or_else(theme::text_muted, state_color_public);
    let quiet = matches!(state, None | Some(StepState::Idle));
    let mut square = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .role(gpui::Role::Status)
        .aria_label(format!(
            "{} task {:02}: {}",
            step.id,
            column.number,
            state.map_or("no lane", StepState::label)
        ))
        .flex()
        .items_center()
        .justify_center()
        .size(px(theme::scaled(MATRIX_CELL)))
        .rounded(px(6.));
    // Every square carries its outline, so a tinted cell and a quiet one read
    // as the same grid; the hue and the glyph are what say which state it is.
    square = square.border_1().border_color(theme::hairline());
    if quiet {
        square = square.bg(theme::well());
    } else {
        square = square.bg(theme::tint(color));
    }
    let inner = match state {
        Some(StepState::Running) => div()
            .id(SharedString::from(format!("{state_selector}-icon")))
            .debug_selector({
                let selector = state_selector.clone();
                move || selector.clone()
            })
            .flex()
            .items_center()
            .justify_center()
            .child(motion::spinner(
                format!("{state_selector}-spinner"),
                state_icon(StepState::Running),
                px(theme::icon_tiny()),
                color,
            ))
            .into_any_element(),
        Some(state) => div()
            .id(SharedString::from(format!("{state_selector}-icon")))
            .debug_selector({
                let selector = state_selector.clone();
                move || selector.clone()
            })
            .flex()
            .items_center()
            .justify_center()
            .child(icon(state_icon(state), px(theme::icon_tiny()), color))
            .into_any_element(),
        None => div()
            .id(SharedString::from(format!("{state_selector}-none")))
            .debug_selector(move || state_selector.clone())
            .text_size(px(theme::font_mono_meta()))
            .text_color(theme::text_muted())
            .child("–")
            .into_any_element(),
    };
    div()
        .flex_shrink_0()
        .w(px(column_width))
        .flex()
        .items_center()
        .justify_center()
        .child(square.child(inner))
        .into_any_element()
}

/// One step's matrix row: its label, then either the pill a step that runs once
/// carries or one cell for every task a `per_task` step runs for.
fn matrix_step_row(
    step: &StepView,
    columns: &[MatrixColumn],
    column_width: f32,
    separated: bool,
) -> AnyElement {
    let selector = format!("flow-run-step-{}", step.id);
    let state_selector = format!("flow-run-state-{}", step.id);
    // The design reads `gate · test` and `harness · skill analyze ·
    // produces spec.md`: a gate keeps its own command; a harness step keeps its
    // skill and output names.
    let detail = match step.kind {
        Some(StepKind::Gate) => step
            .detail
            .strip_prefix("gate: ")
            .unwrap_or(&step.detail)
            .to_owned(),
        _ => step.detail.replace(": ", " "),
    };
    let mut row = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap(px(theme::scaled(MATRIX_GAP)))
        .min_h(px(theme::scaled(MATRIX_ROW_HEIGHT)))
        .px(px(theme::scaled(MATRIX_SIDE)))
        .when(separated, |row| {
            row.border_t_1().border_color(theme::border_light())
        })
        .child(matrix_step_label(step, &detail, columns.len()));
    if step.per_task {
        for column in columns {
            row = row.child(matrix_cell(step, column, column_width));
        }
    } else if step.state == StepState::Idle {
        // A step that runs once has no cells: its word sits at the row's end,
        // over the task columns the per-task rows carry.
        row = row.child(div().flex_1()).child(
            div()
                .id(SharedString::from(format!("{state_selector}-idle")))
                .debug_selector({
                    let selector = state_selector.clone();
                    move || selector.clone()
                })
                .flex_shrink_0()
                .text_size(px(theme::font_label()))
                .text_color(theme::text_muted())
                .child("not run yet"),
        );
    } else {
        row = row
            .child(div().flex_1())
            .child(state_pill(state_selector, step.state));
    }
    row.into_any_element()
}

/// The workflow's matrix: the task header over one row per step. With no task
/// files there are no columns to draw; the one note under the rows says why the
/// per-task rows carry no cells.
fn step_matrix(run: &Run, columns: &[MatrixColumn], column_width: f32) -> AnyElement {
    let mut matrix = div()
        .debug_selector(|| "flow-run-steps".into())
        .flex()
        .flex_col()
        .w_full();
    if !columns.is_empty() {
        matrix = matrix.child(matrix_head(columns, column_width));
    }
    for (index, step) in run.steps.iter().enumerate() {
        matrix = matrix.child(matrix_step_row(step, columns, column_width, index > 0));
    }
    if columns.is_empty() && run.steps.iter().any(|step| step.per_task) {
        matrix = matrix.child(
            div()
                .debug_selector(|| "flow-run-matrix-note".into())
                .px(px(theme::scaled(MATRIX_SIDE)))
                .py(px(8.))
                .text_size(px(theme::font_sub()))
                .text_color(theme::text_muted())
                .child("No task files yet."),
        );
    }
    matrix.into_any_element()
}

// ---- small rendering helpers ----------------------------------------------

/// How many lines of a session's transcript the run tab shows: the tail is what
/// a person reads, and the card must not become the page.
const SESSION_LINES: usize = 40;

/// A card: the design's base surface, 1 px hairline, 8 px corners.
fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(px(8.))
        .border_1()
        .border_color(theme::hairline())
        .bg(theme::surface())
        .overflow_hidden()
}

/// A card's 44-high head: the section word at 16/600, a grow, then what the
/// caller puts on the right.
fn card_head(title: &str, trailing: Vec<AnyElement>) -> AnyElement {
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(theme::scaled(44.)))
        .px(px(16.))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(theme::font_heading()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.to_owned()),
        )
        .child(div().flex_1());
    for element in trailing {
        row = row.child(element);
    }
    row.into_any_element()
}

/// A card head whose machine word sits right after the title, then the grow,
/// then the caller's trailing items: the engine log, the sessions and the issue
/// card all read their mono path as part of the heading, not as a right-aligned
/// count.
fn card_head_path(title: &str, path: AnyElement, trailing: Vec<AnyElement>) -> AnyElement {
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(theme::scaled(44.)))
        .px(px(16.))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(theme::font_heading()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.to_owned()),
        )
        .child(path)
        .child(div().flex_1());
    for element in trailing {
        row = row.child(element);
    }
    row.into_any_element()
}

/// A trailing count or word at the label step, `text_dim`.
fn head_word(text: impl Into<String>) -> AnyElement {
    div()
        .flex_shrink_0()
        .text_size(px(theme::font_label()))
        .text_color(theme::text_dim())
        .child(text.into())
        .into_any_element()
}

/// A mono path or id at 13, `text_muted`.
fn head_mono(text: impl Into<String>) -> AnyElement {
    div()
        .min_w_0()
        .max_w(px(theme::scaled(320.)))
        .truncate()
        .font_family(theme::code_font())
        .text_size(px(theme::font_mono_meta()))
        .text_color(theme::text_muted())
        .child(text.into())
        .into_any_element()
}

/// The `·` between two words in a card head.
fn dot() -> AnyElement {
    div()
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_muted())
        .child("·")
        .into_any_element()
}

/// A quiet status pill: a glyph and a word on `raised`.
fn quiet_pill(icon_path: &str, word: &str) -> AnyElement {
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(theme::scaled(theme::PILL_HEIGHT)))
        .px(px(8.))
        .rounded(px(999.))
        .bg(theme::raised())
        .text_size(px(theme::font_meta()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_muted())
        .child(icon(icon_path, px(theme::icon_tiny()), theme::text_muted()))
        .child(word.to_owned())
        .into_any_element()
}

/// A running step's spinner and the word beside it: motion never stands alone.
fn busy_word(id: &'static str, phase: Phase) -> Option<AnyElement> {
    if !phase_moves(phase) {
        return None;
    }
    let color = theme::BEHIND;
    Some(
        div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(px(5.))
            .text_size(px(theme::font_meta()))
            .font_weight(FontWeight::MEDIUM)
            .text_color(color)
            .child(motion::spinner(
                id,
                "icons/icon-spinner.svg",
                px(theme::icon_tiny()),
                color,
            ))
            .child(phase_word(phase).to_owned())
            .into_any_element(),
    )
}

/// One paragraph from a spec body: headings, rules and table rows are skipped.
fn first_paragraph(body: &str) -> Option<String> {
    let mut paragraph = String::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if line.starts_with('#')
            || line.starts_with('|')
            || line.starts_with("```")
            || line.starts_with("---")
        {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(line);
    }
    (!paragraph.is_empty()).then_some(paragraph)
}

/// The question's markdown without its leading `# ` heading: the card already
/// shows that heading at 14/600, so rendering it again from the document would
/// duplicate the question's title.
fn question_markdown(body: &str) -> String {
    let mut out = String::new();
    let mut seen_content = false;
    for line in body.lines() {
        if !seen_content {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            seen_content = true;
            if trimmed.starts_with("# ") {
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `No step has run. Starting writes <run.md> and …`, with the one machine word
/// in the mono face.
fn mono_sentence(lead: &str, mono: &str, tail: &str) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .text_size(px(theme::font_meta()))
        .line_height(relative(1.43))
        .text_color(theme::text_muted())
        .child(lead.to_owned())
        .child(
            div()
                .font_family(theme::code_font())
                .text_color(theme::text_dim())
                .child(mono.to_owned()),
        )
        .child(tail.to_owned())
        .into_any_element()
}

/// One full-width option in the question card. The picked row takes the accent
/// edge and wash and a leading check; every row keeps the 16 px slot so the
/// text cannot move when the pick changes.
fn option_row(
    selector: String,
    label: &str,
    picked: bool,
    multi: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let row = div()
        .id(SharedString::from(format!("{selector}-row")))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap(px(8.))
        .w_full()
        .min_h(shell::control_height())
        .px(px(12.))
        .py(px(6.))
        .rounded(px(6.))
        .border_1()
        .border_color(if picked {
            theme::accent()
        } else {
            theme::line_strong()
        })
        .bg(if picked {
            theme::accent_soft()
        } else {
            theme::panel()
        })
        .text_size(px(theme::font_control()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text())
        .cursor_pointer()
        .hover(move |style| {
            if picked {
                style.bg(theme::accent_soft())
            } else {
                style.bg(theme::raised())
            }
        })
        .on_click(on_click)
        // The empty state is an outline — a circle for one answer, a square for
        // several — so an option reads as a choice and not as a text field. A
        // picked one takes the check in the accent.
        .child(if picked {
            icon(
                "icons/icon-check.svg",
                px(theme::icon_small()),
                theme::accent_ink(),
            )
            .into_any_element()
        } else {
            div()
                .flex_shrink_0()
                .size(px(theme::icon_small()))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(theme::icon_small() - 4.))
                        .border_1()
                        .border_color(theme::line_strong())
                        .when(multi, |square| square.rounded(px(3.)))
                        .when(!multi, |ring| ring.rounded_full()),
                )
                .into_any_element()
        })
        .child(div().flex_1().min_w_0().child(label.to_owned()));
    let row = if multi {
        row.a11y_toggle(label.to_owned(), picked)
    } else {
        row.a11y_button(label.to_owned())
    };
    row.into_any_element()
}

/// A quiet chip-like button: no fill until armed, and then the danger wash.
fn ghost_button(
    selector: String,
    label: &str,
    icon_path: Option<&str>,
    danger: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let color = if danger {
        theme::DANGER
    } else {
        theme::text_muted()
    };
    let mut button = div()
        .id(SharedString::from(format!("{selector}-button")))
        .debug_selector(move || selector.clone())
        .flex_shrink_0()
        .h(shell::control_height())
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .border_1()
        .border_color(if danger {
            theme::DANGER
        } else {
            theme::hairline()
        })
        .when(danger, |button| button.bg(theme::danger_bg()))
        .text_size(px(theme::font_control()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .cursor_pointer()
        .hover(move |style| style.bg(theme::raised()))
        .a11y_button(label.to_owned())
        .on_click(on_click);
    if let Some(path) = icon_path {
        button = button.child(icon(path, px(theme::icon_tiny()), color));
    }
    button.child(label.to_owned()).into_any_element()
}

/// The one filled primary of a surface: accent fill, on-accent ink, with the
/// design's leading icon. A disabled primary stays at 45 % and keeps its word.
fn primary_button(
    id: String,
    label: &str,
    icon_path: Option<&str>,
    enabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let mut button = div()
        .id(SharedString::from(id))
        .flex_shrink_0()
        .h(shell::control_height())
        .px(px(12.))
        .flex()
        .items_center()
        .gap(px(6.))
        .rounded(px(6.))
        .bg(theme::accent())
        .text_size(px(theme::font_control()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::on_accent())
        .cursor_pointer()
        .when(enabled, |button| button.hover(|style| style.opacity(0.9)))
        .when(!enabled, |button| button.opacity(0.45))
        .a11y_button(label.to_owned())
        .on_click(on_click);
    if let Some(path) = icon_path {
        button = button.child(icon(path, px(theme::icon_tiny()), theme::on_accent()));
    }
    button.child(label.to_owned()).into_any_element()
}

/// A shared head action: a primary or a quiet chip under its own debug
/// selector, with the reason a disabled button is disabled in its tooltip.
fn action_button(
    id: &'static str,
    label: &str,
    primary: bool,
    icon_path: Option<&'static str>,
    enabled: bool,
    reason: Option<&str>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> AnyElement {
    // A stateful wrapper is what carries a tooltip in this gpui rev; the kit
    // button keeps the click, the wrapper names the control and its reason.
    let mut wrapper = div()
        .id(SharedString::from(id))
        .debug_selector(move || id.into());
    if let Some(reason) = reason {
        wrapper = wrapper.tooltip(tooltip_text(reason.to_owned()));
    }
    let button = if primary {
        primary_button(format!("{id}-button"), label, icon_path, enabled, on_click)
    } else {
        controls::button(
            format!("{id}-button"),
            label.to_owned(),
            BtnKind::Chip,
            theme::accent(),
            theme::font_control(),
            enabled,
            on_click,
        )
    };
    wrapper.child(button).into_any_element()
}

/// A shared button under its own debug selector: the kit button does not take
/// one, and the probes click the wrapper's centre.
fn probed(id: &'static str, element: AnyElement) -> AnyElement {
    div()
        .debug_selector(move || id.into())
        .child(element)
        .into_any_element()
}

/// One number chip: the task's or the attempt's number in the mono face.
fn number_chip(selector: Option<String>, label: String, color: gpui::Rgba) -> AnyElement {
    let chip = div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .h(px(theme::scaled(theme::CHIP_HEIGHT)))
        .min_w(px(theme::scaled(26.)))
        .px(px(6.))
        .rounded(px(6.))
        .bg(theme::raised())
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(label);
    match selector {
        Some(selector) => chip
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .into_any_element(),
        None => chip.into_any_element(),
    }
}

/// One task in the Tasks card: its chip, then its title. The pair carries the
/// old `flow-run-task-NN` selector so the level geometry still reads.
fn task_pair(task: &TaskRow) -> AnyElement {
    let number = task.number;
    let selector = format!("flow-run-task-{number:02}");
    let chip_selector = format!("flow-task-chip-{number:02}");
    task_pair_inner(
        selector,
        chip_selector,
        format!("{number:02}"),
        task.title.clone(),
        task_status_color(task.status),
    )
}

fn task_pair_number(number: u32) -> AnyElement {
    let selector = format!("flow-run-task-{number:02}");
    let chip_selector = format!("flow-task-chip-{number:02}");
    task_pair_inner(
        selector,
        chip_selector,
        format!("{number:02}"),
        format!("task {number:02}"),
        theme::text_muted(),
    )
}

fn task_pair_inner(
    selector: String,
    chip_selector: String,
    label: String,
    title: String,
    color: gpui::Rgba,
) -> AnyElement {
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .role(gpui::Role::Status)
        .aria_label(format!("task {label} {title}"))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(number_chip(Some(chip_selector), label, color))
        .child(
            div()
                .max_w(px(theme::scaled(240.)))
                .truncate()
                .text_size(px(theme::font_body()))
                .text_color(theme::text_dim())
                .child(title),
        )
        .into_any_element()
}

/// One step's outcome: `not run yet` gets no pill, everything else a state pill
/// whose running word keeps its spinner.
fn state_pill(selector: String, state: StepState) -> AnyElement {
    let color = state_color(state);
    let spinner_id = format!("{selector}-spinner");
    let mut pill = div()
        .id(SharedString::from(format!("{selector}-pill")))
        .debug_selector(move || selector.clone())
        .role(gpui::Role::Status)
        .aria_label(state.label())
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(theme::scaled(theme::PILL_HEIGHT)))
        .px(px(8.))
        .rounded(px(999.))
        .bg(theme::tint(color))
        .text_size(px(theme::font_meta()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(if state == StepState::Running {
            motion::spinner(
                SharedString::from(spinner_id),
                "icons/icon-spinner.svg",
                px(theme::icon_tiny()),
                color,
            )
        } else {
            icon(state_icon(state), px(theme::icon_tiny()), color).into_any_element()
        });
    pill = pill.child(state.label().to_owned());
    pill.into_any_element()
}

/// The colour a step state is drawn in, for the workflow diagram.
pub(crate) fn state_color_public(state: StepState) -> gpui::Rgba {
    state_color(state)
}

fn state_color(state: StepState) -> gpui::Rgba {
    match state {
        StepState::Done => theme::ADD,
        StepState::Running => theme::BEHIND,
        StepState::Failed => theme::DANGER,
        StepState::Waiting => theme::CHANGES,
        StepState::Idle => theme::text_dim(),
    }
}

fn state_icon(state: StepState) -> &'static str {
    match state {
        StepState::Done => "icons/icon-check.svg",
        StepState::Running => "icons/icon-arc.svg",
        StepState::Failed => "icons/icon-close.svg",
        StepState::Waiting => "icons/icon-shield.svg",
        StepState::Idle => "icons/icon-clock.svg",
    }
}

fn kind_icon(kind: Option<StepKind>) -> &'static str {
    match kind {
        Some(StepKind::Harness) => "icons/icon-terminal.svg",
        Some(StepKind::Gate) => "icons/icon-gate.svg",
        Some(StepKind::Question) => "icons/icon-help.svg",
        Some(StepKind::Merge) => "icons/icon-merge.svg",
        Some(StepKind::Cleanup) => "icons/icon-trash.svg",
        None => "icons/icon-flow.svg",
    }
}

fn kind_word(kind: Option<StepKind>) -> &'static str {
    match kind {
        Some(StepKind::Harness) => "harness",
        Some(StepKind::Gate) => "gate",
        Some(StepKind::Question) => "question",
        Some(StepKind::Merge) => "merge",
        Some(StepKind::Cleanup) => "cleanup",
        None => "step",
    }
}

/// `test gate`, `code harness`, `merge step`: the step's word in a phrase.
fn step_label(step: &StepView) -> String {
    format!("{} {}", step.id, kind_word(step.kind))
}

fn is_blocked(run: &Run, phase: Option<Phase>) -> bool {
    if phase.is_some_and(Phase::is_driving) {
        return false;
    }
    let terminal =
        run.status.is_terminal() || matches!(phase, Some(Phase::Done) | Some(Phase::Abandoned));
    !terminal && (phase == Some(Phase::Blocked) || run.status.is_blocked())
}

fn is_paused(run: &Run, phase: Option<Phase>) -> bool {
    if phase.is_some_and(Phase::is_driving) {
        return false;
    }
    let terminal =
        run.status.is_terminal() || matches!(phase, Some(Phase::Done) | Some(Phase::Abandoned));
    !terminal && (phase == Some(Phase::Paused) || (phase.is_none() && run.status.is_paused()))
}

/// The paused note: the well strip, not a card, because the run has nothing to
/// decide here.
fn paused_note(host: Option<&RunSnapshot>) -> AnyElement {
    let clock = host
        .and_then(|host_run| host_run.finished.as_deref())
        .and_then(clock_word);
    let head = match clock {
        Some(clock) => format!("Paused {clock}"),
        None => "Paused".to_owned(),
    };
    div()
        .id("flow-run-paused-note")
        .debug_selector(|| "flow-run-paused".into())
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .w_full()
        .rounded(px(6.))
        .bg(theme::well())
        .px(px(12.))
        .py(px(8.))
        .text_size(px(theme::font_meta()))
        .line_height(relative(1.43))
        .text_color(theme::text_dim())
        .child(icon(
            "icons/icon-pause.svg",
            px(theme::icon_small()),
            theme::text_muted(),
        ))
        .child(
            div()
                .flex_shrink_0()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(head),
        )
        .child("· This run is paused. Resume starts the step it stopped on again.")
        .into_any_element()
}

/// The last failing line the engine left: the repair record first, then a
/// failed session's transcript, then the newest failed attempt in `run.md`.
///
/// The repair is first because it is the engine's own sentence about why the
/// step stopped, and the only one that survives the app restarting — the card
/// promises the repair file has the detail, so it must read it.
fn failure_line(run: &Run, failed: Option<&StepView>) -> Option<String> {
    let step = failed.map(|step| step.id.as_str());
    let repair = run
        .repairs
        .iter()
        .rev()
        .find(|repair| step.is_none_or(|id| repair.step == id))
        .and_then(|repair| {
            repair
                .summary
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|reason| format!("{} attempt {}: {reason}", repair.step, repair.attempt))
        });
    if repair.is_some() {
        return repair;
    }
    let session_line = run
        .sessions
        .iter()
        .rev()
        .find(|session| matches!(session.status.as_str(), "failed" | "timed-out"))
        .and_then(|session| {
            session
                .text
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty() && !is_session_placeholder(line))
        })
        .map(|line| line.trim().to_owned());
    if session_line.is_some() {
        return session_line;
    }
    run.rows
        .iter()
        .rev()
        .find(|row| outcome_state(&row.outcome) == StepState::Failed)
        .map(|row| {
            format!(
                "{} · task {} · attempt {} · {} · {}",
                row.step, row.task, row.attempt, row.outcome, row.at
            )
        })
}

/// The door writes this line into a session that left no text. It is a
/// placeholder in the artifact, not a reason, so the card never quotes it.
fn is_session_placeholder(line: &str) -> bool {
    line.trim() == "_The session reported no text._"
}

/// The host log's newest failing line, when this window drove the run.
fn host_failure_line(host: Option<&RunSnapshot>) -> Option<String> {
    let host = host?;
    host.log
        .iter()
        .rev()
        .find(|line| is_failure_line(line))
        .or_else(|| host.log.iter().rev().find(|line| !line.trim().is_empty()))
        .map(|line| line.trim().to_owned())
}

fn is_failure_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("fail")
        || lower.contains("error")
        || lower.contains("exited")
        || lower.contains("timed out")
        || lower.contains("conflict")
        || lower.contains("dirty")
        || lower.contains("budget spent")
}

/// The `HH:MM` of an RFC 3339 stamp, for the paused note's clock.
fn clock_word(stamp: &str) -> Option<String> {
    let (_, rest) = stamp.trim().split_once('T')?;
    let (hour, rest) = rest.split_once(':')?;
    let (minute, _) = rest.split_once(':')?;
    Some(format!("{hour}:{minute}"))
}

fn session_color(status: &str) -> gpui::Rgba {
    match status {
        "done" => theme::ADD,
        "running" => theme::BEHIND,
        "failed" | "timed-out" => theme::DANGER,
        _ => theme::text_muted(),
    }
}

fn task_status_color(status: Status) -> gpui::Rgba {
    match status {
        Status::Resolved => theme::ADD,
        Status::ReadyForAgent | Status::ReadyForHuman => theme::text_dim(),
        Status::NeedsTriage | Status::NeedsInfo => theme::text_muted(),
        Status::Wontfix => theme::DANGER,
    }
}

fn attempt_title(group: &LogGroup) -> String {
    if group.task.trim().is_empty() || group.task == "-" {
        group.step.clone()
    } else {
        format!("{} · task {}", group.step, group.task)
    }
}

/// The duration of one attempt, when a harness session recorded when it
/// settled. `run.md`'s table records the start only, so a gate attempt reads
/// `—` rather than a number the app cannot know.
fn attempt_duration(run: &Run, group: &LogGroup, row: &RunRow) -> String {
    let Some(start) = stamp_seconds(&row.at) else {
        return "—".to_owned();
    };
    let task = (!group.task.trim().is_empty() && group.task != "-").then_some(group.task.as_str());
    run.sessions
        .iter()
        .find(|session| {
            session.step == group.step
                && session.attempt == row.attempt
                && match (task, session.task.as_deref()) {
                    (Some(task), Some(session_task)) => task == session_task,
                    (None, None) => true,
                    _ => false,
                }
        })
        .and_then(|session| stamp_seconds(&session.updated).map(|end| end - start))
        .filter(|seconds| *seconds >= 0)
        .map(duration_words)
        .unwrap_or_else(|| "—".to_owned())
}

fn duration_words(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3_600, (seconds % 3_600) / 60)
    }
}

/// Seconds since the epoch from `run.md`'s and the harness's `YYYY-MM-DDTHH:MM:SSZ`
/// stamps. Anything else is `None`, which the duration shows as `—`.
fn stamp_seconds(stamp: &str) -> Option<i64> {
    let (date, rest) = stamp.trim().split_once('T')?;
    let rest = rest.strip_suffix('Z').unwrap_or(rest);
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    let mut clock = rest.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock
        .next()
        .and_then(|second| second.get(..2))
        .and_then(|second| second.parse().ok())
        .unwrap_or(0);
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted = (month + 9) % 12;
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The option this question would answer with, but only when it is an option
/// the current question actually offers.
fn picked_option<'a>(question: &'a Question, picked: &'a [String]) -> Option<&'a str> {
    if question.kind == "multi" {
        return None;
    }
    if question.kind == "approve" {
        return picked.first().map(String::as_str);
    }
    picked
        .iter()
        .map(String::as_str)
        .find(|choice| question.options.iter().any(|option| option == choice))
}

fn count_word(count: usize, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

/// The phase in one word, with an ellipsis where the engine is moving.
fn phase_word(phase: Phase) -> &'static str {
    match phase {
        Phase::Starting => "Starting…",
        Phase::Running => "Running",
        Phase::Pausing => "Pausing…",
        Phase::Paused => "Paused",
        Phase::Waiting => "Waiting on you",
        Phase::Finishing => "Finishing…",
        Phase::Done => "Done",
        Phase::Blocked => "Blocked",
        Phase::Abandoned => "Abandoned",
        Phase::Failed => "Failed",
    }
}

/// Whether the phase is a moving one: this is where a spinner is allowed to
/// show, and reduced motion keeps the word.
fn phase_moves(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Starting | Phase::Running | Phase::Pausing | Phase::Finishing
    )
}

/// The phase's colour, from the run tab's state palette.
fn phase_color(phase: Phase) -> gpui::Rgba {
    match phase {
        Phase::Starting | Phase::Running | Phase::Pausing => theme::BEHIND,
        Phase::Paused => theme::text_dim(),
        Phase::Waiting => theme::CHANGES,
        Phase::Finishing => theme::text_muted(),
        Phase::Done => theme::ADD,
        Phase::Blocked | Phase::Abandoned | Phase::Failed => theme::DANGER,
    }
}

/// The error lines the run body shows above its state card: the window's own
/// refusal first, then the host worker's.
///
/// A worker that never reached the engine — the door is not published, the
/// worker thread could not start — fails before it writes a log line, so the
/// snapshot's error is the only trace of a `Retry gate` that did nothing. The
/// same sentence twice is one line.
fn run_error_lines(action_error: Option<&str>, host: Option<&RunSnapshot>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for error in [action_error, host.and_then(|host| host.error.as_deref())]
        .into_iter()
        .flatten()
    {
        let error = error.trim();
        if !error.is_empty() && !lines.iter().any(|line| line == error) {
            lines.push(error.to_owned());
        }
    }
    lines
}

/// One readable line for a start, resume, pause, retry or abandon the host
/// refused, or for a worker that failed before it could drive.
fn action_error_line(error: &str) -> AnyElement {
    div()
        .id("flow-run-action-error-line")
        .debug_selector(|| "flow-run-action-error".into())
        .w_full()
        .rounded(px(8.))
        .border_1()
        .border_color(theme::DANGER)
        .bg(theme::danger_bg())
        .px(px(12.))
        .py(px(10.))
        .font_family(theme::code_font())
        .text_size(px(theme::font_mono_meta()))
        .text_color(theme::DANGER)
        .child(error.to_owned())
        .into_any_element()
}

/// The workflow's one warning line: a missing or unreadable workflow file
/// means the bands came from the log.
fn workflow_note_row(note: &str) -> AnyElement {
    div()
        .id("flow-run-workflow-note-line")
        .debug_selector(|| "flow-run-workflow-note".into())
        .flex()
        .items_start()
        .gap(px(8.))
        .mx(px(16.))
        .my(px(10.))
        .rounded(px(6.))
        .bg(theme::well())
        .px(px(12.))
        .py(px(8.))
        .text_size(px(theme::font_meta()))
        .line_height(relative(1.43))
        .text_color(theme::text_dim())
        .child(icon(
            "icons/icon-help.svg",
            px(theme::icon_small()),
            theme::text_muted(),
        ))
        .child(div().flex_1().min_w_0().child(note.to_owned()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Column, Repair, Run, RunKey, Session};

    /// A run record with the fields the failure line reads.
    fn run() -> Run {
        Run {
            key: RunKey {
                repo: std::path::PathBuf::from("/tmp/repo"),
                id: "demo".to_owned(),
            },
            dir: std::path::PathBuf::from("/tmp/repo/.scratch/demo"),
            has_run: true,
            title: "Demo".to_owned(),
            column: Column::Blocked,
            status: crate::store::RunState::Blocked,
            step: "analyze".to_owned(),
            workflow: "implement@1".to_owned(),
            attempts: 3,
            tasks_total: 0,
            tasks_done: 0,
            task_rows: Vec::new(),
            task_levels: Vec::new(),
            question: None,
            open_questions: Vec::new(),
            rows: Vec::new(),
            stop_reason: None,
            graph: None,
            workflow_path: None,
            workflow_note: None,
            steps: Vec::new(),
            spec: String::new(),
            tasks: String::new(),
            sessions: Vec::new(),
            repairs: Vec::new(),
            note: None,
            updated: None,
        }
    }

    fn repair(step: &str, attempt: u32, summary: &str) -> Repair {
        Repair {
            step: step.to_owned(),
            task: None,
            attempt: attempt.to_string(),
            of: "3".to_owned(),
            outcome: "blocked".to_owned(),
            at: format!("2026-11-22T10:3{attempt}:22Z"),
            summary: summary.to_owned(),
        }
    }

    fn timed_out_session(text: &str) -> Session {
        Session {
            session: "session-1".to_owned(),
            harness: "dsh".to_owned(),
            step: "analyze".to_owned(),
            task: None,
            attempt: "3".to_owned(),
            status: "timed-out".to_owned(),
            updated: "2026-11-22T10:33:22Z".to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn the_blocked_card_quotes_the_repair_not_the_session_placeholder() {
        let mut run = run();
        run.sessions = vec![timed_out_session("_The session reported no text._")];
        run.repairs = vec![repair(
            "analyze",
            3,
            "harness blocked: the session did not report idle within 300s",
        )];

        let line = failure_line(&run, None).expect("the repair names the reason");

        assert!(
            line.contains("analyze attempt 3:"),
            "the line names the step and attempt: {line}"
        );
        assert!(
            line.contains("did not report idle"),
            "the line is the engine's own sentence: {line}"
        );
        assert!(
            !line.contains("reported no text"),
            "the session's placeholder is never a reason: {line}"
        );
    }

    #[test]
    fn the_placeholder_alone_leaves_no_failure_line() {
        let mut run = run();
        run.sessions = vec![timed_out_session("_The session reported no text._")];

        assert!(
            failure_line(&run, None).is_none(),
            "a session that wrote no text is not a reason"
        );
    }

    #[test]
    fn every_error_the_window_and_the_worker_left_is_one_line_each() {
        let snapshot = RunSnapshot {
            issue: std::path::PathBuf::from("/tmp/repo/.scratch/demo"),
            repo: std::path::PathBuf::from("/tmp/repo"),
            title: "Demo".to_owned(),
            phase: Phase::Failed,
            status: None,
            started: "2026-11-22T14:05:00Z".to_owned(),
            finished: Some("2026-11-22T14:05:01Z".to_owned()),
            error: Some("harness: the door is not published".to_owned()),
            log: Vec::new(),
        };

        assert_eq!(
            run_error_lines(None, Some(&snapshot)),
            vec!["harness: the door is not published".to_owned()],
            "a worker that never logged is still visible"
        );
        assert_eq!(
            run_error_lines(Some("  "), Some(&snapshot)).len(),
            1,
            "blank action errors are not lines"
        );
    }

    fn task(number: u32, title: &str) -> TaskRow {
        TaskRow {
            number,
            title: title.to_owned(),
            status: Status::ReadyForAgent,
            blocked_by: Vec::new(),
        }
    }

    fn lane(number: u32, title: &str, state: StepState) -> LaneView {
        LaneView {
            number,
            title: title.to_owned(),
            state,
        }
    }

    fn step(id: &str, per_task: bool, lanes: Vec<LaneView>) -> StepView {
        StepView {
            id: id.to_owned(),
            kind: Some(StepKind::Harness),
            detail: String::new(),
            per_task,
            state: StepState::Idle,
            note: None,
            lanes,
        }
    }

    #[test]
    fn the_matrix_puts_every_task_across_once_and_keeps_a_stray_lane_column() {
        let mut run = run();
        run.task_rows = vec![task(1, "One"), task(2, "Two")];
        run.steps = vec![
            step(
                "code",
                true,
                vec![
                    lane(1, "One", StepState::Done),
                    lane(2, "Two", StepState::Running),
                    lane(7, "A task the files lost", StepState::Failed),
                ],
            ),
            step("test", true, vec![lane(1, "One", StepState::Done)]),
        ];

        let columns = matrix_columns(&run);
        let numbers: Vec<u32> = columns.iter().map(|column| column.number).collect();
        assert_eq!(
            numbers,
            vec![1, 2, 7],
            "each task is one column, in task order"
        );
        assert_eq!(
            columns[2].title, "A task the files lost",
            "a lane naming a task the task files lost keeps its own title"
        );
    }

    #[test]
    fn a_run_with_no_task_files_has_no_columns() {
        let mut run = run();
        run.steps = vec![step("code", true, Vec::new())];
        assert!(
            matrix_columns(&run).is_empty(),
            "no tasks means no header and no cells"
        );
    }

    #[test]
    fn a_task_without_a_lane_reads_as_no_lane() {
        let mut run = run();
        run.task_rows = vec![task(1, "One"), task(2, "Two")];
        let code = step("code", true, vec![lane(1, "One", StepState::Done)]);
        assert_eq!(
            matrix_lane(&code, 2),
            None,
            "a task the step never ran has no lane"
        );
        assert_eq!(
            matrix_lane(&code, 1).map(|lane| lane.state),
            Some(StepState::Done),
            "the lane a step does hold keeps its state"
        );
    }

    #[test]
    fn more_tasks_shrink_the_columns_to_fit_the_reading_column() {
        let max = theme::scaled(MATRIX_COLUMN_MAX);
        let min = theme::scaled(MATRIX_COLUMN_MIN);
        let reading = theme::scaled(shell::READING_WIDTH);
        let single = matrix_column_width(1, reading);
        let five = matrix_column_width(5, reading);
        let twelve = matrix_column_width(12, reading);
        let many = matrix_column_width(40, reading);
        assert_eq!(single, max, "one task takes the widest column");
        assert!(
            five < max && five > min,
            "five tasks share the measure instead of sitting at one width"
        );
        assert!(five > twelve, "columns shrink as tasks are added");
        assert_eq!(twelve, min, "twelve tasks sit at the floor");
        assert_eq!(many, min, "a column never narrows past one cell");
        assert_eq!(many, min, "a column never narrows below one cell");
        let gap = theme::scaled(MATRIX_GAP);
        let total = |tasks: usize| {
            theme::scaled(2. * MATRIX_SIDE)
                + theme::scaled(MATRIX_LABEL_MIN)
                + tasks as f32 * (matrix_column_width(tasks, reading) + gap)
        };
        for tasks in 1..=10 {
            assert!(
                total(tasks) <= reading + 0.01,
                "{tasks} tasks overflow the reading column: {}",
                total(tasks)
            );
        }
        assert!(
            total(11) > reading,
            "past ten tasks the matrix keeps its label minimum and scrolls"
        );
    }

    #[test]
    fn a_matrix_too_wide_for_the_reading_column_asks_for_a_scroller() {
        let reading = theme::scaled(shell::READING_WIDTH);
        let tasks = 24;
        let width = matrix_column_width(tasks, reading);
        assert_eq!(
            width,
            theme::scaled(MATRIX_COLUMN_MIN),
            "twenty-four tasks sit at the floor"
        );
        assert!(
            matrix_width(tasks, width) > reading,
            "the card must scroll, not clip, once the columns pass the measure"
        );
        assert!(
            matrix_width(3, matrix_column_width(3, reading)) <= reading,
            "three tasks still fit the reading column without a scroll"
        );
    }

    /// The matrix sizes its columns from the live body, not the constant 760:
    /// in a 1100 px window the body is 544, and five tasks must still fit.
    #[test]
    fn a_narrow_body_fits_its_task_columns() {
        let narrow = 544.;
        let tasks = 5;
        let width = matrix_column_width(tasks, narrow);
        let gap = theme::scaled(MATRIX_GAP);
        let total = theme::scaled(2. * MATRIX_SIDE)
            + theme::scaled(MATRIX_LABEL_MIN)
            + tasks as f32 * (width + gap);
        assert!(
            total <= narrow + 0.01,
            "five tasks in a 544 px body need {total}"
        );
    }
}
