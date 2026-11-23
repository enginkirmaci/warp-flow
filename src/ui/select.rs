//! The app's select: a `.field`-shaped trigger and the list it opens.
//!
//! Three step fields are picked rather than typed — the harness a step runs on,
//! one of that harness's models, one of that model's efforts. They are what the
//! door publishes (`core::door::read_catalog`), so a field offering them as a
//! list of names cannot be given a name that does not exist, and the empty
//! choice carries a meaning of its own (the engine's choice, the harness's
//! first model, the model's default effort) instead of being a blank box.
//!
//! The trigger is the family's `.field` box, so a select sits in a form beside
//! the text fields without a second look. The list is deliberately not a child
//! of the row: a catalog long enough to matter would be clipped by the form's
//! scroll container and could not open past the pane's edge. So the row reports
//! where its trigger was drawn ([`Anchors`]) and the caller renders [`panel`]
//! once, as the last child of the pane that owns the form — a window-sized
//! catcher with the card anchored under the trigger in window coordinates. That
//! is the family's menu pattern: the catcher makes a click anywhere else
//! dismiss the list, and `anchored` keeps a long list on screen.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, ClickEvent, FocusHandle, KeyDownEvent, Pixels, SharedString, Window,
    anchored, div, point, px,
};
use gpui_kit::base::ElementExt as _;
use warpui_kit_core::A11y as _;
use warpui_kit_core::{icon, shell};

use crate::theme;

/// The open list's narrowest width: the trigger's own width when it is wider,
/// so a model key and the harness beside it still fit.
const PANEL_MIN_WIDTH: f32 = 300.;

/// The open list's ceiling before it scrolls, so the rows under it stay
/// reachable with the list open.
const PANEL_MAX_HEIGHT: f32 = 320.;

/// A form row's own vertical padding around the field.
const ROW_PADDING_Y: f32 = 6.;

/// A pick: the value a row writes back to the caller that opened the list.
pub type PickFn = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// A dismissal: the caller closes the list it opened.
pub type DismissFn = Rc<dyn Fn(&mut App)>;

/// One row the list offers: the value the graph would carry, and how it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// The value a pick writes: a harness id, a model key, an effort variant.
    pub value: String,
    /// The row's label. A harness id or a model key is its own label.
    pub label: String,
    /// A quiet word after the label: the harness a model belongs to, or a
    /// harness's long name beside its id.
    pub hint: Option<String>,
}

impl Choice {
    /// A choice whose label is its value.
    pub fn new(value: &str) -> Self {
        Self {
            value: value.to_owned(),
            label: value.to_owned(),
            hint: None,
        }
    }

    /// The same choice with a quiet word after the label.
    pub fn with_hint(mut self, hint: Option<String>) -> Self {
        self.hint = hint;
        self
    }
}

/// What one select draws: its label, its value, and what it offers.
pub struct Select<'a> {
    /// The selector base. The trigger answers to `id`, a row to `id-<value>`
    /// and the clear row to `id-clear`, so a probe reaches any of them by name.
    pub id: &'a str,
    /// The row's label in the form.
    pub label: &'a str,
    /// What the graph holds, shown in the trigger.
    pub value: Option<String>,
    /// The trigger's text when nothing is picked, which is what the empty
    /// choice means: "The engine's choice", "The harness's first".
    pub unset: &'a str,
    /// The rows after it, in the door's order.
    pub choices: Vec<Choice>,
    /// One line above the rows. A list that is empty, or that waits on another
    /// field, says so here instead of showing nothing.
    pub note: Option<String>,
    /// Whether the list is showing.
    pub open: bool,
}

/// Where an open select's trigger was drawn, in window pixels.
///
/// The list is rendered by the pane rather than by the row, so the row has to
/// report its own geometry. It is a `Cell` because prepaint is where bounds are
/// known and it hands out `&`, and it is keyed by the field because all three
/// triggers prepaint in the same frame.
/// One trigger's drawn position: the field's selector base and where it was.
type Slot = (String, Bounds<Pixels>);

#[derive(Clone, Default)]
pub struct Anchors(Rc<RefCell<Vec<Slot>>>);

