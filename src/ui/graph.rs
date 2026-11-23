//! The workflow as a picture: nodes, forward edges, and back-edges.
//!
//! The editor and the run screen both need the same thing — a node-link diagram
//! that shows what the file says. The graph is a list of steps with
//! dependencies and a list of edges, and neither of those is a picture; this
//! module is where the list becomes one.
//!
//! # Layout
//!
//! Layered, top to bottom — the orientation a workflow reads in, and the one a
//! chain of twelve steps fits a side panel in:
//!
//! 1. **Rank** each step by the longest path from a root. A step's rank is one
//!    past the rank of its deepest dependency, so a chain is a column of rows
//!    and a fork opens a new column for each branch.
//! 2. **Order** the steps of a rank by the mean position of their parents — a
//!    single barycenter sweep, which is enough for the dozen-or-so steps a
//!    workflow has.
//! 3. **Place** them: `y` from the rank, `x` from the order, then centre each
//!    step on its parents so a join does not look like a branch.
//!
//! A **back-edge** is drawn as a loop on the right: out of the sender's right
//! side, around, and into the target's right side. That is the one shape the
//! diagram exists to show, and it is why the layout leaves room to its right.
//! A routed link enters its target at its own height on the target's right
//! edge, so two loops into one step are two arrivals rather than one arrowhead
//! drawn twice.
//!
//! A **forward dependency that skips a rank** is routed the same way: a
//! straight line between the two facing edges would run through every box
//! between them. It leaves the sender's right side, runs its vertical stretch
//! in a lane of its own just beside the boxes it skips — inside the picture
//! when those ranks leave the room, outside it when they do not — and comes
//! back in at the target's right side. A dependency between adjacent ranks is
//! still the straight facing-edge line it always was.
//!
//! # Scale
//!
//! The shell measures everything through `theme::scaled`, so a larger
//! text-size setting grows the labels inside the boxes. [`Metrics`] is every
//! length this module draws from at one scale: [`layout`] places the picture at
//! the design's own size, [`live_layout`] at the running one, and the layout
//! math is the same pure function either way.
//!
//! # Drawing
//!
//! Edges are painted on one [`canvas`]: a small number of elements rather than
//! one per edge, and the same trick `warpui-kit-diff` uses for its row washes.
//! The nodes are ordinary `div`s laid over the canvas by the caller, so they
//! keep their text, their hover, and their accessibility.

use std::collections::{BTreeMap, BTreeSet};

use gpui::prelude::*;
use gpui::{
    AnyElement, Background, Hsla, IntoElement, PathBuilder, Pixels, Point, Window, canvas, point,
    px,
};
use warp_flow_core::graph::{StepKind, Trigger, Workflow};

/// One node's box at the design's size: 168 × 48, the design's step row turned
/// on its side. [`Metrics`] carries the size a picture is drawn at.
pub const NODE_W: f32 = 168.;
pub const NODE_H: f32 = 48.;

/// The air a node leaves inside its box, either side of its text.
pub const NODE_PADDING: f32 = 10.;

/// The gap between two ranks, and between two steps in one rank. The horizontal
/// gap is wider because a loop turns in the space beside a box.
pub const GAP_X: f32 = 72.;
pub const GAP_Y: f32 = 24.;

/// Room beside the widest rank for the routed links' lanes: at least this, and
/// one lane step more for each lane past the first.
pub const LOOP_ROOM: f32 = 56.;

/// The air the screen leaves around the picture: the scroller's padding.
pub const DIAGRAM_PADDING: f32 = 14.;

/// The gap between a node's right edge and the corridor a routed link turns in.
/// Less than [`GAP_X`], so the corridor always runs between two boxes, never
/// through one.
const LOOP_CORRIDOR: f32 = 8.;

/// The distance between two lanes, so two routed links that share the picture's
/// right side still read as two lines. Also the widest spacing the arrivals at
/// one node use.
const LANE_STEP: f32 = 14.;

/// The air a lane keeps from the widest rank and from the picture's edge.
const LANE_MARGIN: f32 = 12.;

/// The painted line's width, at the design's size.
const STROKE_WIDTH: f32 = 1.5;

/// The length of each arm of an arrowhead, at the design's size.
const ARROW_LENGTH: f32 = 9.;

/// The angle between an arrowhead's arm and the line it ends: a direction, so
/// it does not scale.
const ARROW_SPREAD: f32 = 0.55;

/// Every length the picture is drawn from, at one scale.
///
/// The shell sizes everything through `theme::scaled`, so a larger text-size
/// setting makes a node's label grow; a box that stayed 168 × 48 while its
/// label grew would clip it. [`Metrics::base`] is the design as drawn,
/// [`Metrics::live`] is the same at the running text size, and [`layout_at`]
/// takes either — the layout math is a pure function of the workflow and the
/// metrics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// One node's box.
    pub node_w: f32,
    pub node_h: f32,
    /// The air a node leaves inside its box, either side of its text.
    pub node_padding: f32,
    /// The gap between two ranks, and between two steps in one rank.
    pub gap_x: f32,
    pub gap_y: f32,
    /// Room beside the widest rank for the routed links' lanes.
    pub loop_room: f32,
    /// The air the picture leaves around itself.
    pub diagram_padding: f32,
    /// The gap between a node's right edge and a routed link's corridor.
    pub loop_corridor: f32,
    /// The distance between two lanes.
    pub lane_step: f32,
    /// The air a lane keeps from the widest rank and the picture's edge.
    pub lane_margin: f32,
    /// The painted line's width.
    pub stroke_width: f32,
    /// The length of one arrowhead arm.
    pub arrow_length: f32,
}

impl Metrics {
    /// The design's own size, factor 1.
    pub fn base() -> Self {
        Self::at(1.)
    }

    /// The design scaled by `factor`: node size, gaps, lane room and stroke.
    pub fn at(factor: f32) -> Self {
        let scaled = |v: f32| v * factor;
        Self {
            node_w: scaled(NODE_W),
            node_h: scaled(NODE_H),
            node_padding: scaled(NODE_PADDING),
            gap_x: scaled(GAP_X),
            gap_y: scaled(GAP_Y),
            loop_room: scaled(LOOP_ROOM),
            diagram_padding: scaled(DIAGRAM_PADDING),
            loop_corridor: scaled(LOOP_CORRIDOR),
            lane_step: scaled(LANE_STEP),
            lane_margin: scaled(LANE_MARGIN),
            stroke_width: scaled(STROKE_WIDTH),
            arrow_length: scaled(ARROW_LENGTH),
        }
    }

    /// The design at the running text size: the scale the rest of the shell
    /// measures through.
    pub fn live() -> Self {
        Self::at(crate::theme::scaled(1.))
    }
}

