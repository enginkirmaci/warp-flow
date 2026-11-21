//! The run screen: one run's head, workflow graph, task strip, open question
//! and attempt history in one scrolling column. The repository rail stays; the
//! board is replaced. Back returns to the board with the run still selected.
//!
//! The graph is rows, not an SVG: one band per workflow step, one lane per task
//! for a `per_task` step. The states come from `run.md`'s log and the task
//! files, through the scan, never from the filesystem during a frame.

use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, FontWeight, KeyDownEvent, SharedString, Window, div, px, relative,
};
use warp_flow_core::artifact::Status;
use warp_flow_core::graph::StepKind;
use warpui_kit_core::controls::{self, BtnKind};
use warpui_kit_core::tooltip::tooltip_text;
use warpui_kit_core::{A11y, icon, motion};
use warpui_kit_markdown::LiveEditorState;

use crate::host::{AnswerOutcome, Phase};
use crate::store::{
    self, LaneView, Question, Run, RunKey, StepState, StepView, TaskRow, outcome_state,
};
use crate::theme;
use crate::ui::screens::Screen;
use crate::ui::window::{FlowApp, attempt_word, ensure_document, repo_label, status_pill, tag};

impl FlowApp {
    // ---- entry points ------------------------------------------------------

    /// Open the full run screen for `key`, keeping it selected under the
    /// detail pane so Back returns to the same run.
    pub(crate) fn open_run_screen(
        &mut self,
        key: RunKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cursor = Some(key.clone());
        self.selected_run = Some(key.clone());
        self.screen = Some(Screen::Run);
        self.run_scroll = gpui::ScrollHandle::new();
        self.run_log_scroll = gpui::ScrollHandle::new();
        self.run_question = None;
        self.run_multi.clear();
        self.run_answer_error = None;
        self.run_action_error = None;
        self.run_abandon_confirm = None;
        self.run_issue = None;
        self.answer_field
            .update(cx, |state, cx| state.set_value("", window, cx));
        window.focus(&self.run_focus, cx);
        cx.notify();
    }