impl Anchors {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `id`'s trigger bounds, as its prepaint does.
    ///
    /// Every field keeps its own slot: all three triggers prepaint in one
    /// frame, and the last one to prepaint is not the one whose list is open.
    fn set(&self, id: &str, bounds: Bounds<Pixels>) {
        let mut slots = self.0.borrow_mut();
        match slots.iter_mut().find(|(slot, _)| slot == id) {
            Some((_, slot)) => *slot = bounds,
            None => slots.push((id.to_owned(), bounds)),
        }
    }

    /// Where `id`'s trigger was last drawn.
    pub fn bounds_of(&self, id: &str) -> Option<Bounds<Pixels>> {
        self.0
            .borrow()
            .iter()
            .find(|(slot, _)| slot == id)
            .map(|(_, bounds)| *bounds)
    }
}

/// One field of the form: its label, its trigger, and the click that opens it.
///
/// `label_width` is the form's own label column, so a select lines up with the
/// text fields around it. `on_toggle` opens or closes the list and `on_dismiss`
/// closes it from Escape.
pub fn row(
    select: &Select<'_>,
    label_width: f32,
    anchors: &Anchors,
    focus: &FocusHandle,
    on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_dismiss: impl Fn(&mut App) + 'static,
    window: &Window,
) -> AnyElement {
    let id = select.id;
    let measured = anchors.clone();
    let measured_id = id.to_owned();
    let value = select.value.clone();
    let open = select.open;

    // the id becomes the box's element id (and its debug selector), which the
    // kit takes as an owned name
    let trigger = warpui_kit_core::field::boxed(
        SharedString::from(id.to_owned()),
        focus,
        window,
        shell::field_height(),
    )
    .cursor_pointer()
    .a11y_button(format!(
        "{}: {}",
        select.label,
        value.as_deref().unwrap_or(select.unset)
    ))
    .aria_expanded(open)
    .on_click(on_toggle)
    .on_key_down(move |event: &KeyDownEvent, _, cx| {
        if event.keystroke.key == "escape" {
            on_dismiss(cx);
            cx.stop_propagation();
        }
    })
    .child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .font_family(theme::code_font())
            .text_size(px(theme::font_mono_meta()))
            .when(value.is_none(), |text| text.text_color(theme::text_muted()))
            .child(match value {
                Some(value) => value,
                None => select.unset.to_owned(),
            }),
    )
    .child(icon(
        if open {
            "icons/icon-chevron-up.svg"
        } else {
            "icons/icon-chevron-down.svg"
        },
        px(theme::icon_small()),
        theme::text_muted(),
    ));

    div()
        .debug_selector({
            let selector = format!("{id}-row");
            move || selector.clone()
        })
        .flex()
        .items_center()
        .gap(px(12.))
        .min_h(shell::field_height())
        .py(px(ROW_PADDING_Y))
        .child(
            div()
                .flex_shrink_0()
                .w(px(label_width))
                .text_size(px(theme::font_label()))
                .text_color(theme::text_dim())
                .child(select.label.to_owned()),
        )
        // the trigger measures itself, which is what the floating list opens
        // against: a row never knows where it landed, and prepaint does
        .child(
            div()
                .flex_1()
                .min_w_0()
                .on_prepaint(move |bounds, _, _| measured.set(&measured_id, bounds))
                .child(trigger),
        )
        .into_any_element()
}

