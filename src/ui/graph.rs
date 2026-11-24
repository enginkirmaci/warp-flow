//! The workflow as a picture: nodes, the dependencies drawn downwards, and
//! back-edges.
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
//! # Lines
//!
//! The picture is read the way the file is, downwards, and every arrowhead
//! says where the work goes next:
//!
//! - A **dependency** leaves the bottom of the step waited for and enters the
//!   top of the step that waits. The lines turn at right angles, in the gap
//!   between two ranks: a diagonal from box to box runs through its neighbours
//!   and meets them at the same point. A step with several dependents sends
//!   one stem down, which splits in the gap; a step that waits for several is
//!   entered once per dependency, at points of their own along its top edge,
//!   so a join is several arrowheads rather than one drawn on top of another.
//! - Every horizontal **run** sits on one of its gap's levels, and two runs
//!   that share no step never share one: on one level they would read as a
//!   single line. A gap is as tall as its levels need.
//! - A **back-edge** — an `edges = [{ to = … }]` entry — is drawn as a loop on
//!   the right: out of the sender's right side, around, and into the target's
//!   right side. The file says it is a return, so its arrowhead stays on the
//!   step it names however the ranks fall, and that is why the picture reads
//!   downwards while a loop climbs. That is the one shape the diagram exists
//!   to show. A loop enters its target at its own height on the target's
//!   right edge, so two loops into one step are two arrivals rather than one
//!   arrowhead drawn twice. It turns in the middle of the gaps it crosses,
//!   where it has always turned: a dependency whose run would cross that
//!   stretch of a gap turns below the loop instead, and the gap is that much
//!   taller. Its lane is the nearest one that clears the boxes it passes,
//!   inside the picture: a chain's loops stay beside the chain rather than out
//!   past a wider rank below it, and loops that pass the same boxes take the
//!   same lane, so their runs line up. Two loops busy over the same stretch of
//!   a lane take lanes a step apart, and only a loop whose boxes reach the
//!   picture's edge takes a lane outside it — which is why the layout leaves
//!   room to its right when there is such a loop.
//! - A **dependency that skips a rank** has boxes in its way, and a straight
//!   line between the two rows would run through every one of them. It runs
//!   down a channel of its own beside them instead, far enough from each box
//!   not to read as a second border on it — inside the picture when the ranks
//!   it passes leave room, and in a lane outside it, on the nearer side, when
//!   they do not — and turns back in at the top of the step that waits.
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

use std::cmp::Ordering;
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

/// Room beside the widest rank for a loop's lane when the boxes it passes
/// reach the picture's edge: at least this, and one lane step more for each
/// lane past the first.
pub const LOOP_ROOM: f32 = 56.;

/// The air the screen leaves around the picture: the scroller's padding.
pub const DIAGRAM_PADDING: f32 = 14.;

/// The gap between a node's right edge and the corridor a loop turns in.
/// Less than [`GAP_X`], so the corridor always runs between two boxes, never
/// through one.
const LOOP_CORRIDOR: f32 = 8.;

/// How far a loop's lane keeps from the boxes it passes: past the corridor it
/// turns in, so the loop's run has some length to it, and far enough that a
/// lane beside a box does not read as a second border on it.
const LOOP_CLEAR: f32 = 24.;

/// The distance between two lanes — two loops busy over the same stretch of
/// the picture are two lines, not one — and the least a lane and a channel
/// through one rank keep apart. Also the widest spacing a step's right edge
/// gives the arrivals along it.
const LANE_STEP: f32 = 14.;

/// The air a channel or a lane keeps from the picture's edges.
const LANE_MARGIN: f32 = 12.;

/// The distance between two levels in one gap: two horizontal runs this far
/// apart read as two lines where one level would read as one.
const TRACK_STEP: f32 = 10.;

/// How far a skipping dependency's channel keeps from every box it passes. A
/// line any closer reads as a second border on the box beside it.
const CHANNEL_CLEAR: f32 = 16.;

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
    /// Room beside the widest rank for a loop's lane outside the picture.
    pub loop_room: f32,
    /// The air the picture leaves around itself.
    pub diagram_padding: f32,
    /// The gap between a node's right edge and a loop's corridor.
    pub loop_corridor: f32,
    /// How far a loop's lane keeps from the boxes it passes.
    pub loop_clear: f32,
    /// The distance between two lanes.
    pub lane_step: f32,
    /// The air a channel or a lane keeps from the picture's edges.
    pub lane_margin: f32,
    /// The distance between two levels in one gap.
    pub track_step: f32,
    /// How far a skipping dependency's channel keeps from the boxes it passes.
    pub channel_clear: f32,
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
            loop_clear: scaled(LOOP_CLEAR),
            lane_step: scaled(LANE_STEP),
            lane_margin: scaled(LANE_MARGIN),
            track_step: scaled(TRACK_STEP),
            channel_clear: scaled(CHANNEL_CLEAR),
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
    /// The row the step sits in: one past its deepest dependency.
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
    /// The step the line leaves: the step a dependency is on, the sender of a
    /// loop.
    pub from: usize,
    /// The step the line reaches, and where its arrowhead is: the step that
    /// waits, for a dependency; the step the edge names, for a loop.
    pub to: usize,
    /// The edge's trigger, or `None` when the link is a dependency.
    pub trigger: Option<Trigger>,
    /// Whether the link is drawn as a loop on the right. A declared edge
    /// always is — the file says it is a return, so the picture says the same
    /// thing whatever the ranks — and so is a dependency on a step that is
    /// not strictly above, which only a cycle has (`validate` refuses those,
    /// but the editor draws one mid-edit).
    pub back: bool,
    /// Whether the link skips a rank: a dependency whose step that waits sits
    /// more than one rank below it, drawn around the boxes between the two rows
    /// rather than through them.
    pub long: bool,
    /// The x of the lane a routed link runs down, in diagram coordinates: for
    /// a loop, the lane beside the boxes it passes; for a skipping dependency,
    /// the channel beside the boxes it passes. `0.` for a link that has no
    /// lane.
    pub lane: f32,
    /// Where a routed link enters its target, as an offset from the middle of
    /// the edge it enters — the top edge for a dependency, the right edge for a
    /// loop — so two lines into one step are two arrivals rather than one
    /// arrowhead drawn twice.
    pub entry: f32,
    /// The y of the link's horizontal runs: the one in the gap below the rank
    /// it leaves and the one in the gap above the rank it enters. A loop has
    /// both; a dependency between neighbouring ranks has one, written twice; a
    /// straight line has none.
    pub runs: [f32; 2],
}

impl Link {
    /// Whether the line is routed around the boxes between its two ends — a
    /// loop on the right, or a skip's channel — rather than drawn through the
    /// gap it shares with its target.
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