/// One step, placed.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// The index of the step in the graph's declaration order.
    pub index: usize,
    pub id: String,
    pub kind: StepKind,
    /// Whether the step runs once for every task.
    pub per_task: bool,
    /// Whether the step runs at all. A disabled step is drawn, but never runs.
    pub enabled: bool,
    /// The step's left edge, in the diagram's own coordinates.
    pub x: f32,
    /// The step's top edge.
    pub y: f32,
    /// The column the step sits in: one past its deepest dependency.
    pub rank: usize,
}

impl Node {
    /// The node's centre, which is where an edge starts and ends, at the
    /// picture's own scale.
    pub fn centre(&self, metrics: Metrics) -> (f32, f32) {
        (self.x + metrics.node_w / 2., self.y + metrics.node_h / 2.)
    }

    /// The node's second line: the compact vocabulary a 168-wide box fits.
    ///
    /// The kind is one word, and a marker says the one thing about the step a
    /// reader cannot see from the picture: `per task` when it runs for every
    /// task, `off` when it never runs (a disabled step has no cadence). Never
    /// a sentence — the lines already say the dependencies, and the lanes say
    /// the loops.
    pub fn meta(&self) -> String {
        let mut words = vec![kind_word(self.kind)];
        if !self.enabled {
            words.push("off");
        } else if self.per_task {
            words.push("per task");
        }
        words.join(" · ")
    }
}

/// The word a step kind shows on a node.
pub fn kind_word(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Harness => "harness",
        StepKind::Gate => "gate",
        StepKind::Question => "question",
        StepKind::Merge => "merge",
        StepKind::Cleanup => "cleanup",
    }
}

/// One connection, drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// The step the line leaves.
    pub from: usize,
    /// The step the line reaches.
    pub to: usize,
    /// The edge's trigger, or `None` when the link is a dependency.
    pub trigger: Option<Trigger>,
    /// Whether the link goes back: its target is not to the left of its sender.
    ///
    /// A dependency is never a back-link — the graph is acyclic — so this is
    /// how the diagram knows to draw a loop.
    pub back: bool,
    /// Whether the link skips a rank: a forward dependency whose target is more
    /// than one rank above it, drawn around the boxes between the two edges
    /// rather than through them.
    pub long: bool,
    /// The x of the lane a routed link runs down, in diagram coordinates. `0.`
    /// for a link drawn straight between the two boxes.
    pub lane: f32,
    /// The y offset from the target's right-edge middle where a routed link
    /// re-enters it, so two links into one node are two arrowheads. `0.` for a
    /// link drawn straight.
    pub entry: f32,
}

impl Link {
    /// Whether the line is routed around the boxes between its two ends — a
    /// loop on the right, or a skip's lane — rather than drawn straight.
    pub fn routed(&self) -> bool {
        self.back || self.long
    }
}

/// The picture: every node placed, every link routed.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub nodes: Vec<Node>,
    pub links: Vec<Link>,
    /// The diagram's size, including the room the routed links loop in.
    pub width: f32,
    pub height: f32,
    /// The measurements the picture was placed at.
    metrics: Metrics,
}

impl Layout {
    /// The measurements the picture was placed at: a node's box, the gaps, the
    /// painted line's width.
    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// The node for `id`.
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// The node's index in the diagram, by step id.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|node| node.id == id)
    }

    /// The width the diagram column needs: the picture plus the air the
    /// scroller leaves around it.
    ///
    /// The screen takes its column from this rather than from a number, so a
    /// fork the file draws is never clipped by a width chosen for the shipped
    /// pipeline.
    pub fn column_width(&self) -> f32 {
        self.width + 2. * self.metrics.diagram_padding
    }
}

/// Place `workflow` as a picture at the design's own size.
///
/// This is [`layout_at`] with [`Metrics::base`]: the layout math is a pure
/// function of the workflow, so a test or a probe compares pictures rather than
/// screens. The screen draws [`live_layout`], the same picture at the running
/// text size.
pub fn layout(workflow: &Workflow) -> Layout {
    layout_at(workflow, Metrics::base())
}

/// The picture as the shell draws it: [`layout_at`] at the running text size.
pub fn live_layout(workflow: &Workflow) -> Layout {
    layout_at(workflow, Metrics::live())
}