    /// Back to the board: the run stays selected, so its detail pane is open.
    pub(crate) fn close_run_screen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.screen = None;
        self.run_question = None;
        self.run_multi.clear();
        self.run_answer_error = None;
        self.run_action_error = None;
        self.run_abandon_confirm = None;
        self.run_issue = None;
        window.focus(&self.board_focus, cx);
        cx.notify();
    }

    /// The run screen's own keys. Escape goes back; nothing here is a
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

    /// Write one answer through core's atomic document write, then let the
    /// next scan move the run. The engine owns every other transition.
    fn submit_answer(
        &mut self,
        question: Question,
        answer: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = answer.trim().to_owned();
        if answer.is_empty() {
            self.run_answer_error = Some("Type an answer first.".to_owned());
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
        // door (S4b) share a writer and the engine owns every other transition.
        // A run this window was driving to its question continues on the same
        // click: the answer is what the engine waited for, and asking for a
        // second press would be one act behind two buttons.
        match self.host.answer(&issue, &question.path, &answer) {
            Ok(AnswerOutcome::Resumed | AnswerOutcome::Recorded) => {
                // Either way the file is written; the phase and the log say the
                // rest, so the click needs no toast of its own.
                self.run_answer_error = None;
                self.run_multi.clear();
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

    // ---- driving the run -------------------------------------------------

    /// Start a fresh run for the open screen's issue.
    fn start_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.start(&issue) {
            Ok(()) => self.run_action_error = None,
            Err(error) => self.run_action_error = Some(error),
        }
        cx.notify();
    }

    /// Ask the host to pause the open run at its next step boundary.
    fn pause_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.pause(&issue) {
            Ok(()) => self.run_action_error = None,
            Err(error) => self.run_action_error = Some(error),
        }
        cx.notify();
    }

    /// Continue the open run from its record, or take over a run another
    /// process is driving: the same takeover `warp-flow-cli run` does.
    fn resume_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
        match self.host.resume(&issue) {
            Ok(()) => self.run_action_error = None,
            Err(error) => self.run_action_error = Some(error),
        }
        cx.notify();
    }

    /// Abandon the open run at its next step boundary, after the confirmation.
    fn abandon_run(&mut self, issue: PathBuf, cx: &mut Context<Self>) {
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

    pub(crate) fn render_run_screen(
        &mut self,
        run: Run,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The host keys its runs by the issue folder; every action below uses
        // this one, which the scan resolved before the frame.
        self.run_issue = Some(run.dir.clone());
        let live_log = self.render_live_log(&run);
        let question = run.question.clone();
        let question_doc = question.as_ref().and_then(|question| {
            // Only the body is the editor's source; the path is not content.
            ensure_document(
                &mut self.run_question,
                &run.key,
                question.body.clone(),
                window,
                cx,
            )
        });

        let mut body = div()
            .id("flow-run-scroll")
            .debug_selector(|| "flow-run-scroll".into())
            .flex()
            .flex_col()
            .gap(px(theme::scaled(theme::RUN_GAP)))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.run_scroll)
            .track_focus(&self.run_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.handle_run_key(event, window, cx);
            }))
            .p(px(theme::scaled(theme::RUN_PADDING)));

        if let Some(error) = &self.run_action_error {
            body = body.child(action_error_line(error));
        }
        if let Some(log) = live_log {
            body = body.child(log);
        }
        if let Some(note) = &run.workflow_note {
            body = body.child(workflow_note(note));
        }
        body = body
            .child(self.render_run_steps(&run))
            .child(self.render_task_strip(&run));
        if let Some(question) = &question {
            body = body.child(self.render_question(question, question_doc.as_ref(), window, cx));
        }
        body = body
            .child(self.render_attempt_history(&run))
            .child(self.render_sessions(&run));

        div()
            .id("flow-run")
            .debug_selector(|| "flow-run".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(theme::surface())
            .child(self.render_run_head(&run, cx))
            .child(body)
            .into_any_element()
    }

    fn render_run_head(&self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("flow-run-head")
            .debug_selector(|| "flow-run-head".into())
            .flex_shrink_0()
            .flex()
            .items_start()
            .gap(px(12.))
            .px(px(theme::scaled(theme::RUN_PADDING)))
            .pt(px(18.))
            .pb(px(14.))
            .border_b_1()
            .border_color(theme::border_light())
            .child(
                controls::icon_button("flow-run-back-button", "Back to the board")
                    .debug_selector(|| "flow-run-back".into())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_run_screen(window, cx);
                    }))
                    .child(icon(
                        "icons/icon-arrow-left.svg",
                        px(theme::icon_small()),
                        theme::text_dim(),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .debug_selector(|| "flow-run-title".into())
                            .text_size(px(theme::font_screen_title()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_height(relative(1.2))
                            .text_color(theme::text())
                            .truncate()
                            .child(run.title.clone()),
                    )
                    .child(
                        div()
                            .debug_selector(|| "flow-run-meta".into())
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(5.))
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_mono_meta()))
                            .text_color(theme::text_muted())
                            .child(repo_label(&run.key.repo))
                            .child(div().text_color(theme::line_strong()).child("·"))
                            .child(run.key.id.clone())
                            .child(div().text_color(theme::line_strong()).child("·"))
                            .child(run.workflow.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted())
                            .child(format!(
                                "{} of {} tasks resolved",
                                run.tasks_done, run.tasks_total
                            ))
                            .child(div().text_color(theme::line_strong()).child("·"))
                            .child(attempt_word(run.attempts)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(8.))
                    .child(status_pill(run))
                    .child(self.render_run_actions(run, cx)),
            )
            .into_any_element()
    }

    // ---- the head's action row ---------------------------------------------

    /// The run's head actions, derived from the host's phase first and its
    /// record second, because a run that is being driven is the host's to say.
    ///
    /// Terminal runs keep their buttons and disable them with the reason in the
    /// tooltip; a start that is in flight shows the pause action instead.
    fn render_run_actions(&self, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let host_run = self.host_snapshot.run(&run.dir);
        let phase = host_run.map(|host_run| host_run.phase);
        let driving = phase.is_some_and(Phase::is_driving);
        let terminal =
            matches!(run.status.as_str(), "done" | "merged") || phase == Some(Phase::Done);
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
                pause_enabled,
                pause_reason,
                cx.listener(move |this, _, _, cx| this.pause_run(pause_issue.clone(), cx)),
            ));
            if self.run_abandon_confirm.as_ref() == Some(&run.key) {
                row = row.child(self.render_abandon_confirm(&issue, cx));
            } else {
                let key = run.key.clone();
                row = row.child(action_button(
                    "flow-run-abandon",
                    "Abandon",
                    !starting,
                    starting.then_some("The engine is starting."),
                    cx.listener(move |this, _, _, cx| {
                        this.run_abandon_confirm = Some(key.clone());
                        cx.notify();
                    }),
                ));
            }
        } else if phase == Some(Phase::Paused) {
            let resume_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-resume",
                "Resume",
                true,
                Some("Continue the paused run from its record."),
                cx.listener(move |this, _, _, cx| this.resume_run(resume_issue.clone(), cx)),
            ));
            if let Some(abandon) = self.render_abandon_offered(run, cx) {
                row = row.child(abandon);
            }
        } else if phase == Some(Phase::Waiting) && question_open {
            row = row.child(action_button(
                "flow-run-resume",
                "Resume",
                false,
                Some("Answer the open question first."),
                nothing,
            ));
        } else if phase == Some(Phase::Waiting) || (phase.is_none() && record_status == "waiting") {
            let resume_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-resume",
                "Resume",
                !question_open,
                question_open
                    .then_some("Answer the open question first.")
                    .or(Some("The question is answered; continue the run.")),
                cx.listener(move |this, _, _, cx| this.resume_run(resume_issue.clone(), cx)),
            ));
            if let Some(abandon) = self.render_abandon_offered(run, cx) {
                row = row.child(abandon);
            }
        } else if phase == Some(Phase::Failed) && has_record {
            let resume_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-resume",
                "Resume",
                true,
                Some("The engine stopped; resume to try again."),
                cx.listener(move |this, _, _, cx| this.resume_run(resume_issue.clone(), cx)),
            ));
            if let Some(abandon) = self.render_abandon_offered(run, cx) {
                row = row.child(abandon);
            }
        } else if terminal {
            row = row.child(action_button(
                "flow-run-start",
                "Start",
                false,
                Some("This run is done; there is nothing to start."),
                nothing,
            ));
        } else if has_record && (record_status == "running" || record_status == "paused") {
            // The takeover decision 17 describes: somebody else's run, or a
            // paused one this window restarted under, is resumed by hand.
            let reason = if record_status == "running" {
                "Not driven by this window; Resume takes it over."
            } else {
                "Not driven by this window; Resume continues the paused run."
            };
            let resume_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-resume",
                "Resume",
                true,
                Some(reason),
                cx.listener(move |this, _, _, cx| this.resume_run(resume_issue.clone(), cx)),
            ));
            if let Some(abandon) = self.render_abandon_offered(run, cx) {
                row = row.child(abandon);
            }
        } else if has_record && record_status == "blocked" {
            let resume_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-resume",
                "Resume",
                true,
                Some("Continue the run from its record."),
                cx.listener(move |this, _, _, cx| this.resume_run(resume_issue.clone(), cx)),
            ));
            if let Some(abandon) = self.render_abandon_offered(run, cx) {
                row = row.child(abandon);
            }
        } else {
            let start_issue = issue.clone();
            row = row.child(action_button(
                "flow-run-start",
                "Start",
                true,
                None,
                cx.listener(move |this, _, _, cx| this.start_run(start_issue.clone(), cx)),
            ));
        }
        row.into_any_element()
    }

    /// Abandon for a run no worker is driving, behind the same one-step
    /// confirmation the driving case uses.
    ///
    /// The host starts a resume worker that never runs a step: its first
    /// step-boundary check records the abandon. A record that is `done` or
    /// `merged` has nothing to abandon, so it gets no button.
    fn render_abandon_offered(&self, run: &Run, cx: &mut Context<Self>) -> Option<AnyElement> {
        if matches!(run.status.as_str(), "done" | "merged" | "abandoned") {
            return None;
        }
        let issue = run.dir.clone();
        if self.run_abandon_confirm.as_ref() == Some(&run.key) {
            return Some(self.render_abandon_confirm(&issue, cx));
        }
        let key = run.key.clone();
        Some(action_button(
            "flow-run-abandon",
            "Abandon",
            true,
            Some("Give up on this run; the engine records it and stops."),
            cx.listener(move |this, _, _, cx| {
                this.run_abandon_confirm = Some(key.clone());
                cx.notify();
            }),
        ))
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
                true,
                None,
                cx.listener(move |this, _, _, cx| {
                    this.abandon_run(yes_issue.clone(), cx);
                }),
            ))
            .child(action_button(
                "flow-run-abandon-no",
                "No",
                true,
                None,
                cx.listener(|this, _, _, cx| {
                    this.run_abandon_confirm = None;
                    cx.notify();
                }),
            ))
            .into_any_element()
    }

    // ---- the live engine log -----------------------------------------------

    /// The engine's own log while this window has the run, newest last, with
    /// the phase in a word so the motion is never the only signal.
    fn render_live_log(&self, run: &Run) -> Option<AnyElement> {
        let host_run = self.host_snapshot.run(&run.dir)?;
        let phase = host_run.phase;
        let color = phase_color(phase);
        let mut head = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(theme::font_row_meta()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child("Engine log"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .debug_selector(|| "flow-run-engine-phase".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(color)
                    .child(phase_word(phase)),
            );
        // One motion signal on this surface: the engine log's head carries the
        // spinner, and only while the engine is moving.
        if phase_moves(phase) {
            head = head.child(motion::spinner(
                "flow-run-engine-spinner",
                "icons/icon-spinner.svg",
                px(theme::icon_tiny()),
                color,
            ));
        }

        let mut lines = div()
            .id("flow-run-engine-log-scroll")
            .flex()
            .flex_col()
            .max_h(px(170.))
            .overflow_y_scroll()
            .track_scroll(&self.run_log_scroll);
        if host_run.log.is_empty() {
            lines = lines.child(quiet_line("The engine has not written a line yet."));
        }
        for (index, line) in host_run.log.iter().enumerate() {
            let selector = format!("flow-run-engine-line-{index}");
            lines = lines.child(
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector.clone())
                    .whitespace_nowrap()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_dim())
                    .child(line.clone()),
            );
        }

        Some(
            div()
                .id("flow-run-engine-log")
                .debug_selector(|| "flow-run-engine-log".into())
                .flex()
                .flex_col()
                .gap(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(theme::hairline())
                .bg(theme::panel())
                .p(px(12.))
                .child(head)
                .child(lines)
                .into_any_element(),
        )
    }

    // ---- the workflow graph, as step bands ---------------------------------

    fn render_run_steps(&self, run: &Run) -> AnyElement {
        let mut section = div()
            .id("flow-run-workflow")
            .debug_selector(|| "flow-run-workflow".into())
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(section_head(
                "Workflow",
                if run.steps.is_empty() {
                    String::new()
                } else {
                    format!("{} steps · top to bottom", run.steps.len())
                },
            ));
        if run.steps.is_empty() {
            section = section.child(quiet_line("No steps to show."));
        }
        for step in &run.steps {
            section = section.child(self.render_step_band(step));
        }
        section.into_any_element()
    }

    fn render_step_band(&self, step: &StepView) -> AnyElement {
        let selector = format!("flow-run-step-{}", step.id);
        let state_selector = format!("flow-run-state-{}", step.id);
        let mut head = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(9.))
            .child(state_dot(step.state))
            .child(
                div()
                    .font_family(theme::code_font())
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(px(theme::font_control()))
                    .text_color(theme::text())
                    .child(step.id.clone()),
            )
            .child(tag(kind_word(step.kind).to_owned()));
        if !step.detail.is_empty() {
            head = head.child(
                div()
                    .max_w(px(300.))
                    .truncate()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(step.detail.clone()),
            );
        }
        head = head.child(div().flex_1());
        if let Some(note) = &step.note {
            head = head.child(
                div()
                    .max_w(px(300.))
                    .truncate()
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(note.clone()),
            );
        }
        head = head.child(state_chip(state_selector, step.state));

        let mut band = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::panel())
            .child(head);
        if step.per_task {
            if step.lanes.is_empty() {
                band = band.child(
                    div()
                        .border_t_1()
                        .border_color(theme::hairline())
                        .bg(theme::surface())
                        .px(px(12.))
                        .py(px(8.))
                        .text_size(px(theme::font_meta()))
                        .text_color(theme::text_muted())
                        .child("No task files yet."),
                );
            }
            for lane in &step.lanes {
                band = band.child(self.render_lane(&step.id, lane));
            }
        }
        band.into_any_element()
    }

    fn render_lane(&self, step: &str, lane: &LaneView) -> AnyElement {
        let selector = format!("flow-run-lane-{step}-{:02}", lane.number);
        let state_selector = format!("flow-run-state-{step}-{:02}", lane.number);
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap(px(8.))
            .border_t_1()
            .border_color(theme::hairline())
            .bg(theme::surface())
            .px(px(12.))
            .py(px(7.))
            .child(state_dot(lane.state))
            .child(
                div()
                    .w(px(52.))
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted())
                    .child(format!("task {:02}", lane.number)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::font_meta()))
                    .text_color(theme::text_dim())
                    .child(lane.title.clone()),
            )
            .child(state_chip(state_selector, lane.state))
            .into_any_element()
    }

    // ---- the task dependency strip -----------------------------------------

    fn render_task_strip(&self, run: &Run) -> AnyElement {
        let has_blockers = run.task_rows.iter().any(|task| !task.blocked_by.is_empty());
        let mut strip = div()
            .id("flow-run-task-strip")
            .debug_selector(|| "flow-run-task-strip".into())
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::panel())
            .p(px(12.))
            .child(section_head(
                "Tasks",
                if run.task_rows.is_empty() {
                    String::new()
                } else {
                    format!(
                        "{} task{}",
                        run.task_rows.len(),
                        if run.task_rows.len() == 1 { "" } else { "s" }
                    )
                },
            ));

        if run.task_levels.is_empty() {
            strip = strip.child(quiet_line("No tasks yet. The split step writes them."));
            return strip.into_any_element();
        }

        let mut row = div().flex().flex_wrap().items_center().gap(px(4.));
        for (level_index, level) in run.task_levels.iter().enumerate() {
            if level_index > 0 {
                let selector = format!("flow-run-arrow-{level_index}");
                row = row.child(
                    div()
                        .debug_selector(move || selector.clone())
                        .px(px(3.))
                        .text_size(px(theme::font_meta()))
                        .text_color(theme::text_muted())
                        .child("→"),
                );
            }
            let level_selector = format!("flow-run-level-{level_index}");
            let mut group = div()
                .id(SharedString::from(level_selector.clone()))
                .debug_selector(move || level_selector.clone())
                .flex()
                .items_center()
                .gap(px(4.));
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
                let task = run.task_rows.iter().find(|task| task.number == *number);
                group = group.child(task_chip(*number, task));
            }
            row = row.child(group);
        }
        strip = strip.child(row).child(
            div()
                .debug_selector(|| "flow-run-task-rule".into())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted())
                .child(if has_blockers {
                    "a blocked task starts only after its blocker has merged"
                } else {
                    "every task is independent"
                }),
        );
        strip.into_any_element()
    }

    // ---- the open question and its answers ---------------------------------

    fn render_question(
        &mut self,
        question: &Question,
        body: Option<&Entity<LiveEditorState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let document = match body {
            Some(editor) => warpui_kit_markdown::render_read_only_document_in_card(editor, cx),
            None => quiet_line("The question has no body."),
        };
        let mut card = div()
            .id("flow-run-question")
            .debug_selector(|| "flow-run-question".into())
            .flex()
            .flex_col()
            .gap(px(10.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::WARN)
            .bg(theme::tint(theme::WARN))
            .p(px(14.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(theme::font_micro()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::WARN)
                            .child("OPEN QUESTION"),
                    )
                    .child(tag(question.kind.clone()))
                    .child(
                        div()
                            .font_family(theme::code_font())
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted())
                            .child(format!("asked by {}", question.asked_by)),
                    )
                    .when_some(question.task.clone(), |row, task| {
                        row.child(
                            div()
                                .font_family(theme::code_font())
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::text_muted())
                                .child(format!("task {task}")),
                        )
                    }),
            )
            .child(
                div()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::hairline())
                    .bg(theme::surface())
                    .p(px(12.))
                    .child(document),
            )
            .child(self.render_answer_controls(question, window, cx));
        if let Some(error) = &self.run_answer_error {
            card = card.child(
                div()
                    .debug_selector(|| "flow-answer-error".into())
                    .font_family(theme::code_font())
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::DANGER)
                    .child(error.clone()),
            );
        }
        card.into_any_element()
    }

    fn render_answer_controls(
        &mut self,
        question: &Question,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match question.kind.as_str() {
            "approve" => {
                let mut row = div().flex().items_center().gap(px(8.));
                for (id, label) in [("flow-answer-yes", "Yes"), ("flow-answer-no", "No")] {
                    let question = question.clone();
                    let answer = label.to_lowercase();
                    row = row.child(probed(
                        id,
                        controls::button(
                            format!("{id}-button"),
                            label,
                            BtnKind::Primary(theme::on_accent()),
                            theme::accent(),
                            theme::font_control(),
                            true,
                            cx.listener(move |this, _, window, cx| {
                                this.submit_answer(question.clone(), answer.clone(), window, cx);
                            }),
                        ),
                    ));
                }
                row.into_any_element()
            }
            "select" if !question.options.is_empty() => {
                let mut rows = div().flex().flex_col().gap(px(4.));
                for (index, option) in question.options.iter().enumerate() {
                    rows =
                        rows.child(self.render_option(question, index, option, false, false, cx));
                }
                rows.into_any_element()
            }
            "multi" if !question.options.is_empty() => {
                let mut rows = div().flex().flex_col().gap(px(4.));
                for (index, option) in question.options.iter().enumerate() {
                    let checked = self.run_multi.contains(option);
                    rows =
                        rows.child(self.render_option(question, index, option, true, checked, cx));
                }
                let question_for_submit = question.clone();
                let enabled = !self.run_multi.is_empty();
                rows = rows.child(div().mt(px(6.)).flex().items_center().gap(px(8.)).child(
                    probed(
                        "flow-answer-submit",
                        controls::button(
                            "flow-answer-submit-button",
                            "Submit",
                            BtnKind::Primary(theme::on_accent()),
                            theme::accent(),
                            theme::font_control(),
                            enabled,
                            cx.listener(move |this, _, window, cx| {
                                if enabled {
                                    let answer = this.run_multi.join(", ");
                                    this.submit_answer(
                                        question_for_submit.clone(),
                                        answer,
                                        window,
                                        cx,
                                    );
                                }
                            }),
                        ),
                    ),
                ));
                rows.into_any_element()
            }
            _ => self.render_text_answer(question, window, cx),
        }
    }

    /// One choice row. A `select` row answers on click; a `multi` row ticks.
    fn render_option(
        &mut self,
        question: &Question,
        index: usize,
        label: &str,
        multi: bool,
        checked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selector = format!("flow-answer-option-{index}");
        let id = format!("flow-answer-row-{index}");
        let question = question.clone();
        let choice = label.to_owned();
        let mut row = div()
            .id(SharedString::from(id))
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(theme::line_strong())
            .bg(theme::panel())
            .px(px(10.))
            .py(px(7.))
            .text_size(px(theme::font_control()))
            .text_color(theme::text())
            .cursor_pointer()
            .hover(|style| style.bg(theme::raised()))
            .on_click(cx.listener(move |this, _, window, cx| {
                if multi {
                    this.toggle_multi(choice.clone());
                    cx.notify();
                } else {
                    this.submit_answer(question.clone(), choice.clone(), window, cx);
                }
            }))
            .child(div().flex_1().min_w_0().truncate().child(label.to_owned()));
        if multi {
            row = row.child(choice_box(checked));
        }
        row = if multi {
            row.a11y_toggle(label.to_owned(), checked)
        } else {
            row.a11y_button(label.to_owned())
        };
        row.into_any_element()
    }

    /// The `text` kind, and any kind this screen does not know.
    fn render_text_answer(
        &mut self,
        question: &Question,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let question_for_submit = question.clone();
        let value = self.answer_field.read(cx).value().to_string();
        let enabled = !value.trim().is_empty();
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(warpui_kit_core::field::text(
                "flow-answer-field",
                &self.answer_field,
                window,
                cx,
            ))
            .child(div().flex().items_center().gap(px(8.)).child(probed(
                "flow-answer-submit",
                controls::button(
                    "flow-answer-submit-button",
                    "Submit",
                    BtnKind::Primary(theme::on_accent()),
                    theme::accent(),
                    theme::font_control(),
                    enabled,
                    cx.listener(move |this, _, window, cx| {
                        let answer = this.answer_field.read(cx).value().to_string();
                        this.submit_answer(question_for_submit.clone(), answer, window, cx);
                    }),
                ),
            )))
            .into_any_element()
    }

    // ---- attempt history ---------------------------------------------------

    fn render_attempt_history(&self, run: &Run) -> AnyElement {
        let groups = store::group_log(&run.rows);
        let mut card = div()
            .id("flow-run-log")
            .debug_selector(|| "flow-run-log".into())
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::panel())
            .p(px(12.))
            .child(section_head("Attempt history", attempt_word(run.attempts)));
        if groups.is_empty() {
            card = card.child(quiet_line("No attempts recorded yet."));
            return card.into_any_element();
        }
        for (group_index, group) in groups.iter().enumerate() {
            let selector = format!("flow-run-log-group-{group_index}");
            let mut group_card = div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .flex()
                .flex_col()
                .rounded(px(6.))
                .border_1()
                .border_color(theme::border_light())
                .bg(theme::surface());
            // `step · task 01` or, for a step that runs once, the step alone:
            // "test · run" reads as one phrase about a test run, not as a step.
            let mut group_head = div()
                .flex()
                .items_center()
                .gap(px(6.))
                .border_b_1()
                .border_color(theme::border_light())
                .px(px(10.))
                .py(px(6.))
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted())
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_dim())
                        .child(group.step.clone()),
                );
            if !group.task.is_empty() && group.task != "-" {
                group_head = group_head
                    .child(div().text_color(theme::line_strong()).child("·"))
                    .child(format!("task {}", group.task));
            }
            group_card = group_card.child(
                group_head
                    .child(div().flex_1())
                    .child(attempt_word(group.rows.len())),
            );
            for (row_index, row) in group.rows.iter().enumerate() {
                let selector = format!("flow-run-log-row-{group_index}-{row_index}");
                group_card = group_card.child(attempt_row(row, selector));
            }
            card = card.child(group_card);
        }
        card.into_any_element()
    }

    /// What the harness sessions said, the newest last.
    ///
    /// The engine writes `run.md`; the sessions are what a person actually wants
    /// to read — the ask, the answer and the tools a step ran. Each is one
    /// markdown file the harness wrote into `sessions/`, shown with the session
    /// it belongs to, in the order it settled.
    fn render_sessions(&self, run: &Run) -> AnyElement {
        let mut card = div()
            .id("flow-run-sessions")
            .debug_selector(|| "flow-run-sessions".into())
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme::hairline())
            .bg(theme::panel())
            .p(px(12.))
            .child(section_head("Sessions", sessions_word(run.sessions.len())));
        if run.sessions.is_empty() {
            card = card.child(quiet_line(
                "No session recorded yet. A harness step writes one when it settles.",
            ));
            return card.into_any_element();
        }
        for (index, session) in run.sessions.iter().enumerate() {
            let selector = format!("flow-run-session-{index}");
            let mut head = div()
                .flex()
                .items_center()
                .gap(px(8.))
                .border_b_1()
                .border_color(theme::border_light())
                .px(px(10.))
                .py(px(6.))
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted())
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_dim())
                        .child(session.step.clone()),
                );
            if let Some(task) = &session.task {
                head = head.child(format!("task {task}"));
            }
            head = head
                .child(format!("attempt {}", session.attempt))
                .child(format!("{} · {}", session.harness, session.session))
                .child(div().flex_1())
                .child(
                    div()
                        .text_color(match session.status.as_str() {
                            "done" => theme::ADD,
                            "failed" | "timed-out" => theme::DANGER,
                            _ => theme::text_muted(),
                        })
                        .child(session.status.clone()),
                );
            // the transcript's tail: a session can be long, and the run screen
            // is a page of sections, not one document. Lines, not one text run,
            // so a transcript that wraps reads as a log, like the engine's own.
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
                .max_h(px(240.))
                .overflow_y_scroll()
                .px(px(10.))
                .py(px(8.))
                .gap(px(1.))
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
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
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::border_light())
                    .bg(theme::surface())
                    .child(head)
                    .child(text),
            );
        }
        card.into_any_element()
    }
}

