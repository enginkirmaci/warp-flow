//! The Run tab's body: the one card the run's state calls for (a question, a
//! block, a pause, or a run that has not started), the workflow's step bands,
//! the task list, the attempt history, the live engine log and the harness
//! sessions. The head, the tabs and the 44 strip belong to the detail pane
//! ([`crate::ui::detail`]); this file draws what sits under them.
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
use crate::ui::window::{FlowApp, RunTab, attempt_word, ensure_document, tag};

impl FlowApp {
    // ---- entry points ------------------------------------------------------

    /// Close the run pane: the same thing `Escape` does, in the one place a
    /// probe can also call.
    pub(crate) fn close_run_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.screen = None;
        self.selected_run = None;
        self.run_question = None;
        self.run_multi.clear();
        self.run_answer_error = None;
        self.run_action_error = None;
        self.run_abandon_confirm = None;
        self.run_issue = None;
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
            self.run_answer_error = Some("Pick an option or write an answer.".to_owned());
            cx.notify();
            return;
        }
        let Some(issue) = self.run_issue.clone() else {
            self.run_answer_error =
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
                self.run_answer_error = None;
                self.answer_field
                    .update(cx, |state, cx| state.set_value("", window, cx));
                cx.notify();
            }
            Err(error) => {
                self.run_answer_error = Some(error);
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
            .run_multi
            .iter()
            .filter(|choice| question.options.iter().any(|option| option == *choice))
            .cloned()
            .collect();
        let answer = if question.kind == "multi" && !picked.is_empty() {
            picked.join(", ")
        } else if !typed.is_empty() {
            typed
        } else if question.kind == "approve" {
            self.run_multi.first().cloned().unwrap_or_default()
        } else {
            picked.first().cloned().unwrap_or_default()
        };
        if answer.trim().is_empty() {
            self.run_answer_error = Some("Pick an option or write an answer.".to_owned());
            cx.notify();
            return;
        }
        self.run_abandon_confirm = None;
        self.submit_answer(question, answer, window, cx);
    }

    // ---- driving the run ---------------------------------------------------

    /// Start a fresh run for the open screen's issue.
    fn start_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.start(&issue) {
            Ok(()) => self.run_action_error = None,
            Err(error) => self.run_action_error = Some(error),
        }
        cx.notify();
    }

    /// Ask the host to pause the open run at its next step boundary.
    pub(crate) fn pause_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.pause(&issue) {
            Ok(()) => self.run_action_error = None,
            Err(error) => self.run_action_error = Some(error),
        }
        cx.notify();
    }

    /// Continue the open run from its record, or take over a run another
    /// process is driving: the same takeover `warp-flow-cli run` does.
    pub(crate) fn resume_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.resume(&issue) {
            Ok(()) => self.run_action_error = None,
            Err(error) => self.run_action_error = Some(error),
        }
        cx.notify();
    }

    /// Retry the step a blocked run stopped on. The host has no separate retry
    /// verb: resuming the record re-enters the blocked step, and the engine
    /// spends a fresh attempt budget there. The blocked card's footer states
    /// that in words so `Retry gate` is not a promise the engine cannot keep.
    pub(crate) fn retry_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.resume(&issue) {
            Ok(()) => self.run_action_error = None,
            Err(error) => self.run_action_error = Some(error),
        }
        cx.notify();
    }

    /// Abandon the open run at its next step boundary, after the confirmation.
    pub(crate) fn abandon_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        self.run_abandon_confirm = None;
        match self.host.abandon(&issue) {
            Ok(()) => self.run_action_error = None,
            Err(error) => self.run_action_error = Some(error),
        }
        cx.notify();
    }

    /// Tick or untick one `multi` choice.
    fn toggle_multi(&mut self, label: String) {
        if let Some(index) = self.run_multi.iter().position(|choice| choice == &label) {
            self.run_multi.remove(index);
        } else {
            self.run_multi.push(label);
        }
    }

    // ---- rendering ---------------------------------------------------------

    /// The Run tab's body: what the engine is doing, what it waits on, and the
    /// attempts and sessions it recorded.
    pub(crate) fn render_run_body(
        &mut self,
        run: Run,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The host keys its runs by the issue folder; every action below uses
        // this one, which the scan resolved before the frame.
        self.run_issue = Some(run.dir.clone());
        if run.question.is_none() {
            // The card closed: the pick it held has no meaning on the next one.
            self.run_multi.clear();
        }
        let host = self.host_snapshot.run(&run.dir).cloned();
        let phase = host.as_ref().map(|host_run| host_run.phase);
        let question = run.question.clone();
        let question_doc = question.as_ref().and_then(|question| {
            // Only the body is the editor's source; the path is not content.
            ensure_document(
                &mut self.run_question,
                &run.key,
                question_markdown(&question.body),
                window,
                cx,
            )
        });

        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(theme::scaled(theme::RUN_GAP)))
            .w_full()
            .max_w(px(theme::scaled(shell::READING_WIDTH)))
            .mx_auto();

        for error in run_error_lines(self.run_action_error.as_deref(), host.as_ref()) {
            column = column.child(action_error_line(&error));
        }

        let issue_on_top = question.is_none()
            && !run.has_run
            && !is_blocked(&run, phase)
            && !is_paused(&run, phase);

        if let Some(card) = self.render_state_card(
            &run,
            host.as_ref(),
            question.as_ref(),
            question_doc.as_ref(),
            window,
            cx,
        ) {
            column = column.child(card);
        }

        if !run.steps.is_empty() {
            column = column.child(self.render_workflow_card(&run, phase));
        } else {
            if let Some(note) = &run.workflow_note {
                column = column.child(workflow_note_row(note));
            }
            if !issue_on_top {
                column = column.child(self.render_issue_card(&run, phase));
            }
        }

        column = column
            .child(self.render_tasks_card(&run))
            .child(self.render_attempts_card(&run, cx));
        if let Some(log) = self.render_engine_log(&run, host.as_ref()) {
            column = column.child(log);
        }
        column = column.child(self.render_sessions_card(&run));

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
            return Some(self.render_question_card(run, question, question_doc, window, cx));
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
        let picked = picked_option(question, &self.run_multi);
        let typed = self.answer_field.read(cx).value().to_string();
        let ticked = self
            .run_multi
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
                    .child(warpui_kit_markdown::render_read_only_document_in_card(
                        body, cx,
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
                    let picked_here = self.run_multi.iter().any(|pick| pick == choice);
                    let question = question.clone();
                    let answer = choice.to_owned();
                    let on_click = cx.listener(
                        move |this, _, window: &mut Window, cx: &mut Context<Self>| {
                            this.run_abandon_confirm = None;
                            this.run_multi.clear();
                            this.run_multi.push(answer.clone());
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
                            this.run_abandon_confirm = None;
                            this.run_multi.clear();
                            this.run_multi.push(choice.clone());
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
                    let picked_here = self.run_multi.contains(option);
                    let choice = option.clone();
                    let on_click = cx.listener(
                        move |this, _, _window: &mut Window, cx: &mut Context<Self>| {
                            this.run_abandon_confirm = None;
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

        let armed = self.run_abandon_confirm.as_ref() == Some(&run.key);
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
                if this.run_abandon_confirm.as_ref() == Some(&key) {
                    this.abandon_run(issue.clone(), cx);
                } else {
                    this.run_abandon_confirm = Some(key.clone());
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
        if let Some(error) = &self.run_answer_error {
            card = card.child(
                div()
                    .debug_selector(|| "flow-answer-error".into())
                    .mt(px(8.))
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::DANGER)
                    .child(error.clone()),
            );
        }
        card.child(
            div()
                .mt(px(10.))
                .text_size(px(theme::font_meta()))
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
    /// `Retry gate` and `Abandon`.
    fn render_blocked_card(&self, run: &Run, host: Option<&RunSnapshot>) -> AnyElement {
        let failed = failed_step(run);
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
                if let Some(note) = &run.note {
                    why.push(' ');
                    why.push_str(note);
                }
                why
            }
            None => {
                let mut why = "The run stopped with no failing step in its record.".to_owned();
                if let Some(note) = &run.note {
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
                    .text_size(px(theme::font_meta()))
                    .line_height(relative(1.43))
                    .text_color(theme::text_muted())
                    .child(why),
            );

        if let Some(line) = host_failure_line(host).or_else(|| failure_line(run, failed)) {
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

        let footer = match failed {
            Some(_) => mono_sentence(
                &format!(
                    "Retry gate starts the step again with a fresh {budget}-attempt budget \
                     ({spent} recorded); Abandon writes "
                ),
                "status: abandoned",
                " without running it.",
            ),
            None => mono_sentence(
                "Retry gate resumes the run from its record; Abandon writes ",
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
        trailing.push(quiet_pill("icons/icon-box.svg", &run.status));

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
                    .text_size(px(theme::font_meta()))
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

    /// One 44-high band per workflow step, a per-task step's rows hanging under
    /// it on one hairline, and the outcome at the right of each band.
    fn render_workflow_card(&self, run: &Run, phase: Option<Phase>) -> AnyElement {
        let mut head = Vec::new();
        if let Some(busy) = phase.and_then(|phase| busy_word("flow-run-workflow-spinner", phase)) {
            head.push(busy);
        }
        let mut meta = div().flex().items_center().gap(px(6.));
        if !run.workflow.trim().is_empty() && run.workflow.trim() != "—" {
            meta = meta.child(head_mono(run.workflow.clone())).child(
                div()
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child("·"),
            );
        }
        meta = meta.child(head_word(count_word(run.steps.len(), "step")));
        head.push(meta.into_any_element());

        let mut steps = div()
            .debug_selector(|| "flow-run-steps".into())
            .flex()
            .flex_col();
        for (index, step) in run.steps.iter().enumerate() {
            steps = steps.child(self.render_step_band(step, index > 0));
        }

        let mut card = card()
            .id("flow-run-workflow-card")
            .debug_selector(|| "flow-run-workflow".into())
            .child(card_head("Workflow", head))
            .child(steps);
        if let Some(note) = &run.workflow_note {
            card = card.child(workflow_note_row(note));
        }
        card.into_any_element()
    }

    fn render_step_band(&self, step: &StepView, separated: bool) -> AnyElement {
        let selector = format!("flow-run-step-{}", step.id);
        let state_selector = format!("flow-run-state-{}", step.id);
        let kind = kind_word(step.kind);
        // The design reads `gate · test` and `harness · skill analyze ·
        // produces spec.md`: the gate's own name is the value, not the word
        // "gate" again; a harness step keeps its skill and output names.
        let detail = match step.kind {
            Some(StepKind::Gate) => step
                .detail
                .strip_prefix("gate: ")
                .unwrap_or(&step.detail)
                .to_owned(),
            _ => step.detail.replace(": ", " "),
        };

        let mut band = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .when(separated, |band| {
                band.border_t_1().border_color(theme::border_light())
            });

        let mut row = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .min_h(px(theme::scaled(theme::STEP_HEIGHT)))
            .px(px(16.))
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
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::font_meta()))
                            .text_color(theme::text_muted())
                            .child(if step.kind == Some(StepKind::Merge) {
                                "the merge queue".to_owned()
                            } else {
                                kind.to_owned()
                            }),
                    )
                    .when(
                        !detail.is_empty() && step.kind != Some(StepKind::Merge),
                        |row| {
                            row.child(
                                div()
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
                                    .child(detail.clone()),
                            )
                        },
                    ),
            );

        if step.state == StepState::Idle {
            row = row.child(
                div()
                    .id(SharedString::from(format!("{state_selector}-idle")))
                    .debug_selector({
                        let selector = state_selector.clone();
                        move || selector.clone()
                    })
                    .flex_shrink_0()
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_muted())
                    .child("not run yet"),
            );
        } else {
            row = row.child(state_pill(state_selector, step.state));
        }
        band = band.child(row);

        if step.per_task {
            let mut lanes = div()
                .flex()
                .flex_col()
                .ml(px(44.))
                .mr(px(16.))
                .mb(px(6.))
                .mt(px(-4.))
                .border_l_1()
                .border_color(theme::hairline());
            if step.lanes.is_empty() {
                lanes = lanes.child(
                    div()
                        .px(px(12.))
                        .py(px(6.))
                        .text_size(px(theme::font_meta()))
                        .text_color(theme::text_muted())
                        .child("No task files yet."),
                );
            }
            for lane in &step.lanes {
                lanes = lanes.child(self.render_lane(step, lane));
            }
            band = band.child(lanes);
        }
        band.into_any_element()
    }

    fn render_lane(&self, step: &StepView, lane: &LaneView) -> AnyElement {
        let selector = format!("flow-run-lane-{}-{:02}", step.id, lane.number);
        let state_selector = format!("flow-run-state-{}-{:02}", step.id, lane.number);
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap(px(10.))
            .min_h(px(theme::scaled(theme::TASK_ROW_HEIGHT)))
            .pl(px(12.))
            .pr(px(8.))
            .child(number_chip(
                None,
                format!("{:02}", lane.number),
                state_color(lane.state),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::font_body()))
                    .text_color(theme::text_dim())
                    .child(lane.title.clone()),
            )
            .child(state_pill(state_selector, lane.state))
            .into_any_element()
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
                .text_size(px(theme::font_meta()))
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
                .items_center()
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
                        .text_size(px(theme::font_meta()))
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
            .text_size(px(theme::font_meta()))
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
                        .text_size(px(theme::font_meta()))
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
                .text_size(px(theme::font_meta()))
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

    /// The run's head actions, derived from the host's phase first and its
    /// record second, because a run that is being driven is the host's to say.
    ///
    /// One primary per state: Start run for triage/ready, Resume for paused,
    /// Retry gate for blocked — and none for a waiting run (the question card's
    /// Answer is the run's primary), a driving run or a landed one. Terminal
    /// runs keep their Start button, parked with the reason in its tooltip.
    pub(crate) fn render_run_actions(&self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let host_run = self.host_snapshot.run(&run.dir);
        let phase = host_run.map(|host_run| host_run.phase);
        let driving = phase.is_some_and(Phase::is_driving);
        let terminal = matches!(
            run.status.as_str(),
            "done" | "merged" | "abandoned" | "wontfix"
        ) || matches!(phase, Some(Phase::Done) | Some(Phase::Abandoned));
        let has_record = run.has_run
            || host_run
                .and_then(|host_run| host_run.status.as_deref())
                .is_some();
        let question_open = run.question.is_some();
        let record_status = host_run
            .and_then(|host_run| host_run.status.clone())
            .unwrap_or_else(|| run.status.clone());
        let issue = run.dir.clone();

        let mut row = div()
            .id("flow-run-actions")
            .debug_selector(|| "flow-run-actions".into())
            .flex()
            .flex_wrap()
            .items_center()
            .justify_end()
            .gap(px(6.));

        if driving {
            // A start that is in flight disables both actions, with the reason
            // in each tooltip; once the engine reports, they are live again.
            let starting = phase == Some(Phase::Starting);
            let (pause_enabled, pause_reason) = match phase {
                Some(Phase::Pausing) => {
                    (false, Some("The run is pausing at the next step boundary."))
                }
                Some(Phase::Starting) => (false, Some("The engine is starting.")),
                _ => (true, None),
            };
            let pause_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-pause",
                "Pause",
                false,
                None,
                pause_enabled,
                pause_reason,
                cx.listener(move |this, _, _, cx| this.pause_run(pause_issue.clone(), cx)),
            ));
            row = row.child(self.abandon_control(run, starting, cx));
        } else if terminal {
            row = row.child(action_button(
                "flow-run-start",
                "Start",
                false,
                None,
                false,
                Some("This run is done; there is nothing to start."),
                nothing,
            ));
        } else if is_blocked(run, phase) {
            let retry_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-retry",
                "Retry gate",
                true,
                Some("icons/icon-refresh.svg"),
                true,
                Some("Run the blocked step again from its record."),
                cx.listener(move |this, _, _, cx| this.retry_run(retry_issue.clone(), cx)),
            ));
            row = row.child(self.abandon_control(run, false, cx));
        } else if question_open {
            // The question card owns the run's one filled primary and its
            // Abandon; the head stays empty so the two never compete.
        } else if is_paused(run, phase) {
            let resume_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-resume",
                "Resume",
                true,
                Some("icons/icon-play.svg"),
                true,
                Some("Continue the paused run from its record."),
                cx.listener(move |this, _, _, cx| this.resume_run(resume_issue.clone(), cx)),
            ));
            row = row.child(self.abandon_control(run, false, cx));
        } else if phase == Some(Phase::Failed) && has_record {
            let resume_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-resume",
                "Resume",
                false,
                None,
                true,
                Some("The engine stopped; resume to try again."),
                cx.listener(move |this, _, _, cx| this.resume_run(resume_issue.clone(), cx)),
            ));
            row = row.child(self.abandon_control(run, false, cx));
        } else if phase == Some(Phase::Waiting) || (phase.is_none() && record_status == "waiting") {
            let resume_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-resume",
                "Resume",
                false,
                None,
                true,
                Some("The question is answered; continue the run."),
                cx.listener(move |this, _, _, cx| this.resume_run(resume_issue.clone(), cx)),
            ));
            row = row.child(self.abandon_control(run, false, cx));
        } else if has_record && record_status == "running" {
            // Somebody else's run: Resume is the takeover, not a primary.
            let resume_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-resume",
                "Resume",
                false,
                None,
                true,
                Some("Not driven by this window; Resume takes it over."),
                cx.listener(move |this, _, _, cx| this.resume_run(resume_issue.clone(), cx)),
            ));
            row = row.child(self.abandon_control(run, false, cx));
        } else if has_record && record_status == "blocked" {
            let retry_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-retry",
                "Retry gate",
                true,
                Some("icons/icon-refresh.svg"),
                true,
                Some("Run the blocked step again from its record."),
                cx.listener(move |this, _, _, cx| this.retry_run(retry_issue.clone(), cx)),
            ));
            row = row.child(self.abandon_control(run, false, cx));
        } else {
            let start_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-start",
                "Start run",
                true,
                Some("icons/icon-play.svg"),
                true,
                None,
                cx.listener(move |this, _, _, cx| this.start_run(start_issue.clone(), cx)),
            ));
        }
        row.into_any_element()
    }

    /// Abandon for a run no worker is driving, behind the same one-step
    /// confirmation the driving case uses.
    fn abandon_control(&self, run: &Run, disabled: bool, cx: &mut Context<Self>) -> AnyElement {
        if self.run_abandon_confirm.as_ref() == Some(&run.key) {
            return self.render_abandon_confirm(&run.dir, cx);
        }
        let key = run.key.clone();
        action_button(
            "flow-run-abandon",
            "Abandon",
            false,
            None,
            !disabled,
            disabled.then_some("The engine is starting."),
            cx.listener(move |this, _, _, cx| {
                this.run_abandon_confirm = Some(key.clone());
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
                    .text_size(px(theme::font_micro()))
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
                    this.run_abandon_confirm = None;
                    cx.notify();
                }),
            ))
            .into_any_element()
    }
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