/// Place `workflow` as a picture drawn at `metrics`.
///
/// The graph is walked by declaration order and `depends_on` only — never by
/// the edges, which would make the picture's own order depend on the loops it
/// is drawing. A graph with a cycle (which `validate` refuses, but the editor
/// draws mid-edit) is laid out with every step in some rank rather than
/// recursing forever.
pub fn layout_at(workflow: &Workflow, metrics: Metrics) -> Layout {
    let m = metrics;
    let count = workflow.steps.len();
    if count == 0 {
        return Layout {
            nodes: Vec::new(),
            links: Vec::new(),
            width: 0.,
            height: 0.,
            metrics,
        };
    }

    // 1. Rank: one past the deepest dependency.
    let mut rank: Vec<usize> = vec![0; count];
    let rank_of: BTreeMap<&str, usize> = workflow
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.id.as_str(), index))
        .collect();
    let mut settled = vec![false; count];
    for index in 0..count {
        rank[index] = rank_of_step(
            index,
            workflow,
            &rank_of,
            &mut rank,
            &mut settled,
            &mut Vec::new(),
        );
    }

    // 2. Order inside each rank, by the mean rank of the parents.
    let mut columns: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (index, level) in rank.iter().enumerate() {
        columns.entry(*level).or_default().push(index);
    }
    let parents = |index: usize| -> Vec<usize> {
        workflow
            .dependencies(&workflow.steps[index])
            .iter()
            .filter_map(|id| rank_of.get(id.as_str()).copied())
            .collect()
    };
    for members in columns.values_mut() {
        members.sort_by(|left, right| {
            let bary = |index: usize| {
                let parents = parents(index);
                if parents.is_empty() {
                    index as f32
                } else {
                    parents.iter().map(|parent| *parent as f32).sum::<f32>() / parents.len() as f32
                }
            };
            bary(*left)
                .partial_cmp(&bary(*right))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.cmp(right))
        });
    }

    // 3. Place: y from the rank, x from the order, centred on the parents.
    let mut nodes: Vec<Option<Node>> = vec![None; count];
    let widest = columns.values().map(Vec::len).max().unwrap_or(1);
    let content_width = widest as f32 * m.node_w + (widest.saturating_sub(1)) as f32 * m.gap_x;
    for (row, members) in &columns {
        let count_in_row = members.len();
        let row_width =
            count_in_row as f32 * m.node_w + (count_in_row.saturating_sub(1)) as f32 * m.gap_x;
        // centre a short row in the widest one, so a single step under a fork
        // sits level with the middle of the fork instead of hanging left
        let left = (content_width - row_width) / 2.;
        for (position, index) in members.iter().enumerate() {
            let x = left + position as f32 * (m.node_w + m.gap_x);
            let y = *row as f32 * (m.node_h + m.gap_y);
            nodes[*index] = Some(Node {
                index: *index,
                id: workflow.steps[*index].id.clone(),
                kind: workflow.steps[*index].kind,
                per_task: workflow.steps[*index].per_task,
                enabled: workflow.steps[*index].enabled,
                x,
                y,
                rank: *row,
            });
        }
    }

    // A step with several parents is centred on them: a join should look like a
    // join, not like a branch that happens to sit left.
    let mut placed: Vec<Node> = nodes.into_iter().flatten().collect();
    placed.sort_by_key(|node| (node.rank, node.index));
    for index in 0..count {
        let parents = parents(index);
        if parents.len() < 2 {
            continue;
        }
        let mean = parents
            .iter()
            .filter_map(|parent| placed.iter().find(|node| node.index == *parent))
            .map(|parent| parent.x)
            .sum::<f32>()
            / parents.len() as f32;
        if let Some(node) = placed.iter_mut().find(|node| node.index == index) {
            node.x = mean.clamp(0., (content_width - m.node_w).max(0.));
        }
    }

    // 4. Links: the dependencies a step *has* to be told apart by, then every
    //    declared edge. A link is "back" when its target is not strictly above
    //    its sender, and "long" when a dependency skips a rank and so has to be
    //    drawn around the boxes between the two edges.
    //
    //    The drawn dependencies are the covering relation of the order, not the
    //    whole of it: a step's `depends_on` is omitted almost everywhere, so it
    //    names every step above it, and most of those are reached through
    //    another one anyway. Drawing them all turns a pipeline into a circuit
    //    — the shipped template resolves to 29 links, 16 of them implied. A
    //    line that another line already implies says nothing a reader needs,
    //    so an implied dependency is left out of the *picture* only: the
    //    scheduler still waits on everything `dependencies()` returns.
    let mut ancestor_memo: Vec<Option<BTreeSet<usize>>> = vec![None; count];
    let mut links: Vec<Link> = Vec::new();
    for (index, step) in workflow.steps.iter().enumerate() {
        let implied: BTreeSet<usize> = parents(index)
            .iter()
            .flat_map(|dependency| {
                ancestors(
                    workflow,
                    &rank_of,
                    &mut ancestor_memo,
                    &mut Vec::new(),
                    *dependency,
                )
            })
            .collect();
        for dependency in workflow.dependencies(step) {
            let Some(target) = layout_index(&placed, &dependency) else {
                continue;
            };
            if target == index || implied.contains(&target) {
                continue;
            }
            let back = placed[target].y >= placed[index].y;
            // A dependency that skips a rank jumps over the boxes between the
            // two edges; the file says the step waits for that one, so the line
            // has to go around them rather than through them.
            let long = !back && rank[index] > rank[placed[target].index] + 1;
            links.push(Link {
                from: index,
                to: target,
                trigger: None,
                back,
                long,
                lane: 0.,
                entry: 0.,
            });
        }
        for edge in &step.edges {
            let Some(target) = layout_index(&placed, &edge.to) else {
                continue;
            };
            links.push(Link {
                from: index,
                to: target,
                trigger: Some(edge.when),
                // A back-edge is drawn as a loop even when its target happens
                // to sit far to the left, because the file says it is a return
                // and the picture must say the same thing.
                back: true,
                long: false,
                lane: 0.,
                entry: 0.,
            });
        }
    }

    // 5. A dependency that skips a rank runs its vertical stretch in the first
    //    clear channel beside the boxes it skips. That channel is inside the
    //    picture when the ranks it passes are narrower than the widest one — the
    //    shipped template names its dependencies, so a lane per skipped rank
    //    would make the picture wider than the window it is read in. When the
    //    skipped ranks reach the picture's edge there is no channel, and the
    //    link takes an outside lane like a loop.
    for link in links.iter_mut().filter(|link| link.long) {
        if let Some(channel) = forward_channel(&placed, m, content_width, link) {
            link.lane = channel;
        }
    }

    // 6. Outside lanes: one column of empty space to the right of the widest
    //    rank per routed link that needs one — every loop, and a dependency
    //    that skips a rank with no channel inside the picture — so two lines
    //    sharing the picture's right side are still two lines. The room is
    //    charged only when there is such a line to draw.
    let outside: Vec<usize> = links
        .iter()
        .enumerate()
        .filter(|(_, link)| link.back || (link.long && link.lane == 0.))
        .map(|(number, _)| number)
        .collect();
    let lanes_room = if outside.is_empty() {
        0.
    } else {
        loop_room(outside.len(), m)
    };
    if !outside.is_empty() {
        let first =
            content_width + lanes_room / 2. - (outside.len() as f32 - 1.) * m.lane_step / 2.;
        for (slot, number) in outside.iter().enumerate() {
            links[*number].lane = first + slot as f32 * m.lane_step;
        }
    }

    // 7. Arrivals: a routed link re-enters its target at its own height on the
    //    target's right edge, so two loops into one step are two arrowheads
    //    rather than one drawn twice. One link into a step keeps the middle;
    //    several share the box's inside air, evenly.
    let mut arrivals: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (number, link) in links.iter().enumerate() {
        if link.routed() {
            arrivals.entry(link.to).or_default().push(number);
        }
    }
    for numbers in arrivals.values() {
        if numbers.len() < 2 {
            continue;
        }
        let span = (m.node_h - 2. * m.node_padding).max(0.);
        let step = (span / (numbers.len() - 1) as f32).min(m.lane_step);
        for (slot, number) in numbers.iter().enumerate() {
            let offset = slot as f32 - (numbers.len() - 1) as f32 / 2.;
            links[*number].entry = offset * step;
        }
    }

    // The lanes turn in the gap under a rank, and a link that leaves the last
    // rank needs a gap there too.
    let routed = links.iter().filter(|link| link.routed()).count();
    let bottom_room = if routed == 0 { 0. } else { m.gap_y };
    let height = columns.len() as f32 * m.node_h
        + (columns.len().saturating_sub(1)) as f32 * m.gap_y
        + bottom_room;
    Layout {
        nodes: placed,
        links,
        width: content_width.max(m.node_w) + lanes_room,
        height: height.max(m.node_h),
        metrics,
    }
}