// ---- small rendering helpers ----------------------------------------------

/// How many lines of a session's transcript the run screen shows: the tail is
/// what a person reads, and the panel must not become the page.
const SESSION_LINES: usize = 40;

/// `1 session` / `3 sessions`.
fn sessions_word(count: usize) -> String {
    if count == 1 {
        "1 session".to_owned()
    } else {
        format!("{count} sessions")
    }
}

fn section_head(title: &str, meta: String) -> AnyElement {
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .text_size(px(theme::font_row_meta()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title.to_owned()),
        )
        .child(div().flex_1());
    if !meta.is_empty() {
        row = row.child(
            div()
                .font_family(theme::code_font())
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted())
                .child(meta),
        );
    }
    row.into_any_element()
}

fn quiet_line(message: &str) -> AnyElement {
    div()
        .text_size(px(theme::font_meta()))
        .text_color(theme::text_muted())
        .child(message.to_owned())
        .into_any_element()
}

fn workflow_note(note: &str) -> AnyElement {
    div()
        .debug_selector(|| "flow-run-workflow-note".into())
        .rounded(px(6.))
        .bg(theme::tint(theme::WARN))
        .px(px(10.))
        .py(px(8.))
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::WARN)
        .child(note.to_owned())
        .into_any_element()
}