/// A trailing count or word at 13, `text_dim`.
fn head_word(text: impl Into<String>) -> AnyElement {
    div()
        .flex_shrink_0()
        .text_size(px(theme::font_meta()))
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
        .child(if picked {
            icon(
                "icons/icon-check.svg",
                px(theme::icon_small()),
                theme::accent_ink(),
            )
            .into_any_element()
        } else {
            div().size(px(theme::icon_small())).into_any_element()
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

/// The action a disabled button never runs: the kit keeps the handler wired,
/// so the caller's guard is this no-op.
fn nothing(_: &gpui::ClickEvent, _: &mut Window, _: &mut gpui::App) {}

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
        None => "icons/icon-flow.svg",
    }
}

fn kind_word(kind: Option<StepKind>) -> &'static str {
    match kind {
        Some(StepKind::Harness) => "harness",
        Some(StepKind::Gate) => "gate",
        Some(StepKind::Question) => "question",
        Some(StepKind::Merge) => "merge",
        None => "step",
    }
}

/// The failing step a blocked run stopped on: a failed gate first, then any
/// failed step.
fn failed_step(run: &Run) -> Option<&StepView> {
    run.steps
        .iter()
        .find(|step| step.state == StepState::Failed && step.kind == Some(StepKind::Gate))
        .or_else(|| {
            run.steps
                .iter()
                .find(|step| step.state == StepState::Failed)
        })
}

/// `test gate`, `code harness`, `merge step`: the step's word in a phrase.
fn step_label(step: &StepView) -> String {
    format!("{} {}", step.id, kind_word(step.kind))
}

fn is_blocked(run: &Run, phase: Option<Phase>) -> bool {
    if phase.is_some_and(Phase::is_driving) {
        return false;
    }
    let terminal = matches!(
        run.status.as_str(),
        "done" | "merged" | "abandoned" | "wontfix"
    ) || matches!(phase, Some(Phase::Done) | Some(Phase::Abandoned));
    !terminal && (phase == Some(Phase::Blocked) || run.status == "blocked")
}

fn is_paused(run: &Run, phase: Option<Phase>) -> bool {
    if phase.is_some_and(Phase::is_driving) {
        return false;
    }
    let terminal = matches!(
        run.status.as_str(),
        "done" | "merged" | "abandoned" | "wontfix"
    ) || matches!(phase, Some(Phase::Done) | Some(Phase::Abandoned));
    !terminal && (phase == Some(Phase::Paused) || (phase.is_none() && run.status == "paused"))
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
            status: "blocked".to_owned(),
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
}