/// The x of the clear channel a long forward link runs its vertical stretch in:
/// just outside the boxes of every rank it passes, while that is still inside
/// the picture. `None` when those ranks reach the picture's right edge, where
/// the link has to take an outside lane instead.
fn forward_channel(
    nodes: &[Node],
    metrics: Metrics,
    content_width: f32,
    link: &Link,
) -> Option<f32> {
    let from = &nodes[link.from];
    let to = &nodes[link.to];
    // The stretch starts in the gap under the target's rank and ends in the gap
    // under the sender's, so it passes every rank from the target's next one
    // through the sender's own band.
    let passes = |node: &&Node| node.rank > to.rank && node.rank <= from.rank;
    let right = nodes
        .iter()
        .filter(passes)
        .map(|node| node.x + metrics.node_w)
        .fold(f32::MIN, f32::max);
    let channel = right + metrics.loop_corridor;
    (right.is_finite() && channel <= content_width - metrics.lane_margin).then_some(channel)
}

/// The room the lanes need beside the widest rank: the design's [`LOOP_ROOM`]
/// for one lane, and one [`LANE_STEP`] more for each extra one, with
/// [`LANE_MARGIN`] either side.
fn loop_room(lanes: usize, metrics: Metrics) -> f32 {
    let steps = lanes.saturating_sub(1) as f32 * metrics.lane_step;
    metrics.loop_room.max(steps + 2. * metrics.lane_margin)
}

/// The placed node index for a step id.
fn layout_index(nodes: &[Node], id: &str) -> Option<usize> {
    nodes.iter().position(|node| node.id == id)
}

/// Every step `index` waits for, directly or through another step.
///
/// The picture leaves out a dependency that another dependency of the same step
/// already reaches, so this is what decides which lines are worth drawing.
/// `memo` is per-layout, `walking` is the guard that keeps a cycle (which
/// `validate` refuses but the editor draws mid-edit) from recursing forever.
fn ancestors(
    workflow: &Workflow,
    rank_of: &BTreeMap<&str, usize>,
    memo: &mut [Option<BTreeSet<usize>>],
    walking: &mut Vec<usize>,
    index: usize,
) -> BTreeSet<usize> {
    if let Some(known) = &memo[index] {
        return known.clone();
    }
    if walking.contains(&index) {
        return BTreeSet::new();
    }
    walking.push(index);
    let mut out = BTreeSet::new();
    for dependency in workflow.dependencies(&workflow.steps[index]) {
        let Some(step) = rank_of.get(dependency.as_str()).copied() else {
            continue;
        };
        out.insert(step);
        out.extend(ancestors(workflow, rank_of, memo, walking, step));
    }
    walking.pop();
    memo[index] = Some(out.clone());
    out
}

/// A step's rank: one past the deepest of its dependencies.
///
/// `settled` is the guard that keeps a cycle (drawn mid-edit) from recursing
/// forever; `walking` is the guard that keeps one step from counting twice.
fn rank_of_step(
    index: usize,
    workflow: &Workflow,
    rank_of: &BTreeMap<&str, usize>,
    rank: &mut [usize],
    settled: &mut [bool],
    walking: &mut Vec<usize>,
) -> usize {
    if settled[index] {
        return rank[index];
    }
    if walking.contains(&index) {
        return 0;
    }
    walking.push(index);
    let mut deepest = 0;
    for dependency in workflow.dependencies(&workflow.steps[index]) {
        if let Some(dependency) = rank_of.get(dependency.as_str()).copied() {
            deepest = deepest
                .max(rank_of_step(dependency, workflow, rank_of, rank, settled, walking) + 1);
        }
    }
    walking.pop();
    rank[index] = deepest;
    settled[index] = true;
    deepest
}

/// One link as the paint pass needs it: where it starts, where it goes, and
/// whether it is a loop.
#[derive(Clone, Debug)]
pub struct Stroke {
    /// The points the line passes through, in diagram coordinates.
    pub points: Vec<(f32, f32)>,
    /// The arrowhead's tip and the direction it points in (radians, with 0
    /// pointing right and y growing downwards).
    pub arrow: ((f32, f32), f32),
}

/// The line one link is drawn as.
///
/// A one-rank forward link is a straight line between the two facing edges,
/// clipped to each node's box so it starts at the border rather than inside the
/// text. A routed link — a back-edge, or a dependency that skips a rank — leaves
/// the sender's right side, turns out of the sender's rank through the gap
/// below it, runs up or down its own lane, and turns back into the target's
/// right side through the gap below the target's rank, at the arrival height
/// [`Link::entry`] gives it — the loop shape a flowchart draws.
///
/// Every horizontal run of a routed link is in a rank gap, which no node
/// occupies, and both vertical runs are either in the lane (placed outside the
/// boxes the link skips) or in the space beside one box (inside [`GAP_X`]).
/// That is what keeps a lane from crossing a node's box: it touches only the
/// boxes it runs beside.
pub fn stroke(layout: &Layout, link: &Link) -> Stroke {
    let m = layout.metrics;
    let from = &layout.nodes[link.from];
    let to = &layout.nodes[link.to];
    if !link.routed() {
        // A forward link joins the two **facing** edges: the dependent's top
        // and the dependency's bottom. Drawing it bottom-to-top instead makes
        // the line run through both boxes — and since the node divs are painted
        // over the canvas, the part inside a box simply vanished behind it, so
        // the arrowhead landed on the target's far side and was never seen.
        let downward = to.y > from.y;
        let start = (
            from.x + m.node_w / 2.,
            if downward { from.y + m.node_h } else { from.y },
        );
        let end = (
            to.x + m.node_w / 2.,
            if downward { to.y } else { to.y + m.node_h },
        );
        let angle = (end.1 - start.1).atan2(end.0 - start.0);
        return Stroke {
            points: vec![start, end],
            arrow: (end, angle),
        };
    }
    // A return goes out of the sender's right side, around the outside of the
    // column, and back into the target's right side. Both turns happen in the
    // empty gap under their rank, and the corridor between a box and its
    // neighbour is narrower than the gap between two boxes, so the line never
    // has to cross one.
    let exit_x = from.x + m.node_w;
    let enter_x = to.x + m.node_w;
    let exit_y = from.y + m.node_h + m.gap_y / 2.;
    let enter_y = to.y + m.node_h + m.gap_y / 2.;
    let start = (exit_x, from.y + m.node_h / 2.);
    let end = (enter_x, to.y + m.node_h / 2. + link.entry);
    // A channel inside the picture can sit exactly on the corridor, so the turn
    // is a right angle with one leg of no length: skip the repeated point
    // rather than feeding a zero-length segment to the painter.
    let mut points = vec![start];
    for point in [
        (exit_x + m.loop_corridor, start.1),
        (exit_x + m.loop_corridor, exit_y),
        (link.lane, exit_y),
        (link.lane, enter_y),
        (enter_x + m.loop_corridor, enter_y),
        (enter_x + m.loop_corridor, end.1),
        end,
    ] {
        if points.last() != Some(&point) {
            points.push(point);
        }
    }
    Stroke {
        points,
        arrow: (end, std::f32::consts::PI),
    }
}