    // 3. Place: y from the rank, x from the order, centred on the parents. The
    //    rows are placed here and their heights are not known until the runs
    //    are (step 9), so every step starts at the top and moves down in step 10.
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
            nodes[*index] = Some(Node {
                index: *index,
                id: workflow.steps[*index].id.clone(),
                kind: workflow.steps[*index].kind,
                per_task: workflow.steps[*index].per_task,
                enabled: workflow.steps[*index].enabled,
                x,
                y: 0.,
                rank: *row,
            });
        }
    }

    // A step with several parents is centred on them: a join should look like
    // a join, not like a branch that happens to sit left.
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

    // Two steps centred on their parents can land on top of each other — two
    // joins whose parents interleave do — so a rank ends with its boxes a gap
    // apart, in reading order, and inside the picture.
    for row in placed.chunk_by_mut(|left, right| left.rank == right.rank) {
        row.sort_by(|left, right| {
            left.x
                .total_cmp(&right.x)
                .then(left.index.cmp(&right.index))
        });
        for at in 1..row.len() {
            let reach = row[at - 1].x + m.node_w + m.gap_x;
            if row[at].x < reach {
                row[at].x = reach;
            }
        }
        if let Some(last) = row.last() {
            let over = last.x + m.node_w - content_width;
            if over > 0. {
                for node in row.iter_mut() {
                    node.x -= over;
                }
            }
        }
    }

    // 4. Links: the dependencies a step *has* to be told apart by, then every
    //    declared edge. A dependency is drawn from the step waited for down to
    //    the step that waits, and skips a rank when the two are not in
    //    neighbouring ones.
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
    // `placed` is in rank order, so a step's declaration index is not its
    // position in it. A workflow may declare a step before the step it waits
    // for, and reading `placed[index]` as if it were would draw that
    // dependency backwards.
    let mut slot_of: Vec<usize> = vec![0; count];
    for (position, node) in placed.iter().enumerate() {
        slot_of[node.index] = position;
    }
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
            let Some(waits_for) = layout_index(&placed, &dependency) else {
                continue;
            };
            // `waits_for` is a position in `placed`; `index` and everything the
            // ancestor sets hold are declaration indices.
            let declared = placed[waits_for].index;
            if declared == index || implied.contains(&declared) {
                continue;
            }
            // A cycle has no step above, so a dependency on one that is not
            // strictly above is drawn as a loop, the way an edge is.
            let back = rank[declared] >= rank[index];
            let (from, to) = if back {
                (slot_of[index], waits_for)
            } else {
                (waits_for, slot_of[index])
            };
            links.push(Link {
                from,
                to,
                trigger: None,
                back,
                long: !back && rank[index] > rank[declared] + 1,
                lane: 0.,
                entry: 0.,
                runs: [0.; 2],
            });
        }
        for edge in &step.edges {
            let Some(target) = layout_index(&placed, &edge.to) else {
                continue;
            };
            links.push(Link {
                from: slot_of[index],
                to: target,
                trigger: Some(edge.when),
                // A back-edge is drawn as a loop even when its target happens
                // to sit far to the left, because the file says it is a return
                // and the picture must say the same thing.
                back: true,
                long: false,
                lane: 0.,
                entry: 0.,
                runs: [0.; 2],
            });
        }
    }

    // 5. A loop's lane. A loop runs up or down a lane to the right of the
    //    boxes it passes, and takes the nearest lane it fits in: its
    //    clearance from those boxes, which keeps a chain's loops beside the
    //    chain instead of out past a wider rank below it. Loops that pass the
    //    same boxes take the same lane, so their runs line up; two loops busy
    //    over the same stretch of a lane would run along one line, so the
    //    second takes a lane a step further out. A loop whose boxes reach the
    //    picture's edge has no lane inside it and takes one outside, below.
    let mut lanes: Vec<Lane> = Vec::new();
    let mut outside: Vec<(usize, (usize, usize))> = Vec::new();
    let loops: Vec<usize> = links
        .iter()
        .enumerate()
        .filter(|(_, link)| link.back)
        .map(|(number, _)| number)
        .collect();
    for number in loops {
        let link = &links[number];
        let (from, to) = (&placed[link.from], &placed[link.to]);
        let span = (from.rank.min(to.rank), from.rank.max(to.rank));
        let passes = |node: &Node| {
            (node.rank >= span.0 && node.rank < span.1)
                || node.index == from.index
                || node.index == to.index
        };
        let reach = placed
            .iter()
            .filter(|node| passes(node))
            .map(|node| node.x + m.node_w)
            .fold(f32::MIN, f32::max)
            + m.loop_clear;
        let lane = if let Some(lane) = lanes.iter_mut().find(|lane| {
            lane.x >= reach
                && lane
                    .busy
                    .iter()
                    .all(|(start, end)| *end < span.0 || span.1 < *start)
        }) {
            lane
        } else {
            let x = reach.max(lanes.last().map_or(0., |lane| lane.x + m.lane_step));
            if x > content_width - m.lane_margin {
                outside.push((number, span));
                continue;
            }
            lanes.push(Lane {
                x,
                busy: vec![span],
            });
            links[number].lane = x;
            continue;
        };
        lane.busy.push(span);
        links[number].lane = lane.x;
    }

    // 6. A dependency that skips a rank has boxes in its way. It runs down a
    //    channel of its own beside them, inside the picture when the ranks it
    //    passes leave room for one — and a lane step from every loop's lane it
    //    would otherwise run along; when they line up so that no channel runs
    //    through them all, it takes a lane outside the picture, on the side
    //    nearer its two steps.
    let mut taken: Vec<(usize, usize, f32)> = lanes
        .iter()
        .flat_map(|lane| {
            lane.busy
                .iter()
                .filter(|(low, high)| low < high)
                .map(move |(low, high)| (*low, high - 1, lane.x))
        })
        .collect();
    let mut beside: Vec<(usize, bool)> = Vec::new();
    for number in 0..links.len() {
        if !links[number].long {
            continue;
        }
        let from = placed[links[number].from].rank;
        let to = placed[links[number].to].rank;
        match passage(&placed, &links[number], &taken, content_width, m) {
            Passage::Through(x) => {
                links[number].lane = x;
                taken.push((from + 1, to - 1, x));
            }
            Passage::Beside(left) => beside.push((number, left)),
        }
    }

    // 7. Outside lanes: one column of empty space beside the widest rank for
    //    the routed links that need one — a loop whose boxes reach the
    //    picture's edge, on the right, and a skip with no channel inside the
    //    picture, on whichever side is nearer — so two lines sharing the
    //    picture's edge are still two lines. Loops that are not busy over the
    //    same stretch share a slot, as they share a lane inside. The room is
    //    charged only when there is such a line to draw. Everything is placed
    //    in the widest rank's own coordinates here; the left lanes' room is
    //    outside them, and the whole picture moves right by it at the end.
    let mut slots: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut in_slot: Vec<(usize, usize)> = Vec::new();
    for (number, span) in outside {
        let slot = slots
            .iter()
            .position(|busy| {
                busy.iter()
                    .all(|(start, end)| *end < span.0 || span.1 < *start)
            })
            .unwrap_or_else(|| {
                slots.push(Vec::new());
                slots.len() - 1
            });
        slots[slot].push(span);
        in_slot.push((number, slot));
    }
    let loops = slots.len();
    let mut left: Vec<usize> = Vec::new();
    let mut skips: Vec<usize> = Vec::new();
    for (number, nearer_left) in beside {
        if nearer_left {
            left.push(number);
        } else {
            skips.push(number);
        }
    }
    let on_right = loops + skips.len();
    let (right_room, left_room) = (room(on_right, m), room(left.len(), m));
    if on_right > 0 {
        let lane = |slot: usize| {
            content_width + right_room / 2. - (on_right - 1) as f32 * m.lane_step / 2.
                + slot as f32 * m.lane_step
        };
        for (number, slot) in in_slot {
            links[number].lane = lane(slot);
        }
        for (at, number) in skips.iter().enumerate() {
            links[*number].lane = lane(loops + at);
        }
    }
    for (slot, number) in left.iter().enumerate() {
        links[*number].lane = -left_room / 2. + (left.len() - 1) as f32 * m.lane_step / 2.
            - slot as f32 * m.lane_step;
    }

    // 8. Arrivals. A routed link enters its target at its own point on the
    //    edge that faces it, so two lines into one step are two arrowheads
    //    rather than one drawn twice. One link into a step keeps the middle;
    //    several share the edge's inside air, evenly — a dependency's along
    //    the top, in the order they come from, a loop's along the right.
    let mut arrivals: BTreeMap<(bool, usize), Vec<usize>> = BTreeMap::new();
    for (number, link) in links.iter().enumerate() {
        arrivals
            .entry((link.back, link.to))
            .or_default()
            .push(number);
    }
    for ((back, _), numbers) in arrivals {
        if numbers.len() < 2 {
            continue;
        }
        // Several share the edge's inside air, evenly: a loop's along the
        // right, where the lane step is all a 48-tall box has room for, and a
        // dependency's along the top, twice that, in the order they come from
        // — two arrowheads a lane step apart read as one wide one.
        let edge = if back {
            (m.node_h - 2. * m.node_padding).max(0.)
        } else {
            m.node_w - 2. * m.node_padding
        };
        let widest = if back { m.lane_step } else { 2. * m.lane_step };
        let step = (edge / (numbers.len() - 1) as f32).min(widest);
        let mut ordered = numbers.clone();
        if !back {
            ordered.sort_by(|left, right| {
                let x = |number: usize| placed[links[number].from].x;
                x(*left).total_cmp(&x(*right)).then(left.cmp(right))
            });
        }
        for (slot, number) in ordered.iter().enumerate() {
            let offset = slot as f32 - (numbers.len() - 1) as f32 / 2.;
            links[*number].entry = offset * step;
        }
    }

    // 9. Runs. A dependency turns at right angles: a stem down from the step
    //    it leaves, a horizontal run in the gap between the two ranks, and a
    //    drop into the step that waits. A skip has a run below each of the two
    //    ranks as well, out to its channel and back in.
    //
    //    Two runs that meet and share no step never share a level: on one
    //    level they would read as a single line, and a stem or a drop arriving
    //    at another run's corner reads as a junction. The run that reaches
    //    furthest — counted the way it runs, so one that goes left is measured
    //    from the picture's right — takes the top level, and every run that
    //    meets one already placed goes below it. A line that stops short of
    //    where its neighbour starts then turns under that neighbour's stem,
    //    and its own drop begins below the neighbour's run, so the two never
    //    touch. A gap is made as tall as the levels it ends up with.
    //
    //    A cycle drawn mid-edit can leave a rank with no step in it, so the
    //    rows are counted up to the deepest rank and not by the ranks in use.
    let rows = rank.iter().copied().max().unwrap_or(0) + 1;
    let mut gaps: Vec<Vec<Run>> = vec![Vec::new(); rows - 1];
    for (number, link) in links.iter().enumerate() {
        if link.back {
            continue;
        }
        let stem = placed[link.from].x + m.node_w / 2.;
        let reaches = placed[link.to].x + m.node_w / 2. + link.entry;
        let mut add = |gap: usize, part: usize, stem: f32, drop: f32| {
            gaps[gap].push(Run {
                link: number,
                part,
                stem,
                drop,
            });
        };
        if link.long && link.lane != stem {
            add(placed[link.from].rank, 0, stem, link.lane);
        }
        if link.long {
            if link.lane != reaches {
                add(placed[link.to].rank - 1, 1, link.lane, reaches);
            }
        } else if stem != reaches {
            add(placed[link.from].rank, 0, stem, reaches);
        }
    }
    let mut levels: Vec<Vec<usize>> = gaps.iter().map(|runs| vec![0; runs.len()]).collect();
    for (gap, runs) in gaps.iter().enumerate() {
        let mut order: Vec<usize> = (0..runs.len()).collect();
        order.sort_by(|left, right| {
            runs[*right]
                .reach()
                .partial_cmp(&runs[*left].reach())
                .unwrap_or(Ordering::Equal)
                .then(left.cmp(right))
        });
        for (at, run) in order.iter().enumerate() {
            levels[gap][*run] = order[..at]
                .iter()
                .filter(|other| runs[*run].meets(&runs[**other], &links))
                .map(|other| levels[gap][*other] + 1)
                .max()
                .unwrap_or(0);
        }
    }
    // A step's runs leave its stem together whenever nothing is in the way: a
    // fork is one bar, not a branch off the stem on the way down.
    for gap in 0..gaps.len() {
        let runs = &gaps[gap];
        for (at, run) in runs.iter().enumerate() {
            let target = runs
                .iter()
                .enumerate()
                .filter(|(_, other)| links[other.link].from == links[run.link].from)
                .map(|(other, _)| levels[gap][other])
                .max()
                .unwrap_or(0);
            let level = levels[gap][at];
            let blocked = runs.iter().enumerate().any(|(other, other_run)| {
                matches!(levels[gap][other], between if level < between && between <= target)
                    && run.meets(other_run, &links)
            });
            if !blocked {
                levels[gap][at] = target;
            }
        }
    }

    // A loop turns in the middle of the gap under the rank it leaves and the
    // one under the rank it enters, and runs there from its corridor out to
    // its lane. A dependency's run that would cross that stretch of the gap
    // turns below it instead: the loop keeps the middle it has always had, the
    // dependency's levels start a half-gap further down, and the gap grows by
    // the room between. A loop leaving the last rank turns below the picture,
    // where no dependency runs.
    let mut crossed: Vec<Vec<(f32, f32)>> = vec![Vec::new(); gaps.len()];
    for link in links.iter().filter(|link| link.back) {
        let (from, to) = (&placed[link.from], &placed[link.to]);
        for (rank, corridor) in [
            (from.rank, from.x + m.node_w + m.loop_corridor),
            (to.rank, to.x + m.node_w + m.loop_corridor),
        ] {
            if rank < crossed.len() {
                crossed[rank].push((corridor.min(link.lane), corridor.max(link.lane)));
            }
        }
    }
    let below_loop: Vec<bool> = gaps
        .iter()
        .zip(&crossed)
        .map(|(runs, stretched)| {
            runs.iter().any(|run| {
                let (low, high) = (run.stem.min(run.drop), run.stem.max(run.drop));
                stretched
                    .iter()
                    .any(|(from, to)| low <= *to && *from <= high)
            })
        })
        .collect();

    // 10. Rows: each gap is one design's gap — and half again where a loop made
    //    the dependencies turn below it — and a track step more per level past
    //    the first, so every drop below the lowest run is as long as the
    //    arrowhead it carries.
    let tracks: Vec<usize> = levels
        .iter()
        .map(|gap| gap.iter().copied().max().map_or(0, |level| level + 1))
        .collect();
    let half_gap = m.gap_y / 2.;
    let under_loop = |gap: usize| if below_loop[gap] { half_gap } else { 0. };
    let mut top = 0.;
    let mut row_top = vec![0.; rows];
    for gap in 0..tracks.len() {
        row_top[gap] = top;
        top += m.node_h
            + m.gap_y
            + under_loop(gap)
            + tracks[gap].saturating_sub(1) as f32 * m.track_step;
    }
    row_top[rows - 1] = top;
    for node in &mut placed {
        node.y = row_top[node.rank];
    }
    for (gap, runs) in gaps.iter().enumerate() {
        for (at, run) in runs.iter().enumerate() {
            links[run.link].runs[run.part] = row_top[gap]
                + m.node_h
                + half_gap
                + under_loop(gap)
                + levels[gap][at] as f32 * m.track_step;
        }
    }
    for link in links.iter_mut().filter(|link| link.back) {
        link.runs = [
            placed[link.from].y + m.node_h + half_gap,
            placed[link.to].y + m.node_h + half_gap,
        ];
    }

    // The left lanes' room lies outside the widest rank's coordinates.
    for node in &mut placed {
        node.x += left_room;
    }
    for link in links.iter_mut().filter(|link| link.routed()) {
        link.lane += left_room;
    }

    // A link that leaves the last rank turns in the gap under it.
    let bottom_room = if links.iter().any(|link| link.back) {
        m.gap_y
    } else {
        0.
    };
    Layout {
        nodes: placed,
        links,
        width: left_room + content_width.max(m.node_w) + right_room,
        height: (top + m.node_h + bottom_room).max(m.node_h),
        metrics,
    }
}