/// The open list: a pane-sized catcher with the card anchored under the open
/// trigger.
///
/// Render it once, as the last child of the pane that owns the form, so it
/// paints over the form and is not clipped by the form's scroller. Returns
/// `None` on every frame but the one where a select is open.
pub fn panel(
    select: &Select<'_>,
    anchors: &Anchors,
    on_pick: PickFn,
    on_dismiss: DismissFn,
) -> Option<AnyElement> {
    if !select.open {
        return None;
    }
    let at = anchors.bounds_of(select.id)?;
    let position = point(at.origin.x, at.origin.y + at.size.height);
    let width = at.size.width.max(px(shell::scaled(PANEL_MIN_WIDTH)));

    let mut rows = div()
        .id(SharedString::from(format!("{}-rows", select.id)))
        .max_h(px(shell::scaled(PANEL_MAX_HEIGHT)))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(2.));
    if let Some(note) = &select.note {
        rows = rows.child(
            div()
                .debug_selector({
                    let selector = format!("{}-note", select.id);
                    move || selector.clone()
                })
                .px(px(8.))
                .py(px(6.))
                .text_size(px(theme::font_sub()))
                .text_color(theme::text_muted())
                .child(note.clone()),
        );
    }
    rows = rows.child(option_row(
        &format!("{}-clear", select.id),
        select.unset,
        None,
        select.value.is_none(),
        {
            let on_pick = Rc::clone(&on_pick);
            move |_: &ClickEvent, window, cx| on_pick.as_ref()("", window, cx)
        },
    ));
    for choice in &select.choices {
        let selected = select.value.as_deref() == Some(choice.value.as_str());
        rows = rows.child(option_row(
            &format!("{}-{}", select.id, choice.value),
            &choice.label,
            choice.hint.as_deref(),
            selected,
            {
                let on_pick = Rc::clone(&on_pick);
                let value = choice.value.clone();
                move |_: &ClickEvent, window, cx| on_pick.as_ref()(&value, window, cx)
            },
        ));
    }

    let card = div()
        .id(SharedString::from(format!("{}-panel", select.id)))
        .debug_selector({
            let selector = format!("{}-panel", select.id);
            move || selector.clone()
        })
        .flex()
        .flex_col()
        .w(width)
        // the card is opaque to the catcher beneath it: without this a press on
        // a row reaches the catcher too, which closes the list before the row's
        // own click lands
        .occlude()
        .rounded(px(8.))
        .border_1()
        .border_color(theme::line_strong())
        .bg(theme::popup())
        .shadow_lg()
        .p(px(4.))
        .child(rows);

    // the catcher covers the pane, so a press anywhere in it closes the list;
    // the card sits above it, and a press on a row lands on the row instead
    let catcher = div()
        .id(SharedString::from(format!("{}-catcher", select.id)))
        .debug_selector({
            let selector = format!("{}-catcher", select.id);
            move || selector.clone()
        })
        .absolute()
        .inset_0()
        .occlude()
        .on_mouse_down(gpui::MouseButton::Left, {
            let on_dismiss = Rc::clone(&on_dismiss);
            move |_, _, cx| on_dismiss.as_ref()(cx)
        })
        .on_mouse_down_out({
            let on_dismiss = Rc::clone(&on_dismiss);
            move |_, _, cx| on_dismiss.as_ref()(cx)
        })
        .child(
            anchored()
                .position(position)
                .offset(point(px(0.), px(4.)))
                .snap_to_window_with_margin(px(8.))
                .child(card),
        );

    Some(catcher.into_any_element())
}

/// One row of the list: the check column, the label, the quiet hint.
fn option_row(
    id: &str,
    label: &str,
    hint: Option<&str>,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let selector = id.to_owned();
    div()
        .id(SharedString::from(id.to_owned()))
        .debug_selector(move || selector.clone())
        .a11y_option(label.to_owned(), selected)
        .min_h(px(theme::scaled(28.)))
        .w_full()
        .px(px(8.))
        .py(px(4.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|style| style.bg(theme::raised()))
        .on_click(on_click)
        .child(
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .w(px(theme::icon_small()))
                .child(if selected {
                    icon(
                        "icons/icon-check.svg",
                        px(theme::icon_small()),
                        theme::accent(),
                    )
                    .into_any_element()
                } else {
                    div().into_any_element()
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(theme::code_font())
                .text_size(px(theme::font_mono_meta()))
                .text_color(if selected {
                    theme::text()
                } else {
                    theme::text_dim()
                })
                .child(label.to_owned()),
        )
        .children(hint.map(|hint| {
            div()
                .flex_shrink_0()
                .text_size(px(theme::font_sub()))
                .text_color(theme::text_muted())
                .child(hint.to_owned())
        }))
        .into_any_element()
}