/// Paint a layout's links as one canvas element.
///
/// `color` decides each link's stroke, so the caller can make a back-edge the
/// accent colour and a dependency the hairline. `offset` shifts the whole
/// picture, which is how the caller places the diagram inside a scrolled
/// container.
pub fn paint(
    layout: Layout,
    offset: (f32, f32),
    color: impl Fn(&Link) -> Hsla + 'static,
) -> AnyElement {
    let size = (layout.width, layout.height);
    let metrics = layout.metrics;
    canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| {
            let origin = (
                f32::from(bounds.left()) + offset.0,
                f32::from(bounds.top()) + offset.1,
            );
            for link in &layout.links {
                let stroke = stroke(&layout, link);
                paint_stroke(window, origin, &stroke, color(link), metrics);
            }
        },
    )
    .w(px(size.0))
    .h(px(size.1))
    .into_any_element()
}

/// One stroke: the line, then its arrowhead.
fn paint_stroke(
    window: &mut Window,
    origin: (f32, f32),
    stroke: &Stroke,
    color: Hsla,
    metrics: Metrics,
) {
    let mut builder = PathBuilder::stroke(px(metrics.stroke_width));
    let first = stroke.points.first().copied().unwrap_or((0., 0.));
    builder.move_to(to_point(origin, first));
    // A right-angle corner or a straight run: both are polylines, and a
    // polyline through every point keeps a loop's lane crisp rather than
    // rounding it into a blob.
    for point in stroke.points.iter().skip(1) {
        builder.line_to(to_point(origin, *point));
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, Background::from(color));
    }

    // The arrowhead: two short arms back from the tip, one path each.
    //
    // Neither a filled triangle (`PathBuilder::fill` + `add_polygon`) nor the
    // arms folded into the line's own path draws anything here — both were
    // checked by reading the pixels of a link's tip, where the line ended and
    // the next box began with no widening between them. One stroke path per
    // arm is the shape of path this gpui does paint, so the arrowhead is built
    // the way the line is.
    let ((tip_x, tip_y), angle) = stroke.arrow;
    let length = metrics.arrow_length;
    for side in [-1.0_f32, 1.0] {
        let arm = angle + std::f32::consts::PI + side * ARROW_SPREAD;
        let mut head = PathBuilder::stroke(px(metrics.stroke_width));
        head.move_to(to_point(origin, (tip_x, tip_y)));
        head.line_to(to_point(
            origin,
            (tip_x + length * arm.cos(), tip_y + length * arm.sin()),
        ));
        if let Ok(path) = head.build() {
            window.paint_path(path, Background::from(color));
        }
    }
}

/// A diagram coordinate as an absolute point.
fn to_point(origin: (f32, f32), local: (f32, f32)) -> Point<Pixels> {
    point(px(origin.0 + local.0), px(origin.1 + local.1))
}