/// Where a dependency that skips a rank comes down between the boxes it
/// passes.
#[derive(Debug, PartialEq)]
enum Passage {
    /// A channel inside the picture, at this x.
    Through(f32),
    /// No channel runs through every rank it passes, so it takes a lane
    /// outside the picture: on the left when `true`, on the right when `false`.
    Beside(bool),
}

/// The channel a skipping dependency runs down, clear of every box in the
/// ranks it passes and a lane step from every channel already taken through
/// them. A channel shared with another line is worse than one of its own and
/// better than leaving the picture, so that is the order they are tried in.
fn passage(
    nodes: &[Node],
    link: &Link,
    taken: &[(usize, usize, f32)],
    content_width: f32,
    m: Metrics,
) -> Passage {
    let from = &nodes[link.from];
    let to = &nodes[link.to];
    let (sender, reaches) = (from.x + m.node_w / 2., to.x + m.node_w / 2.);
    let passed: Vec<&Node> = nodes
        .iter()
        .filter(|node| node.rank > from.rank && node.rank < to.rank)
        .collect();
    let mut clear = vec![(m.lane_margin, content_width - m.lane_margin)];
    let mut alone = clear.clone();
    for node in &passed {
        let clearance = (
            node.x - m.channel_clear,
            node.x + m.node_w + m.channel_clear,
        );
        clear = without(clear, clearance);
        alone = without(alone, clearance);
    }
    for (first, last, x) in taken {
        if passed
            .iter()
            .any(|node| *first <= node.rank && node.rank <= *last)
        {
            clear = without(clear, (x - m.lane_step, x + m.lane_step));
        }
    }
    let free = if clear.is_empty() { alone } else { clear };
    let way = if sender <= reaches {
        (sender, reaches)
    } else {
        (reaches, sender)
    };
    if free.is_empty() {
        return Passage::Beside(way.0 < content_width - way.1);
    }
    Passage::Through(nearest(&free, way, sender, reaches))
}