fn state_color(state: StepState) -> gpui::Rgba {
    match state {
        StepState::Done => theme::ADD,
        StepState::Running => theme::accent_ink(),
        StepState::Failed => theme::DANGER,
        StepState::Waiting => theme::WARN,
        StepState::Idle => theme::text_muted(),
    }
}

fn state_dot(state: StepState) -> AnyElement {
    div()
        .size(px(7.))
        .rounded_full()
        .bg(state_color(state))
        .into_any_element()
}

/// The state word as a chip. The step's note sits beside it in the mono face,
/// so the two must read as two things: `last outcome: done` next to a bare
/// `done` reads as one repeated word.
fn state_chip(selector: String, state: StepState) -> AnyElement {
    let color = state_color(state);
    div()
        .debug_selector(move || selector.clone())
        .flex_shrink_0()
        .flex()
        .items_center()
        .h(px(20.))
        .px(px(7.))
        .rounded(px(999.))
        .bg(theme::tint(color))
        .text_size(px(theme::font_micro()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(state.label())
        .into_any_element()
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

fn task_status_color(status: Status) -> gpui::Rgba {
    match status {
        Status::Resolved => theme::ADD,
        Status::ReadyForAgent | Status::ReadyForHuman => theme::accent_ink(),
        Status::NeedsTriage | Status::NeedsInfo => theme::text_muted(),
        Status::Wontfix => theme::DANGER,
    }
}

/// One task chip in the dependency strip, coloured by the task file's status.
fn task_chip(number: u32, task: Option<&TaskRow>) -> AnyElement {
    let selector = format!("flow-run-task-{number:02}");
    let color = task.map_or(theme::text_muted(), |task| task_status_color(task.status));
    let label = format!("{number:02}");
    let accessible = task.map_or_else(
        || label.clone(),
        |task| format!("{number:02} {}", task.title),
    );
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .role(gpui::Role::Status)
        .aria_label(accessible)
        .flex()
        .items_center()
        .h(px(24.))
        .px(px(8.))
        .rounded(px(6.))
        .border_1()
        .border_color(color)
        .bg(theme::tint(color))
        .font_family(theme::code_font())
        .text_size(px(theme::font_mono_meta()))
        .text_color(color)
        .truncate()
        .child(label)
        .into_any_element()
}

/// A check box for a choice row; the check glyph itself is not an asset.
fn choice_box(checked: bool) -> AnyElement {
    let mut boxed = div()
        .size(px(16.))
        .flex_shrink_0()
        .rounded(px(4.))
        .border_1()
        .border_color(if checked {
            theme::accent_ink()
        } else {
            theme::line_strong()
        })
        .bg(if checked {
            theme::tint(theme::accent_ink())
        } else {
            theme::well().into()
        })
        .flex()
        .items_center()
        .justify_center();
    if checked {
        boxed = boxed.child(
            div()
                .text_size(px(theme::font_micro()))
                .text_color(theme::accent_ink())
                .child("✓"),
        );
    }
    boxed.into_any_element()
}

/// A shared button under its own debug selector: the kit button does not take
/// one, and the probes click the wrapper's centre.
fn probed(id: &'static str, element: AnyElement) -> AnyElement {
    div()
        .debug_selector(move || id.into())
        .child(element)
        .into_any_element()
}

/// One head action: a quiet kit button under its own debug selector, with the
/// reason a disabled button is disabled in its tooltip.
fn action_button(
    id: &'static str,
    label: &str,
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
    wrapper
        .child(controls::button(
            format!("{id}-button"),
            label.to_owned(),
            BtnKind::Chip,
            theme::accent(),
            theme::font_control(),
            enabled,
            on_click,
        ))
        .into_any_element()
}

/// The action a disabled button never runs: the kit keeps the handler wired,
/// so the caller's guard is this no-op.
fn nothing(_: &gpui::ClickEvent, _: &mut Window, _: &mut gpui::App) {}

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

/// Whether the phase is a moving one, which is where the log head's spinner
/// shows. A paused or waiting run is still, and reduced motion keeps the word.
fn phase_moves(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Starting | Phase::Running | Phase::Pausing | Phase::Finishing
    )
}

/// The phase's colour, from the run screen's own state palette.
fn phase_color(phase: Phase) -> gpui::Rgba {
    match phase {
        Phase::Starting | Phase::Running | Phase::Pausing => theme::accent_ink(),
        Phase::Paused => theme::accent_ink(),
        Phase::Waiting => theme::WARN,
        Phase::Finishing => theme::text_muted(),
        Phase::Done => theme::ADD,
        Phase::Blocked | Phase::Abandoned | Phase::Failed => theme::DANGER,
    }
}

/// One readable line for a start, resume, pause or abandon the host refused.
fn action_error_line(error: &str) -> AnyElement {
    div()
        .id("flow-run-action-error-line")
        .debug_selector(|| "flow-run-action-error".into())
        .rounded(px(6.))
        .bg(theme::tint(theme::DANGER))
        .px(px(10.))
        .py(px(8.))
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::DANGER)
        .child(error.to_owned())
        .into_any_element()
}

fn attempt_row(row: &store::RunRow, selector: String) -> AnyElement {
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(10.))
        .py(px(4.))
        .font_family(theme::code_font())
        .text_size(px(theme::font_micro()))
        .text_color(theme::text_dim())
        .whitespace_nowrap()
        .child(
            div()
                .w(px(64.))
                .text_color(theme::text_muted())
                .child(format!("attempt {}", row.attempt)),
        )
        .child(
            div()
                .w(px(96.))
                .truncate()
                .text_color(state_color(outcome_state(&row.outcome)))
                .child(row.outcome.clone()),
        )
        .child(div().flex_1().min_w_0().truncate().child(row.at.clone()))
        .into_any_element()
}