/// The element's bounds in diagram coordinates, for hit-testing a click.
///
/// The caller lays its node `div`s over the canvas with these; a helper keeps
/// the two from drifting apart.
pub fn node_style(node: &Node) -> (Pixels, Pixels) {
    (px(node.x), px(node.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(text: &str) -> Workflow {
        let workflow = Workflow::parse(text).expect("parses");
        workflow.validate().expect("validates");
        workflow
    }

    /// The diagram in the review request: a fork, a join, a back-edge from the
    /// join into one branch, and a second back-edge from a chain's end to its
    /// start.
    fn diagram() -> Workflow {
        graph(
            r#"
schema = 3
name = "diagram"
revision = 1

[[step]]
id = "a"
kind = "harness"
skill = "analyze"

[[step]]
id = "b"
kind = "harness"
skill = "analyze"
depends_on = ["a"]

[[step]]
id = "c"
kind = "harness"
skill = "code"
depends_on = ["b"]

[[step]]
id = "d"
kind = "harness"
skill = "code"
depends_on = ["b"]

[[step]]
id = "e"
kind = "harness"
skill = "code"
depends_on = ["c"]

[[step]]
id = "f"
kind = "harness"
skill = "code"
depends_on = ["d"]

[[step]]
id = "g"
kind = "harness"
skill = "review"
produces = "g.md"
depends_on = ["e", "f"]
edges = [{ to = "d", when = "verdict_changes" }]

[[step]]
id = "h"
kind = "harness"
skill = "code"
depends_on = ["g"]

[[step]]
id = "i"
kind = "harness"
skill = "code"
depends_on = ["g"]

[[step]]
id = "j"
kind = "harness"
skill = "review"
produces = "j.md"
depends_on = ["h"]
edges = [{ to = "h", when = "verdict_changes" }]
"#,
        )
    }

    #[test]
    fn a_chain_gets_one_column_per_step() {
        let workflow = graph(
            r#"
schema = 3
name = "t"
revision = 1
[[step]]
id = "one"
kind = "harness"
skill = "a"
[[step]]
id = "two"
kind = "harness"
skill = "a"
[[step]]
id = "three"
kind = "harness"
skill = "a"
"#,
        );
        let layout = layout(&workflow);
        let ranks: Vec<usize> = layout.nodes.iter().map(|node| node.rank).collect();
        assert_eq!(ranks, vec![0, 1, 2], "a chain is a column of rows");
    }

    #[test]
    fn a_fork_opens_one_column_and_a_join_closes_it() {
        let layout = layout(&diagram());
        let rank = |id: &str| layout.node(id).expect(id).rank;
        assert!(rank("b") > rank("a"));
        assert_eq!(rank("c"), rank("d"), "the fork's branches share a column");
        assert!(rank("e") > rank("c"));
        assert!(rank("g") > rank("e"), "the join sits past both branches");
        assert!(rank("h") > rank("g"));
        assert!(rank("i") > rank("g"));
    }

    #[test]
    fn the_two_loops_are_drawn_as_back_links() {
        let layout = layout(&diagram());
        let back: Vec<(String, String)> = layout
            .links
            .iter()
            .filter(|link| link.back)
            .map(|link| {
                (
                    layout.nodes[link.from].id.clone(),
                    layout.nodes[link.to].id.clone(),
                )
            })
            .collect();
        assert!(
            back.contains(&("g".to_owned(), "d".to_owned())),
            "the join loops back into a branch: {back:?}"
        );
        assert!(
            back.contains(&("j".to_owned(), "h".to_owned())),
            "the chain's end loops back to its start: {back:?}"
        );
        assert_eq!(back.len(), 2, "a dependency is never a back-link");
    }

    #[test]
    fn a_back_link_leaves_and_re_enters_on_the_right() {
        let layout = layout(&diagram());
        let link = layout
            .links
            .iter()
            .find(|link| link.back && layout.nodes[link.from].id == "g")
            .expect("the g -> d loop");
        let stroke = stroke(&layout, link);
        let from = &layout.nodes[link.from];
        let to = &layout.nodes[link.to];
        assert_eq!(
            stroke.points.first().copied(),
            Some((from.x + NODE_W, from.y + NODE_H / 2.)),
            "the loop leaves the sender's right edge"
        );
        assert_eq!(
            stroke.points.last().copied(),
            Some((to.x + NODE_W, to.y + NODE_H / 2.)),
            "and re-enters the target's right edge"
        );
        assert!(
            stroke.points.iter().any(|(x, _)| *x > from.x + NODE_W),
            "it runs out into the loop room on the right"
        );
    }

    #[test]
    fn a_forward_link_is_a_straight_line_between_the_boxes() {
        let layout = layout(&diagram());
        // A link runs from a step to the step it waits for.
        let link = layout
            .links
            .iter()
            .find(|link| !link.back && layout.nodes[link.to].id == "a")
            .expect("b waits for a");
        let stroke = stroke(&layout, link);
        assert_eq!(stroke.points.len(), 2);
        assert_eq!(
            stroke.points[0].1, layout.nodes[link.from].y,
            "a forward link leaves the sender's top, the edge facing the step it waits for"
        );
        assert_eq!(
            stroke.points[1].1,
            layout.nodes[link.to].y + NODE_H,
            "and reaches the target's bottom, so neither box hides it or its arrowhead"
        );
    }

    #[test]
    fn every_node_has_room_inside_the_diagram() {
        let layout = layout(&diagram());
        for node in &layout.nodes {
            assert!(node.x >= 0., "{} starts inside the diagram", node.id);
            assert!(
                node.x + NODE_W <= layout.width,
                "{} ends inside the diagram",
                node.id
            );
            assert!(node.y >= 0., "{} sits inside the diagram", node.id);
            assert!(
                node.y + NODE_H <= layout.height,
                "{} is not cut off at the bottom",
                node.id
            );
        }
    }

    #[test]
    fn an_empty_graph_is_an_empty_picture() {
        let workflow = Workflow {
            schema: 3,
            name: "t".to_owned(),
            revision: 1,
            max_parallel: 2,
            steps: Vec::new(),
        };
        let layout = layout(&workflow);
        assert!(layout.nodes.is_empty());
        assert!(layout.links.is_empty());
        assert_eq!(layout.width, 0.);
    }

    #[test]
    fn a_cycle_drawn_mid_edit_still_lays_out() {
        // The editor draws a graph a person is halfway through making, so the
        // layout may not recurse forever on one `validate` would refuse.
        let mut workflow = graph(
            r#"
schema = 3
name = "t"
revision = 1
[[step]]
id = "one"
kind = "harness"
skill = "a"
depends_on = []
[[step]]
id = "two"
kind = "harness"
skill = "a"
depends_on = ["one"]
"#,
        );
        workflow.steps[0].depends_on = Some(vec!["two".to_owned()]);
        let layout = layout(&workflow);
        assert_eq!(layout.nodes.len(), 2);
    }

    /// A fork of three: the widest shape the column has to hold, and the reason
    /// the screen's old fixed 520-point column was too narrow.
    fn fork() -> Workflow {
        graph(
            r#"
schema = 3
name = "fork"
revision = 1

[[step]]
id = "root"
kind = "harness"
skill = "analyze"

[[step]]
id = "left"
kind = "harness"
skill = "code"
depends_on = ["root"]

[[step]]
id = "middle"
kind = "harness"
skill = "code"
depends_on = ["root"]

[[step]]
id = "right"
kind = "harness"
skill = "review"
depends_on = ["root"]

[[step]]
id = "join"
kind = "harness"
skill = "code"
depends_on = ["left", "middle", "right"]
"#,
        )
    }

    #[test]
    fn the_column_grows_to_the_widest_rank() {
        let forked = layout(&fork());
        let widest = 3. * NODE_W + 2. * GAP_X;
        assert_eq!(
            forked.width, widest,
            "a graph with no loop pays for no lane: the widest rank is the width"
        );
        assert_eq!(
            forked.column_width(),
            widest + 2. * DIAGRAM_PADDING,
            "the column adds the scroller's air around the picture"
        );

        let chain = layout(&diagram());
        assert!(
            forked.width > chain.width,
            "a fork is wider than the two-column diagram"
        );
        assert!(
            forked.column_width() >= 3. * NODE_W,
            "the three boxes fit the column"
        );
    }

    #[test]
    fn each_loop_runs_down_its_own_lane() {
        let layout = layout(&diagram());
        let widest = 2. * NODE_W + GAP_X;
        let lanes: Vec<f32> = layout
            .links
            .iter()
            .filter(|link| link.back)
            .map(|link| link.lane)
            .collect();
        assert_eq!(lanes.len(), 2, "the fixture's two loops");
        assert_ne!(lanes[0], lanes[1], "each loop has its own lane");
        assert_eq!(
            layout.width,
            widest + LOOP_ROOM,
            "the lanes are charged beside the widest rank"
        );
        for lane in lanes {
            assert!(
                lane > widest,
                "a lane sits outside the widest rank: {lane} vs {widest}"
            );
            assert!(lane < layout.width, "and inside the picture");
        }
        // the turn below the last rank is room the picture reserved, so the
        // strokes stay inside it
        for link in layout.links.iter().filter(|link| link.back) {
            for (x, y) in stroke(&layout, link).points {
                assert!(
                    (0. ..=layout.width).contains(&x) && (0. ..=layout.height).contains(&y),
                    "the loop stays inside the picture: ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn a_loop_lane_never_crosses_a_node_box() {
        // The loop leaves a step that has siblings to its right: exiting at the
        // node's own middle would run through them.
        let workflow = graph(
            r#"
schema = 3
name = "loop"
revision = 1

[[step]]
id = "root"
kind = "harness"
skill = "analyze"

[[step]]
id = "left"
kind = "harness"
skill = "code"
depends_on = ["root"]

[[step]]
id = "middle"
kind = "harness"
skill = "code"
depends_on = ["root"]

[[step]]
id = "right"
kind = "harness"
skill = "review"
produces = "right.md"
depends_on = ["root"]
edges = [{ to = "left", when = "verdict_changes" }]

[[step]]
id = "join"
kind = "harness"
skill = "code"
depends_on = ["left", "middle", "right"]
"#,
        );
        let layout = layout(&workflow);
        let loops: Vec<&Link> = layout.links.iter().filter(|link| link.back).collect();
        assert_eq!(loops.len(), 1, "the fixture's one loop");
        for link in loops {
            let stroke = stroke(&layout, link);
            for node in &layout.nodes {
                let rect = (node.x, node.y, node.x + NODE_W, node.y + NODE_H);
                for segment in stroke.points.windows(2) {
                    assert!(
                        !crosses_box(segment[0], segment[1], rect),
                        "the {} -> {} loop crosses step `{}`",
                        layout.nodes[link.from].id,
                        layout.nodes[link.to].id,
                        node.id
                    );
                }
            }
        }
    }

    /// Two loops into one step are two lines, so they need two arrivals: the
    /// drawer that draws one arrowhead for both is the defect ADR-0023's list
    /// of edges made visible in the shipped template.
    #[test]
    fn two_loops_into_one_step_get_two_arrowheads() {
        let workflow = graph(
            r#"
schema = 3
name = "two-loops"
revision = 1

[[step]]
id = "code"
kind = "harness"
skill = "code"

[[step]]
id = "review"
kind = "harness"
skill = "review"
produces = "review.md"
edges = [{ to = "code", when = "verdict_changes" }]

[[step]]
id = "test"
kind = "gate"
command = "true"
edges = [{ to = "code", when = "gate_failed" }]
"#,
        );
        let layout = layout(&workflow);
        let code = layout.node("code").expect("code");
        let tips: Vec<(f32, f32)> = layout
            .links
            .iter()
            .filter(|link| link.back && layout.nodes[link.to].id == "code")
            .map(|link| stroke(&layout, link).arrow.0)
            .collect();
        assert_eq!(tips.len(), 2, "the fixture's two loops target `code`");
        assert_ne!(
            tips[0], tips[1],
            "each loop has its own arrowhead: {tips:?}"
        );
        for (x, y) in tips {
            assert_eq!(
                x,
                code.x + NODE_W,
                "a loop arrives on the target's right edge"
            );
            assert!(
                y > code.y && y < code.y + NODE_H,
                "and inside the box it enters: {y}"
            );
        }
    }

    /// A dependency more than one rank up jumps over the boxes between the two
    /// edges, so it is routed around them like a loop; a one-rank link stays
    /// the straight line between the two facing edges.
    #[test]
    fn a_long_forward_link_never_crosses_a_node_box() {
        let workflow = graph(
            r#"
schema = 3
name = "skip"
revision = 1

[[step]]
id = "a"
kind = "harness"
skill = "analyze"
depends_on = []

[[step]]
id = "c"
kind = "harness"
skill = "code"
depends_on = []

[[step]]
id = "b"
kind = "harness"
skill = "code"
depends_on = ["a"]

[[step]]
id = "d"
kind = "harness"
skill = "code"
depends_on = ["c"]

[[step]]
id = "deep"
kind = "harness"
skill = "review"
produces = "deep.md"
depends_on = ["a", "d"]
"#,
        );
        let layout = layout(&workflow);
        let link = |from: &str, to: &str| {
            layout
                .links
                .iter()
                .find(|link| {
                    !link.back
                        && layout.nodes[link.from].id == from
                        && layout.nodes[link.to].id == to
                })
                .unwrap_or_else(|| panic!("the {from} -> {to} dependency"))
        };
        let long = link("deep", "a");
        let drawn = stroke(&layout, long);
        for node in &layout.nodes {
            let rect = (node.x, node.y, node.x + NODE_W, node.y + NODE_H);
            for segment in drawn.points.windows(2) {
                assert!(
                    !crosses_box(segment[0], segment[1], rect),
                    "the deep -> a dependency crosses step `{}`",
                    node.id
                );
            }
        }
        assert!(
            drawn.points.len() > 2,
            "a dependency two ranks up is routed, not a line through the boxes: {:?}",
            drawn.points
        );
        assert!(long.lane > 0., "and it runs down a lane of its own");
        assert!(
            layout.width > 2. * NODE_W + GAP_X,
            "with no channel inside the picture, the lane is charged beside it"
        );
        let near = link("deep", "d");
        assert_eq!(
            stroke(&layout, near).points.len(),
            2,
            "a one-rank dependency is still the straight facing-edge line"
        );
        assert!(
            !near.long && near.lane == 0.,
            "and it costs neither a lane nor a routing"
        );
    }

    #[test]
    fn a_skipping_dependency_stays_inside_the_picture_when_there_is_room() {
        let workflow = graph(
            r#"
schema = 3
name = "skip-inside"
revision = 1

[[step]]
id = "a"
kind = "harness"
skill = "analyze"
depends_on = []

[[step]]
id = "c"
kind = "harness"
skill = "code"
depends_on = []

[[step]]
id = "b"
kind = "harness"
skill = "code"
depends_on = ["a"]

[[step]]
id = "deep"
kind = "harness"
skill = "review"
produces = "deep.md"
depends_on = ["b", "c"]

[[step]]
id = "tail-left"
kind = "harness"
skill = "code"
depends_on = ["deep"]

[[step]]
id = "tail-mid"
kind = "harness"
skill = "code"
depends_on = ["deep"]

[[step]]
id = "tail-right"
kind = "harness"
skill = "code"
depends_on = ["deep"]
"#,
        );
        let layout = layout(&workflow);
        let long = layout
            .links
            .iter()
            .find(|link| {
                !link.back
                    && layout.nodes[link.from].id == "deep"
                    && layout.nodes[link.to].id == "c"
            })
            .expect("deep waits for c");
        // The widest rank is the three tails below it, so a channel that has to
        // clear the target's own row still fits inside the picture.
        let widest = 3. * NODE_W + 2. * GAP_X;
        let skipped = layout
            .nodes
            .iter()
            .filter(|node| node.rank == 1)
            .map(|node| node.x + NODE_W)
            .fold(0., f32::max);
        assert!(
            long.lane > skipped,
            "the channel runs beside the boxes it skips: {} vs {skipped}",
            long.lane
        );
        assert!(
            long.lane < widest,
            "and inside the picture, so it costs no lane room: {} vs {widest}",
            long.lane
        );
        assert_eq!(
            layout.width, widest,
            "a channel inside the picture charges nothing"
        );
        for node in &layout.nodes {
            let rect = (node.x, node.y, node.x + NODE_W, node.y + NODE_H);
            for segment in stroke(&layout, long).points.windows(2) {
                assert!(
                    !crosses_box(segment[0], segment[1], rect),
                    "the deep -> c dependency crosses step `{}`",
                    node.id
                );
            }
        }
    }

    #[test]
    fn an_implied_dependency_is_not_drawn() {
        // `depends_on` omitted means "every step above", so a chain names its
        // transitive closure. The picture draws the covering relation instead:
        // three steps, two lines.
        let workflow = graph(
            r#"
schema = 3
name = "implied"
revision = 1

[[step]]
id = "a"
kind = "harness"
skill = "analyze"

[[step]]
id = "b"
kind = "harness"
skill = "code"

[[step]]
id = "c"
kind = "harness"
skill = "review"
"#,
        );
        let layout = layout(&workflow);
        let drawn: Vec<String> = layout
            .links
            .iter()
            .map(|link| {
                format!(
                    "{}->{}",
                    layout.nodes[link.from].id, layout.nodes[link.to].id
                )
            })
            .collect();
        assert_eq!(
            drawn,
            vec!["b->a", "c->b"],
            "the chain is drawn as a chain, not as its closure"
        );
        assert_eq!(
            workflow.dependencies(&workflow.steps[2]).len(),
            2,
            "while the scheduler still waits for both steps above it"
        );
    }

    #[test]
    fn every_length_scales_with_the_text_size() {
        let workflow = diagram();
        let base = layout(&workflow);
        let larger = layout_at(&workflow, Metrics::at(1.25));
        for (drawn, design) in [
            (larger.metrics().node_w, NODE_W),
            (larger.metrics().node_h, NODE_H),
            (larger.metrics().gap_x, GAP_X),
            (larger.metrics().gap_y, GAP_Y),
            (larger.metrics().lane_step, LANE_STEP),
            (larger.metrics().stroke_width, STROKE_WIDTH),
            (larger.metrics().arrow_length, ARROW_LENGTH),
        ] {
            assert!(
                (drawn - design * 1.25).abs() < 1e-3,
                "{drawn} is the design's {design} at the live text size"
            );
        }
        assert_eq!(
            Metrics::base(),
            Metrics::at(1.),
            "the design's own size is factor 1"
        );

        // the picture is the design's own picture, that much bigger: the same
        // nodes in the same places, the lane room included
        assert!((larger.width - base.width * 1.25).abs() < 0.5);
        assert!((larger.height - base.height * 1.25).abs() < 0.5);
        for (small, big) in base.nodes.iter().zip(&larger.nodes) {
            assert!((big.x - small.x * 1.25).abs() < 0.5);
            assert!((big.y - small.y * 1.25).abs() < 0.5);
        }
        for (small, big) in base.links.iter().zip(&larger.links) {
            assert!((big.lane - small.lane * 1.25).abs() < 0.5);
        }

        // and a stroke is drawn at the picture's scale, not the design's
        let loop_link = larger
            .links
            .iter()
            .find(|link| link.back)
            .expect("the fixture's loop");
        let drawn = stroke(&larger, loop_link);
        let from = &larger.nodes[loop_link.from];
        assert_eq!(
            drawn.points.first().copied(),
            Some((
                from.x + larger.metrics().node_w,
                from.y + larger.metrics().node_h / 2.
            )),
            "the loop leaves the sender's right edge at the picture's own size"
        );
    }

    /// A node's second line is one word plus, at most, one short marker: the
    /// sentence the editor's list uses cannot fit a 168-wide box.
    #[test]
    fn a_node_says_its_kind_and_one_marker() {
        let node = |kind: StepKind, per_task: bool, enabled: bool| Node {
            index: 0,
            id: "step".to_owned(),
            kind,
            per_task,
            enabled,
            x: 0.,
            y: 0.,
            rank: 0,
        };
        assert_eq!(node(StepKind::Harness, false, true).meta(), "harness");
        assert_eq!(node(StepKind::Gate, false, true).meta(), "gate");
        assert_eq!(node(StepKind::Question, false, true).meta(), "question");
        assert_eq!(node(StepKind::Merge, true, true).meta(), "merge · per task");
        assert_eq!(node(StepKind::Cleanup, false, true).meta(), "cleanup");
        assert_eq!(
            node(StepKind::Harness, true, true).meta(),
            "harness · per task"
        );
        assert_eq!(
            node(StepKind::Harness, true, false).meta(),
            "harness · off",
            "a disabled step never runs, so its cadence is not the point"
        );
    }

    #[test]
    fn a_node_label_fits_the_box_it_is_drawn_in() {
        // At the diagram's 12-point micro step the family's UI face fits about
        // twenty characters in the 148 points a node's padding leaves. The
        // compact vocabulary is checked against that ceiling rather than
        // against an ellipsis.
        let widest_char = 7.;
        let room = NODE_W - 2. * NODE_PADDING;
        for kind in [
            StepKind::Harness,
            StepKind::Gate,
            StepKind::Question,
            StepKind::Merge,
            StepKind::Cleanup,
        ] {
            for per_task in [false, true] {
                for enabled in [false, true] {
                    let node = Node {
                        index: 0,
                        id: "step".to_owned(),
                        kind,
                        per_task,
                        enabled,
                        x: 0.,
                        y: 0.,
                        rank: 0,
                    };
                    let label = node.meta();
                    let estimated = label.chars().count() as f32 * widest_char;
                    assert!(
                        estimated <= room,
                        "`{label}` needs {estimated} points of a {room}-point line"
                    );
                    assert!(
                        label.chars().count() <= 20,
                        "`{label}` is longer than the vocabulary allows"
                    );
                }
            }
        }
    }

    /// Whether a segment passes through a box's *inside*: touching a border is
    /// what a link into a node does, and is not a crossing.
    fn crosses_box(
        from: (f32, f32),
        to: (f32, f32),
        (left, top, right, bottom): (f32, f32, f32, f32),
    ) -> bool {
        // Liang–Barsky against the box shrunk by a hair, so a line that only
        // runs along an edge is not counted.
        let inset = 0.01;
        let (left, top, right, bottom) = (left + inset, top + inset, right - inset, bottom - inset);
        let (x0, y0) = from;
        let (dx, dy) = (to.0 - x0, to.1 - y0);
        let mut start = 0.0f32;
        let mut end = 1.0f32;
        for (p, q) in [
            (-dx, x0 - left),
            (dx, right - x0),
            (-dy, y0 - top),
            (dy, bottom - y0),
        ] {
            if p.abs() < f32::EPSILON {
                if q < 0. {
                    return false;
                }
                continue;
            }
            let at = q / p;
            if p < 0. {
                if at > end {
                    return false;
                }
                start = start.max(at);
            } else {
                if at < start {
                    return false;
                }
                end = end.min(at);
            }
        }
        start < end
    }
}