/// The x a line runs down at: the sender's own middle when that is clear, so
/// that its stem simply continues; else the middle of the step it reaches, so
/// that its drop does; else the point of `free` nearest the `way` from one to
/// the other — the middle of the stretch they share, when they share one.
fn nearest(free: &[(f32, f32)], way: (f32, f32), sender: f32, reaches: f32) -> f32 {
    for middle in [sender, reaches] {
        if free
            .iter()
            .any(|(low, high)| *low <= middle && middle <= *high)
        {
            return middle;
        }
    }
    free.iter()
        .map(|&(low, high)| {
            if low <= way.1 && way.0 <= high {
                ((low.max(way.0) + high.min(way.1)) / 2., 0.)
            } else if high < way.0 {
                (high, way.0 - high)
            } else {
                (low, low - way.1)
            }
        })
        .min_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(Ordering::Equal)
                .then(left.0.total_cmp(&right.0))
        })
        .map_or(sender, |(x, _)| x)
}

/// `spans` with `blocked` taken out of each of them: what is left on either
/// side.
fn without(spans: Vec<(f32, f32)>, blocked: (f32, f32)) -> Vec<(f32, f32)> {
    spans
        .into_iter()
        .flat_map(|(low, high)| [(low, high.min(blocked.0)), (low.max(blocked.1), high)])
        .filter(|(low, high)| high - low > 0.)
        .collect()
}

/// The room the lanes need beside the widest rank: none without a lane, the
/// design's [`LOOP_ROOM`] for one, and one [`LANE_STEP`] more for each extra
/// one, with [`LANE_MARGIN`] either side.
fn room(lanes: usize, metrics: Metrics) -> f32 {
    if lanes == 0 {
        return 0.;
    }
    let steps = (lanes - 1) as f32 * metrics.lane_step;
    metrics.loop_room.max(steps + 2. * metrics.lane_margin)
}

/// A lane the loops run up and down inside the picture, and the stretches of
/// it they are busy over.
struct Lane {
    /// The lane's x, in the widest rank's own coordinates.
    x: f32,
    /// The gaps a loop's lane runs between, inclusive: from the one under its
    /// sender's rank to the one under its target's.
    busy: Vec<(usize, usize)>,
}

/// One horizontal run while the picture is being placed: which link it belongs
/// to, where it is entered from above and where it is left downwards.
#[derive(Clone, Copy)]
struct Run {
    link: usize,
    /// Which of the link's two runs it is: the one below the rank it leaves,
    /// or the one above the rank it enters.
    part: usize,
    stem: f32,
    drop: f32,
}

impl Run {
    /// Whether two runs would be mistaken for one line on the same level:
    /// their stretches meet, and no step says they belong together. A fork
    /// shares its stem and a join may share a bar, which is what they are.
    fn meets(&self, other: &Run, links: &[Link]) -> bool {
        if links[self.link].from == links[other.link].from
            || links[self.link].to == links[other.link].to
        {
            return false;
        }
        let (low, high) = (self.stem.min(self.drop), self.stem.max(self.drop));
        let (other_low, other_high) = (other.stem.min(other.drop), other.stem.max(other.drop));
        low <= other_high && other_low <= high
    }

    /// How far the run reaches, counted the way it runs: a run that goes left
    /// is measured from the picture's right.
    fn reach(&self) -> f32 {
        if self.drop >= self.stem {
            self.drop
        } else {
            -self.drop
        }
    }
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
/// where its arrowhead points.
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
/// A dependency runs downwards, the way the file is read: out of the bottom of
/// the step waited for, across the gap on its level, and into the top of the
/// step that waits, at the arrival [`Link::entry`] gives it. A line whose two
/// ends share a middle is the one straight segment between them.
///
/// A back-edge — or, in a cycle the editor draws mid-edit, a dependency on a
/// step that is not above — leaves the sender's right side, turns out of the
/// sender's rank through the gap below it, runs up or down its own lane, and
/// turns back into the target's right side through the gap below the target's
/// rank: the loop shape a flowchart draws, with its arrowhead on the step the
/// edge names.
///
/// Every horizontal run is in a rank gap, which no node occupies, and every
/// vertical run is either in a lane placed outside the boxes it passes or in
/// the space beside one box. That is what keeps a line from crossing a node's
/// box: it touches only the boxes it runs beside.
pub fn stroke(layout: &Layout, link: &Link) -> Stroke {
    let m = layout.metrics;
    let from = &layout.nodes[link.from];
    let to = &layout.nodes[link.to];
    if link.back {
        // A return goes out of the sender's right side, around the outside of
        // the column, and back into the target's right side. The corridor
        // between a box and its neighbour is narrower than the gap between two
        // boxes, so the line never has to cross one.
        let exit_x = from.x + m.node_w;
        let enter_x = to.x + m.node_w;
        let start = (exit_x, from.y + m.node_h / 2.);
        let end = (enter_x, to.y + m.node_h / 2. + link.entry);
        let points = polyline([
            start,
            (exit_x + m.loop_corridor, start.1),
            (exit_x + m.loop_corridor, link.runs[0]),
            (link.lane, link.runs[0]),
            (link.lane, link.runs[1]),
            (enter_x + m.loop_corridor, link.runs[1]),
            (enter_x + m.loop_corridor, end.1),
            end,
        ]);
        return Stroke {
            points,
            arrow: (end, std::f32::consts::PI),
        };
    }
    let start = (from.x + m.node_w / 2., from.y + m.node_h);
    let end = (to.x + m.node_w / 2. + link.entry, to.y);
    let points = if link.long {
        polyline([
            start,
            (start.0, link.runs[0]),
            (link.lane, link.runs[0]),
            (link.lane, link.runs[1]),
            (end.0, link.runs[1]),
            end,
        ])
    } else {
        polyline([start, (start.0, link.runs[0]), (end.0, link.runs[0]), end])
    };
    Stroke {
        points,
        arrow: (end, std::f32::consts::PI / 2.),
    }
}

/// The points a line is drawn through. A point that repeats the last one, or
/// lies past it on the same stretch, extends the line instead of adding a
/// corner to it — a channel on a step's own middle is a straight stem, and a
/// lane on a run's own level is a right angle with one leg of no length.
fn polyline(points: impl IntoIterator<Item = (f32, f32)>) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    for point in points {
        let straight = match out.as_slice() {
            [.., before, last] => {
                (before.0 == last.0 && last.0 == point.0)
                    || (before.1 == last.1 && last.1 == point.1)
            }
            _ => false,
        };
        if straight {
            if let Some(last) = out.last_mut() {
                *last = point;
            }
        } else if out.last() != Some(&point) {
            out.push(point);
        }
    }
    out
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

    /// The line from step `from` to step `to`: for a dependency, the step
    /// waited for and the step that waits.
    fn link<'a>(layout: &'a Layout, from: &str, to: &str) -> &'a Link {
        layout
            .links
            .iter()
            .find(|link| layout.nodes[link.from].id == from && layout.nodes[link.to].id == to)
            .unwrap_or_else(|| panic!("the {from} -> {to} line"))
    }

    /// [`layout`] under a name a test's own `layout` binding does not shadow,
    /// for the tests that lay out a second picture.
    fn picture(workflow: &Workflow) -> Layout {
        layout(workflow)
    }

    /// Every picture the tests look at.
    fn pictures() -> Vec<(&'static str, Workflow)> {
        vec![
            ("diagram", diagram()),
            ("fork", fork()),
            ("shipped", shipped()),
            ("staggered", staggered()),
            ("boxed, root first", boxed(true)),
            ("boxed, root last", boxed(false)),
            ("skip", skip()),
            ("roomy", roomy()),
            ("lined up", lined_up()),
        ]
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

    /// The pipeline this repository ships: a chain, a fork into two gates and
    /// the join under them, two loops, and the four siblings after the merge.
    /// It is the picture the run screen draws for this repository's own runs.
    fn shipped() -> Workflow {
        graph(include_str!("../../.flow/workflow.toml"))
    }

    /// A skip over one rank that is wider than the two steps it joins: the
    /// channel between that rank's boxes is the only way through.
    fn skip() -> Workflow {
        graph(
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
        )
    }

    /// A skip with room to spare: the ranks around the widest one are narrow,
    /// so the channel stays inside the picture.
    fn roomy() -> Workflow {
        graph(
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
        )
    }

    /// A chain of five with a wide rank under it, so there is room beside
    /// every loop: `b` returns to `a`, `d` to `c`, and `e` to `c` two ranks up.
    fn lined_up() -> Workflow {
        graph(
            r#"
schema = 3
name = "lined-up"
revision = 1

[[step]]
id = "a"
kind = "harness"
skill = "analyze"
depends_on = []

[[step]]
id = "b"
kind = "harness"
skill = "review"
produces = "b.md"
edges = [{ to = "a", when = "verdict_changes" }]

[[step]]
id = "c"
kind = "harness"
skill = "code"

[[step]]
id = "d"
kind = "harness"
skill = "review"
produces = "d.md"
edges = [{ to = "c", when = "verdict_changes" }]

[[step]]
id = "e"
kind = "harness"
skill = "review"
produces = "e.md"
edges = [{ to = "c", when = "verdict_changes" }]

[[step]]
id = "tail-left"
kind = "harness"
skill = "code"
depends_on = ["e"]

[[step]]
id = "tail-mid"
kind = "harness"
skill = "code"
depends_on = ["e"]

[[step]]
id = "tail-right"
kind = "harness"
skill = "code"
depends_on = ["e"]
"#,
        )
    }

    /// Two steps over four. `a` reaches past its own middle to `e`, and `e`'s
    /// middle is `b`'s: on one level `a`'s drop would continue `b`'s stem, and
    /// read as one line through both.
    fn staggered() -> Workflow {
        graph(
            r#"
schema = 3
name = "staggered"
revision = 1

[[step]]
id = "a"
kind = "harness"
skill = "analyze"
depends_on = []

[[step]]
id = "b"
kind = "harness"
skill = "analyze"
depends_on = []

[[step]]
id = "c"
kind = "harness"
skill = "code"
depends_on = ["a"]

[[step]]
id = "d"
kind = "harness"
skill = "code"
depends_on = ["a"]

[[step]]
id = "e"
kind = "harness"
skill = "code"
depends_on = ["a"]

[[step]]
id = "f"
kind = "harness"
skill = "code"
depends_on = ["b"]
"#,
        )
    }

    /// A skip over a fork and the join under it. The fork's two boxes and the
    /// join's one line up so that no channel runs through both ranks, and the
    /// line has to leave the picture. `root_first` declares the root it leaves
    /// on the left, so the lane it takes is the left one.
    fn boxed(root_first: bool) -> Workflow {
        let root = r#"
[[step]]
id = "g"
kind = "harness"
skill = "analyze"
depends_on = []
"#;
        let first = r#"
[[step]]
id = "a"
kind = "harness"
skill = "analyze"
depends_on = []
"#;
        let rest = r#"
[[step]]
id = "b"
kind = "harness"
skill = "code"
depends_on = ["a"]

[[step]]
id = "c"
kind = "harness"
skill = "code"
depends_on = ["a"]

[[step]]
id = "d"
kind = "harness"
skill = "code"
depends_on = ["b", "c"]

[[step]]
id = "f"
kind = "harness"
skill = "code"
depends_on = ["g", "d"]
"#;
        let (left, right) = if root_first {
            (root, first)
        } else {
            (first, root)
        };
        graph(&format!(
            "schema = 3\nname = \"boxed\"\nrevision = 1\n{left}{right}{rest}"
        ))
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

        // and it is drawn as the straight lines down that column, each with
        // its arrowhead on the step it reaches
        for (from, to) in [("one", "two"), ("two", "three")] {
            let link = link(&layout, from, to);
            let drawn = stroke(&layout, link);
            let (from, to) = (layout.node(from).unwrap(), layout.node(to).unwrap());
            assert_eq!(drawn.points.len(), 2, "no corner in a straight line");
            assert_eq!(drawn.points[0], (from.x + NODE_W / 2., from.y + NODE_H));
            assert_eq!(drawn.points[1], (to.x + NODE_W / 2., to.y));
            assert_eq!(drawn.arrow.0, drawn.points[1]);
            assert_eq!(drawn.arrow.1, std::f32::consts::FRAC_PI_2, "pointing down");
        }
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
            "it runs out to its lane on the right"
        );
    }

    #[test]
    fn a_loop_keeps_its_arrow_on_the_step_the_edge_names() {
        // The picture's arrows point down, but an edge says "go back to": the
        // loop climbs, and its arrowhead is on the step the file names.
        let layout = layout(&shipped());
        for (sender, named) in [("approve-plan", "analyze"), ("review", "code")] {
            let link = link(&layout, sender, named);
            assert!(link.back, "an edge is a loop however the ranks fall");
            assert!(
                link.trigger.is_some(),
                "and it is the edge, trigger and all"
            );
            let drawn = stroke(&layout, link);
            let named = layout.node(named).unwrap();
            assert_eq!(
                drawn.arrow.0,
                (named.x + NODE_W, named.y + NODE_H / 2. + link.entry),
                "the arrowhead is on `{}`'s right edge",
                named.id
            );
            assert_eq!(
                drawn.arrow.1,
                std::f32::consts::PI,
                "pointing left, into it"
            );
            assert!(
                layout.nodes[link.from].y > layout.nodes[link.to].y,
                "a loop climbs: the sender sits below the step it names"
            );
        }
    }

    #[test]
    fn a_fork_shares_one_stem() {
        let layout = layout(&shipped());
        let merge = layout.node("merge").unwrap();
        let mut bars = Vec::new();
        for name in ["resolve-conflict", "fix-gates", "summary", "cleanup"] {
            let link = link(&layout, "merge", name);
            assert!(!link.back && !link.long);
            let drawn = stroke(&layout, link);
            assert_eq!(
                drawn.points.len(),
                4,
                "stem, run, drop: one corner each way"
            );
            assert_eq!(
                drawn.points[0],
                (merge.x + NODE_W / 2., merge.y + NODE_H),
                "`{name}`'s line leaves the merge's bottom middle"
            );
            assert_eq!(drawn.points[1].0, drawn.points[0].0, "straight down");
            assert_eq!(
                drawn.points[2].0, drawn.points[3].0,
                "and straight down again"
            );
            assert_eq!(drawn.points[3].1, layout.node(name).unwrap().y);
            bars.push(drawn.points[1].1);
        }
        assert!(
            bars.windows(2).all(|pair| pair[0] == pair[1]),
            "the four runs are one bar: {bars:?}"
        );
    }

    #[test]
    fn a_join_gives_each_line_its_own_arrival() {
        // Two lines into one step used to meet at the same point, and one
        // arrowhead was drawn on top of the other.
        let layout = layout(&shipped());
        let bump = layout.node("bump").unwrap();
        let tips: Vec<(f32, f32)> = ["fmt", "test"]
            .iter()
            .map(|from| stroke(&layout, link(&layout, from, "bump")).arrow.0)
            .collect();
        assert_ne!(tips[0], tips[1], "two lines into `bump` are two arrowheads");
        for &(x, y) in &tips {
            assert_eq!(y, bump.y, "each arrives on the top edge");
            assert!(
                x > bump.x && x < bump.x + NODE_W,
                "inside the box it enters: {x}"
            );
        }
        assert!(
            tips[0].0 < tips[1].0,
            "`fmt` sits left of `test`, so it arrives left of it"
        );

        let joined = picture(&diagram());
        let tips: Vec<f32> = ["e", "f"]
            .iter()
            .map(|from| stroke(&joined, link(&joined, from, "g")).arrow.0.0)
            .collect();
        assert_ne!(tips[0], tips[1], "and so do the join's two branches");
    }

    #[test]
    fn every_dependency_arrow_points_down() {
        for (name, workflow) in pictures() {
            let layout = layout(&workflow);
            let dependencies: Vec<&Link> = layout.links.iter().filter(|link| !link.back).collect();
            assert!(!dependencies.is_empty(), "{name}");
            for link in dependencies {
                let drawn = stroke(&layout, link);
                let from = &layout.nodes[link.from];
                let to = &layout.nodes[link.to];
                assert_eq!(
                    drawn.points.first(),
                    Some(&(from.x + NODE_W / 2., from.y + NODE_H)),
                    "{name}: {} -> {} leaves the step waited for at its bottom",
                    from.id,
                    to.id
                );
                assert_eq!(
                    drawn.arrow.0,
                    (to.x + NODE_W / 2. + link.entry, to.y),
                    "{name}: {} -> {} reaches the step that waits at its top",
                    from.id,
                    to.id
                );
                assert_eq!(
                    drawn.arrow.1,
                    std::f32::consts::FRAC_PI_2,
                    "{name}: {} -> {} points down, the way the file is read",
                    from.id,
                    to.id
                );
                assert!(
                    to.y > from.y,
                    "{name}: the step that waits sits below the step it waits for"
                );
            }
        }
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
        // and the step that is not above the other is reached by a loop, the
        // way an edge reaches its target, rather than by a line through boxes
        let loops: Vec<String> = layout
            .links
            .iter()
            .filter(|link| link.back)
            .map(|link| {
                format!(
                    "{} <- {}",
                    layout.nodes[link.to].id, layout.nodes[link.from].id
                )
            })
            .collect();
        assert_eq!(
            loops,
            vec!["one <- two".to_owned()],
            "`two` waits for `one`, which the cycle put below it"
        );
        assert!(
            lines_through_boxes(&layout).is_empty(),
            "{:?}",
            lines_through_boxes(&layout)
        );
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
    fn loops_outside_the_picture_line_up_when_they_do_not_overlap() {
        // Both loops in the diagram pass a rank whose boxes reach the picture's
        // edge, so both take a lane outside it. Neither is busy where the other
        // is, so it is one lane.
        let layout = layout(&diagram());
        let widest = 2. * NODE_W + GAP_X;
        let lanes: Vec<f32> = layout
            .links
            .iter()
            .filter(|link| link.back)
            .map(|link| link.lane)
            .collect();
        assert_eq!(lanes.len(), 2, "the fixture's two loops");
        assert_eq!(
            lanes[0], lanes[1],
            "loops that are not busy over the same stretch share a lane, and line up"
        );
        assert_eq!(
            layout.width,
            widest + LOOP_ROOM,
            "one lane's room is charged beside the widest rank"
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
    fn a_loop_takes_the_nearest_lane_inside_the_picture() {
        // The widest rank is the four siblings at the bottom. A loop used to
        // run out past it whatever it passed, which put the chain's three
        // loops 400 points from the chain and each in a lane of its own.
        let layout = layout(&shipped());
        let right = |id: &str| layout.node(id).unwrap().x + NODE_W;
        let (to_analyze, to_code, from_test) = (
            link(&layout, "approve-plan", "analyze"),
            link(&layout, "review", "code"),
            link(&layout, "test", "code"),
        );
        assert_eq!(
            to_analyze.lane,
            right("analyze") + LOOP_CLEAR,
            "the loop to `analyze` passes nothing wider than the chain"
        );
        assert_eq!(
            to_code.lane, to_analyze.lane,
            "and so does the loop to `code`: the two line up"
        );
        assert_eq!(
            from_test.lane,
            right("clippy") + LOOP_CLEAR,
            "the loop from `test` has to clear `clippy`"
        );
        assert_eq!(
            layout.width,
            4. * NODE_W + 3. * GAP_X,
            "no lane is outside the picture, so no room is charged"
        );
        for link in [to_analyze, to_code, from_test] {
            assert!(
                link.lane < layout.width - LANE_MARGIN,
                "the lane is inside the picture: {}",
                link.lane
            );
        }
    }

    #[test]
    fn loops_line_up_in_shared_lanes() {
        // `b`'s loop and `d`'s pass the same chain, one stretch each, so they
        // take one lane; `e`'s is busy where `d`'s is, so it takes the next.
        let layout = layout(&lined_up());
        // the chain sits in the middle of the three-wide picture
        let reach = layout.node("a").unwrap().x + NODE_W + LOOP_CLEAR;
        assert_eq!(link(&layout, "b", "a").lane, reach);
        assert_eq!(
            link(&layout, "d", "c").lane,
            reach,
            "the same lane: the loops line up"
        );
        assert_eq!(
            link(&layout, "e", "c").lane,
            reach + LANE_STEP,
            "a loop busy over the same stretch takes the next lane out"
        );
        assert_eq!(
            layout.width,
            3. * NODE_W + 2. * GAP_X,
            "and none of them leaves the picture"
        );
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
        let loops: Vec<&Link> = layout
            .links
            .iter()
            .filter(|link| link.back && layout.nodes[link.to].id == "code")
            .collect();
        assert_eq!(loops.len(), 2, "the fixture's two loops target `code`");
        assert_ne!(
            loops[0].lane, loops[1].lane,
            "busy over the same stretch, they take lanes a step apart"
        );
        assert_eq!(
            (loops[1].lane - loops[0].lane).abs(),
            LANE_STEP,
            "and no further apart than that"
        );
        let tips: Vec<(f32, f32)> = loops
            .iter()
            .map(|link| stroke(&layout, link).arrow.0)
            .collect();
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

    #[test]
    fn a_fork_under_a_loop_turns_below_it() {
        // `review` loops back to `code` and also forks to `clippy`, on its
        // right. The loop keeps the middle of its gap, as every loop does; the
        // fork's bar turns below it with a half-gap of air between, and the gap
        // is that much taller.
        let layout = layout(&shipped());
        let (review, code, fmt) = (
            layout.node("review").unwrap(),
            layout.node("code").unwrap(),
            layout.node("fmt").unwrap(),
        );
        let [out, back] = link(&layout, "review", "code").runs;
        for (run, under) in [(out, review), (back, code)] {
            assert_eq!(
                run,
                under.y + NODE_H + GAP_Y / 2.,
                "a loop turns in the middle of the gap"
            );
        }
        let bar = link(&layout, "review", "clippy").runs[0];
        assert_eq!(
            bar,
            out + GAP_Y / 2.,
            "the fork's bar is a half-gap below the loop's run"
        );
        assert_eq!(
            fmt.y,
            review.y + NODE_H + GAP_Y * 3. / 2.,
            "and the gap grew by the room between them"
        );
        assert_eq!(
            review.y,
            code.y + NODE_H + GAP_Y,
            "a gap the loop crosses with no run in it keeps its height"
        );
    }

    /// A dependency more than one rank down jumps over the boxes between the
    /// two rows, so it is routed around them; a one-rank link stays the line
    /// through the gap it shares with its target.
    #[test]
    fn a_long_forward_link_never_crosses_a_node_box() {
        let layout = layout(&skip());
        let long = link(&layout, "a", "deep");
        let drawn = stroke(&layout, long);
        for node in &layout.nodes {
            let rect = (node.x, node.y, node.x + NODE_W, node.y + NODE_H);
            for segment in drawn.points.windows(2) {
                assert!(
                    !crosses_box(segment[0], segment[1], rect),
                    "the a -> deep dependency crosses step `{}`",
                    node.id
                );
            }
        }
        assert!(
            drawn.points.len() > 2,
            "a dependency two ranks down is routed, not a line through the boxes: {:?}",
            drawn.points
        );
        assert!(
            !link(&layout, "d", "deep").long && link(&layout, "d", "deep").lane == 0.,
            "a one-rank dependency costs neither a lane nor a routing"
        );
    }

    #[test]
    fn a_channel_keeps_clear_of_the_boxes_it_passes() {
        // The channel between the two boxes of the rank it passes is the only
        // way through, and it is inside the picture: no room is charged.
        let layout = layout(&skip());
        let (b, d) = (layout.node("b").unwrap(), layout.node("d").unwrap());
        let lane = link(&layout, "a", "deep").lane;
        assert!(
            lane >= b.x + NODE_W + CHANNEL_CLEAR && lane <= d.x - CHANNEL_CLEAR,
            "the channel keeps a box's clearance from either side: {lane}"
        );
        assert_eq!(
            layout.width,
            2. * NODE_W + GAP_X,
            "a channel inside the picture is free"
        );

        // In the shipped pipeline `bump` waits for `fmt` two ranks down, and
        // `test` sits between them: `fmt`'s own middle is clear of `test`, so
        // its stem simply continues down beside it.
        let pipeline = picture(&shipped());
        let (fmt, test) = (
            pipeline.node("fmt").unwrap(),
            pipeline.node("test").unwrap(),
        );
        let lane = link(&pipeline, "fmt", "bump").lane;
        assert_eq!(
            lane,
            fmt.x + NODE_W / 2.,
            "the channel is `fmt`'s own middle"
        );
        assert!(
            test.x - lane >= CHANNEL_CLEAR,
            "and no closer to `test` than a channel keeps: {} vs {lane}",
            test.x
        );
    }

    #[test]
    fn a_skipping_dependency_stays_inside_the_picture_when_there_is_room() {
        let layout = layout(&roomy());
        let long = link(&layout, "c", "deep");
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
            long.lane,
            layout.node("c").unwrap().x + NODE_W / 2.,
            "on the sender's own middle, so its stem continues down"
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
                    "the c -> deep dependency crosses step `{}`",
                    node.id
                );
            }
        }
    }

    #[test]
    fn a_skip_with_no_channel_takes_a_lane_on_the_nearer_side() {
        let widest = 2. * NODE_W + GAP_X;

        // The root it leaves sits right of the join, so the right side is nearer.
        let layout = layout(&boxed(false));
        let long = link(&layout, "g", "f");
        assert!(long.long, "the g -> f dependency skips two ranks");
        assert!(
            long.lane > widest,
            "its lane is outside the picture's right edge: {}",
            long.lane
        );
        assert_eq!(
            layout.width,
            widest + LOOP_ROOM,
            "charged beside the widest rank"
        );
        assert_eq!(layout.node("a").unwrap().x, 0., "the picture did not move");

        // Declared first, the same root is on the left, and so is its lane.
        let moved = picture(&boxed(true));
        let long = link(&moved, "g", "f");
        let g = moved.node("g").unwrap();
        assert!(
            long.lane < g.x,
            "its lane is outside the picture's left edge: {} vs {}",
            long.lane,
            g.x
        );
        assert_eq!(g.x, LOOP_ROOM, "the picture moved right to make the room");
        assert_eq!(moved.width, LOOP_ROOM + widest);

        for workflow in [boxed(false), boxed(true)] {
            let layout = picture(&workflow);
            assert!(
                lines_through_boxes(&layout).is_empty(),
                "{:?}",
                lines_through_boxes(&layout)
            );
            for (x, y) in stroke(&layout, link(&layout, "g", "f")).points {
                assert!(
                    (0. ..=layout.width).contains(&x) && (0. ..=layout.height).contains(&y),
                    "the lane stays inside the picture: ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn two_runs_that_meet_share_no_level() {
        let layout = layout(&staggered());
        let (a_e, b_f, a_c) = (
            link(&layout, "a", "e"),
            link(&layout, "b", "f"),
            link(&layout, "a", "c"),
        );
        assert_ne!(
            a_e.runs[0], b_f.runs[0],
            "`a`'s drop to `e` is `b`'s stem's line"
        );
        assert!(
            a_e.runs[0] > b_f.runs[0],
            "`a`'s run turns under `b`'s stem, so its drop starts below `b`'s run"
        );
        assert_eq!(
            a_e.runs, a_c.runs,
            "and `a`'s fork is still one bar, not a branch off its stem"
        );
        assert_eq!(
            layout.node("c").unwrap().y,
            NODE_H + GAP_Y + TRACK_STEP,
            "the gap is a track step taller for its second level"
        );

        // The mirrored case: the line that goes left turns under the drop
        // arriving at its stem.
        let mirrored = picture(&boxed(true));
        let (g_f, a_b) = (link(&mirrored, "g", "f"), link(&mirrored, "a", "b"));
        assert_ne!(
            g_f.runs[0], a_b.runs[0],
            "`g`'s stem is where `a`'s drop to `b` lands"
        );
        assert!(g_f.runs[0] < a_b.runs[0], "`g`'s run stays above `a`'s");
    }

    #[test]
    fn no_two_lines_read_as_one_and_none_crosses_a_box() {
        for (name, workflow) in pictures() {
            let layout = layout(&workflow);
            let through = lines_through_boxes(&layout);
            assert!(through.is_empty(), "{name}: {through:?}");
            let read = read_as_one(&layout);
            assert!(read.is_empty(), "{name}: {read:?}");
            for link in &layout.links {
                for (x, y) in stroke(&layout, link).points {
                    assert!(
                        (0. ..=layout.width).contains(&x) && (0. ..=layout.height).contains(&y),
                        "{name}: {} -> {} leaves the picture at ({x}, {y})",
                        layout.nodes[link.from].id,
                        layout.nodes[link.to].id
                    );
                }
            }
        }
    }

    #[test]
    fn two_channels_through_one_rank_keep_apart() {
        let node = |index: usize, id: &str, rank: usize, x: f32| Node {
            index,
            id: id.to_owned(),
            kind: StepKind::Harness,
            per_task: false,
            enabled: true,
            x,
            y: 0.,
            rank,
        };
        let nodes = vec![
            node(0, "from", 0, 0.),
            node(1, "left", 1, 0.),
            node(2, "right", 1, 240.),
            node(3, "to", 2, 120.),
        ];
        let link = Link {
            from: 0,
            to: 3,
            trigger: None,
            back: false,
            long: true,
            lane: 0.,
            entry: 0.,
            runs: [0.; 2],
        };
        let (taken, width) = (&[][..], 2. * NODE_W + GAP_X);
        assert_eq!(
            passage(&nodes, &link, taken, width, Metrics::base()),
            Passage::Through(NODE_W + GAP_X / 2.),
            "the middle of the channel between the two boxes"
        );
        let Passage::Through(x) = passage(
            &nodes,
            &link,
            &[(1, 1, NODE_W + GAP_X / 2.)],
            width,
            Metrics::base(),
        ) else {
            panic!("a shared rank still has a channel");
        };
        assert!(
            (x - (NODE_W + GAP_X / 2.)).abs() >= LANE_STEP,
            "and it keeps a lane step from the one already taken: {x}"
        );
    }

    #[test]
    fn a_dependency_declared_earlier_is_still_drawn_forwards() {
        // A step may be declared before the step it waits for. `placed` is in
        // rank order, so reading it by declaration index would draw this
        // dependency as a return.
        let workflow = graph(
            r#"
schema = 3
name = "out-of-order"
revision = 1

[[step]]
id = "child"
kind = "harness"
skill = "code"
depends_on = ["root"]

[[step]]
id = "root"
kind = "harness"
skill = "analyze"
depends_on = []
"#,
        );
        let layout = layout(&workflow);
        let link = link(&layout, "root", "child");
        assert!(
            !link.back,
            "a dependency is drawn forwards however the file orders the two steps"
        );
        assert!(
            layout.nodes[link.from].y < layout.nodes[link.to].y,
            "and the step that waits sits below the one it waits for"
        );
    }

    #[test]
    fn two_steps_centred_on_the_same_parents_do_not_overlap() {
        // Both joins are centred between `a` and `c`, between `b` and `d`.
        let workflow = graph(
            r#"
schema = 3
name = "interleaved"
revision = 1

[[step]]
id = "a"
kind = "harness"
skill = "analyze"
depends_on = []

[[step]]
id = "b"
kind = "harness"
skill = "analyze"
depends_on = []

[[step]]
id = "c"
kind = "harness"
skill = "analyze"
depends_on = []

[[step]]
id = "d"
kind = "harness"
skill = "analyze"
depends_on = []

[[step]]
id = "wide"
kind = "harness"
skill = "code"
depends_on = ["a", "d"]

[[step]]
id = "narrow"
kind = "harness"
skill = "code"
depends_on = ["b", "c"]
"#,
        );
        let layout = layout(&workflow);
        for row in layout.nodes.chunk_by(|left, right| left.rank == right.rank) {
            for pair in row.windows(2) {
                assert!(
                    pair[1].x >= pair[0].x + NODE_W + GAP_X,
                    "`{}` and `{}` are a gap apart",
                    pair[0].id,
                    pair[1].id
                );
            }
        }
        assert!(
            read_as_one(&layout).is_empty(),
            "{:?}",
            read_as_one(&layout)
        );
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
            vec!["a->b", "b->c"],
            "the chain is drawn as a chain, top to bottom, not as its closure"
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
            (larger.metrics().track_step, TRACK_STEP),
            (larger.metrics().channel_clear, CHANNEL_CLEAR),
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
            for (low, high) in small.runs.iter().zip(big.runs) {
                assert!((high - low * 1.25).abs() < 0.5);
            }
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

    /// Whether any line of the picture passes through a box's *inside*:
    /// touching a border is what a link into a node does, and is not a
    /// crossing.
    fn lines_through_boxes(layout: &Layout) -> Vec<String> {
        let m = layout.metrics;
        let mut out = Vec::new();
        for link in &layout.links {
            let points = stroke(layout, link).points;
            for node in &layout.nodes {
                let rect = (node.x, node.y, node.x + m.node_w, node.y + m.node_h);
                if points
                    .windows(2)
                    .any(|segment| crosses_box(segment[0], segment[1], rect))
                {
                    out.push(format!(
                        "{} -> {} crosses `{}`",
                        layout.nodes[link.from].id, layout.nodes[link.to].id, node.id
                    ));
                }
            }
        }
        out
    }

    const HAIR: f32 = 0.01;

    /// A point's coordinate along an axis: `0` is x, `1` is y.
    fn at(point: (f32, f32), axis: usize) -> f32 {
        if axis == 0 { point.0 } else { point.1 }
    }

    /// Whether two segments run along one line and share more than a point of
    /// it.
    fn along(a: [(f32, f32); 2], b: [(f32, f32); 2]) -> bool {
        [(0, 1), (1, 0)].into_iter().any(|(fixed, run)| {
            let same_line = [a[0], a[1], b[0], b[1]]
                .iter()
                .all(|point| (at(*point, fixed) - at(a[0], fixed)).abs() < HAIR);
            let ends = |segment: [(f32, f32); 2]| {
                at(segment[0], run).min(at(segment[1], run))
                    ..at(segment[0], run).max(at(segment[1], run))
            };
            let (a, b) = (ends(a), ends(b));
            same_line && a.start.min(b.end) - a.end.max(b.start) > HAIR
        })
    }

    /// Whether a point lies in a segment's inside: on it, and clear of both
    /// its ends.
    fn within(point: (f32, f32), segment: [(f32, f32); 2]) -> bool {
        let vertical = (segment[0].0 - segment[1].0).abs() < HAIR;
        let horizontal = (segment[0].1 - segment[1].1).abs() < HAIR;
        let (fixed, run) = match (vertical, horizontal) {
            (true, _) => (0, 1),
            (false, true) => (1, 0),
            _ => return false,
        };
        let low = at(segment[0], run).min(at(segment[1], run));
        let high = at(segment[0], run).max(at(segment[1], run));
        (at(point, fixed) - at(segment[0], fixed)).abs() < HAIR
            && low + HAIR < at(point, run)
            && at(point, run) < high - HAIR
    }

    /// The pairs of lines that share no step and could still be read as one:
    /// a corner of either lying on the other, or a stretch of one running
    /// along the other. Two lines crossing in mid-stroke are not among them —
    /// that reads as two lines — and neither is anything at a step they both
    /// touch: a fork shares its stem, and a join may share a bar.
    fn read_as_one(layout: &Layout) -> Vec<String> {
        let mut out = Vec::new();
        for (number, link) in layout.links.iter().enumerate() {
            let mine = stroke(layout, link).points;
            for other in layout.links.iter().skip(number + 1) {
                let shared = [link.from, link.to]
                    .iter()
                    .any(|step| [other.from, other.to].contains(step));
                if shared {
                    continue;
                }
                let theirs = stroke(layout, other).points;
                let met = mine.windows(2).any(|mine| {
                    let mine = [mine[0], mine[1]];
                    theirs.windows(2).any(|theirs| {
                        let theirs = [theirs[0], theirs[1]];
                        along(mine, theirs)
                            || within(mine[0], theirs)
                            || within(mine[1], theirs)
                            || within(theirs[0], mine)
                            || within(theirs[1], mine)
                    })
                });
                if met {
                    out.push(format!(
                        "{} -> {} meets {} -> {}",
                        layout.nodes[link.from].id,
                        layout.nodes[link.to].id,
                        layout.nodes[other.from].id,
                        layout.nodes[other.to].id
                    ));
                }
            }
        }
        out
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
