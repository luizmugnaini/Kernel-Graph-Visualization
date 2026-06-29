use eframe::{egui, run_native, App, CreationContext, NativeOptions};
use egui::{Color32, FontId, Pos2, Shape, Stroke, Vec2};
use egui_graphs::{
    DisplayEdge, DisplayNode, DrawContext, EdgeProps, FruchtermanReingoldWithCenterGravity,
    FruchtermanReingoldWithCenterGravityState, Graph, GraphView, LayoutForceDirected, LayoutState,
    Node, NodeProps, SettingsInteraction, SettingsNavigation, SettingsStyle,
};
use petgraph::{
    graph::IndexType,
    stable_graph::{EdgeIndex, NodeIndex, StableGraph},
    Directed, EdgeType,
};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet, VecDeque, hash_map};
use std::sync::Arc;

const BASE_NODE: Color32 = Color32::from_rgb(64, 150, 235);
const SELECTED: Color32 = Color32::from_rgb(205, 80, 255);
const CYCLE: Color32 = Color32::from_rgb(255, 55, 55);
const GREEN: Color32 = Color32::from_rgb(50, 215, 120);
const GREEN_DIM: Color32 = Color32::from_rgb(40, 130, 80);
const YELLOW: Color32 = Color32::from_rgb(255, 225, 60);
const YELLOW_DIM: Color32 = Color32::from_rgb(155, 130, 45);
const HIDDEN: Color32 = Color32::TRANSPARENT;

const LAYOUT_STEPS_MAX_COUNT_MAX_STEPS: u64 = 200;

const GOLDEN_ANGLE_RADIANS: f32 = 2.3999632;

const RANKING_RESULT_MAX_COUNT: usize = 10;

const VISIBLE_LABEL_RADIUS_MIN: f32 = 6.0;
const FONT_SIZE_MIN: f32 = 8.0;
const FONT_SIZE_MAX: f32 = 48.0;

fn is_hidden(color: Color32) -> bool {
    color.a() == 0
}

fn color_faint_edge() -> Color32 {
    Color32::from_rgba_unmultiplied(150, 150, 160, 40)
}

fn color_base_edge() -> Color32 {
    Color32::from_rgba_unmultiplied(160, 162, 172, 150)
}

fn color_lerp(from: Color32, to: Color32, factor: f32) -> Color32 {
    let factor = factor.clamp(0.0, 1.0);
    let blend = |from_channel: u8, to_channel: u8| (from_channel as f32 + (to_channel as f32 - from_channel as f32) * factor).round() as u8;
    Color32::from_rgba_unmultiplied(
        blend(from.r(), to.r()),
        blend(from.g(), to.g()),
        blend(from.b(), to.b()),
        blend(from.a(), to.a()),
    )
}

fn color_fade(level: f32, span: f32, bright: Color32, dim: Color32) -> Color32 {
    let factor = if level <= 1.0 { 0.0 } else { (level - 1.0) / span };
    color_lerp(bright, dim, factor)
}

fn color_brighten(color: Color32) -> Color32 {
    color_lerp(color, Color32::WHITE, 0.4)
}

fn vec2_rotate(vector: Vec2, angle: f32) -> Vec2 {
    let (sin, cos) = angle.sin_cos();
    Vec2::new(cos * vector.x - sin * vector.y, sin * vector.x + cos * vector.y)
}

fn distance_segment_to_point(segment_start: Pos2, segment_end: Pos2, point: Pos2) -> f32 {
    let segment = segment_end - segment_start;
    let length_squared = segment.dot(segment);
    if length_squared <= f32::EPSILON {
        return (point - segment_start).length();
    }
    let factor = ((point - segment_start).dot(segment) / length_squared).clamp(0.0, 1.0);
    let projection = segment_start + segment * factor;
    (point - projection).length()
}

#[derive(Clone)]
pub struct NodeData {
    radius: f32,
    label: Arc<str>, // Necessary Arc because egui_graphs clones every node for each incident edge, every frame.
}

type EdgeColor = Color32;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Edge {
    source: usize,
    target: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct RankEntry {
    position: usize,
    count: usize,
}

#[derive(Clone)]
pub struct IncludeNodeShape {
    location: Pos2,
    radius: f32,
    color: Option<Color32>,
    label: Arc<str>,
    selected: bool,
    dragged: bool,
    hovered: bool,
}

impl From<NodeProps<NodeData>> for IncludeNodeShape {
    fn from(props: NodeProps<NodeData>) -> Self {
        Self {
            location: props.location(),
            radius: props.payload.radius.max(1.0),
            color: props.color(),
            label: props.payload.label.clone(),
            selected: props.selected,
            dragged: props.dragged,
            hovered: props.hovered,
        }
    }
}

impl<E: Clone, Ty: EdgeType, Idx: IndexType> DisplayNode<NodeData, E, Ty, Idx> for IncludeNodeShape {
    fn closest_boundary_point(&self, direction: Vec2) -> Pos2 {
        self.location + direction.normalized() * self.radius
    }

    fn is_inside(&self, pos: Pos2) -> bool {
        let color = self.color.unwrap_or(BASE_NODE);
        if is_hidden(color) {
            return false; // Hidden node, not pickable.
        }
        (pos - self.location).length() <= self.radius
    }

    fn shapes(&mut self, ctx: &DrawContext) -> Vec<Shape> {
        let color = self.color.unwrap_or(BASE_NODE);
        if is_hidden(color) {
            return vec![];
        }
        let center = ctx.meta.canvas_to_screen_pos(self.location);
        let radius = ctx.meta.canvas_to_screen_size(self.radius).max(1.0);
        let mut shapes = vec![Shape::circle_filled(center, radius, color)];

        let interacted = self.selected || self.dragged || self.hovered;
        if interacted || radius >= VISIBLE_LABEL_RADIUS_MIN {
            let font_size = (radius * 1.6).clamp(FONT_SIZE_MIN, FONT_SIZE_MAX);
            let font = FontId::proportional(font_size);
            let galley = ctx.painter.layout_no_wrap(self.label.to_string(), font, Color32::WHITE);
            let pos = center + Vec2::new(-galley.size().x / 2.0, radius + 2.0);
            shapes.push(Shape::galley(pos, galley, Color32::WHITE));
        }
        shapes
    }

    fn update(&mut self, props: &NodeProps<NodeData>) {
        *self = props.clone().into();
    }
}

#[derive(Clone)]
pub struct ColoredEdgeShape {
    color: Color32,
    selected: bool,
    width: f32,
    tip_size: f32,
    tip_angle: f32,
}

impl From<EdgeProps<EdgeColor>> for ColoredEdgeShape {
    fn from(props: EdgeProps<EdgeColor>) -> Self {
        Self {
            color: props.payload,
            selected: props.selected,
            width: 1.6,
            tip_size: 9.0,
            tip_angle: std::f32::consts::TAU / 30.0,
        }
    }
}

impl<N: Clone, Ty: EdgeType, Idx: IndexType, D: DisplayNode<N, EdgeColor, Ty, Idx>>
    DisplayEdge<N, EdgeColor, Ty, Idx, D> for ColoredEdgeShape
{
    fn shapes(
        &mut self,
        start: &Node<N, EdgeColor, Ty, Idx, D>,
        end: &Node<N, EdgeColor, Ty, Idx, D>,
        ctx: &DrawContext,
    ) -> Vec<Shape> {
        if is_hidden(self.color) || start.id() == end.id() {
            return vec![];
        }

        let start_location = start.location();
        let end_location = end.location();
        let delta = end_location - start_location;
        let length = delta.length();
        if !length.is_finite() || length <= 1e-5 {
            return vec![];
        }

        let direction = delta / length;
        let start_point = start.display().closest_boundary_point(direction);
        let end_point = end.display().closest_boundary_point(-direction);

        let color = if self.selected {
            color_brighten(self.color)
        } else {
            self.color
        };

        let width = ctx.meta.canvas_to_screen_size(self.width).max(0.6);
        let stroke = Stroke::new(width, color);
        let directed = ctx.is_directed;
        let line_end = if directed {
            end_point - direction * self.tip_size
        } else {
            end_point
        };

        let screen_start = ctx.meta.canvas_to_screen_pos(start_point);
        let screen_line_end = ctx.meta.canvas_to_screen_pos(line_end);

        let mut shapes = vec![Shape::line_segment([screen_start, screen_line_end], stroke)];
        if directed {
            let tip_left = end_point - vec2_rotate(direction, self.tip_angle) * self.tip_size;
            let tip_right = end_point - vec2_rotate(direction, -self.tip_angle) * self.tip_size;
            let arrow_polygon = vec![
                ctx.meta.canvas_to_screen_pos(end_point),
                ctx.meta.canvas_to_screen_pos(tip_left),
                ctx.meta.canvas_to_screen_pos(tip_right),
            ];
            shapes.push(Shape::convex_polygon(arrow_polygon, color, Stroke::NONE));
        }

        shapes
    }

    fn update(&mut self, props: &EdgeProps<EdgeColor>) {
        self.color = props.payload;
        self.selected = props.selected;
    }

    fn is_inside(&self, start: &Node<N, EdgeColor, Ty, Idx, D>, end: &Node<N, EdgeColor, Ty, Idx, D>, pos: Pos2) -> bool {
        if is_hidden(self.color) {
            return false;
        }
        distance_segment_to_point(start.location(), end.location(), pos) <= self.width.max(2.0)
    }
}

#[derive(Default)]
struct GlobalGraphStats {
    most_included: usize,
    most_includes: usize,
    files_in_cycles: usize,
}

struct SelectionInfo {
    selected_position: usize,
    includer_depth: HashMap<usize, usize>,
    include_depth: HashMap<usize, usize>,
    subgraph: HashSet<usize>,
    cycle_set: HashSet<usize>,
}

impl SelectionInfo {
    fn positions_excluding_self(&self, depths: &HashMap<usize, usize>) -> Vec<usize> {
        depths.keys().copied().filter(|&position| position != self.selected_position).collect()
    }

    fn ancestors(&self) -> Vec<usize> {
        self.positions_excluding_self(&self.includer_depth)
    }

    fn descendants(&self) -> Vec<usize> {
        self.positions_excluding_self(&self.include_depth)
    }

    fn count_dependents(&self) -> usize {
        self.includer_depth.len().saturating_sub(1)
    }

    fn count_transitive_includes(&self) -> usize {
        self.include_depth.len().saturating_sub(1)
    }

    fn max_includer_depth(&self) -> usize {
        self.includer_depth.values().copied().max().unwrap_or(0)
    }
}

type IncludeGraph = Graph<NodeData, EdgeColor, Directed, u32, IncludeNodeShape, ColoredEdgeShape>;

struct InfoRow {
    label: String,
    node: NodeIndex,
}

struct InfoColumn {
    title: String,
    rows: Vec<InfoRow>,
}

struct LoadedGraph {
    graph: StableGraph<NodeData, EdgeColor>,
    nodes: Vec<NodeIndex>,
}

pub struct KernelGraphApp {
    g: IncludeGraph,
    physics_initialized: bool,
    filter_text: String,

    node_count: usize,
    node_indices: Vec<NodeIndex>,
    node_position: Vec<usize>,

    position_labels: Vec<String>,
    position_forward: Vec<Vec<usize>>,
    position_reverse: Vec<Vec<usize>>,
    position_in_degree: Vec<usize>,
    position_out_degree: Vec<usize>,

    edge_list: Vec<(EdgeIndex, Edge)>,

    // Cycle detection (back edges): `cycle_edges` is the set of edges that close a directed cycle;
    // `cycle_edge_list` is the same, sorted by label for display.
    cycle_edges: HashSet<Edge>,
    cycle_edge_list: Vec<Edge>,

    stats: GlobalGraphStats,

    impact_ranking: Vec<RankEntry>, // Highest transitive includer count (rebuild impact), in descending order.
    compile_weight_ranking: Vec<RankEntry>, // Highest transitive include count (compile cost), in descending order.

    // Interaction state.
    selected: Option<NodeIndex>,
    selection: Option<SelectionInfo>,
    show_info: bool,
    show_stats: bool,
    info_filters: Vec<String>, // Per-info-column filter text, indexed by column.
    finder_open: bool,
    finder_query: String,
    request: Option<NodeIndex>, // A node selection pending from a window/panel click, applied next frame.
    layout_settling: bool, // Layout layout_settling: the force layout runs for a fixed number of steps.
    fit_pending: bool,
}

impl KernelGraphApp {
    pub fn new(_cc: &CreationContext) -> Self {
        let mut app = Self {
            g: Graph::new(StableGraph::new()),
            physics_initialized: false,
            filter_text: String::new(),
            node_count: 0,
            node_indices: Vec::new(),
            node_position: Vec::new(),
            position_labels: Vec::new(),
            position_forward: Vec::new(),
            position_reverse: Vec::new(),
            position_in_degree: Vec::new(),
            position_out_degree: Vec::new(),
            edge_list: Vec::new(),
            cycle_edges: HashSet::new(),
            cycle_edge_list: Vec::new(),
            stats: GlobalGraphStats::default(),
            impact_ranking: Vec::new(),
            compile_weight_ranking: Vec::new(),
            selected: None,
            selection: None,
            show_info: false,
            show_stats: false,
            info_filters: Vec::new(),
            finder_open: false,
            finder_query: String::new(),
            request: None,
            layout_settling: true,
            fit_pending: false,
        };
        app.reload_graph();
        app
    }

    fn reload_graph(&mut self) {
        if let Ok(LoadedGraph { graph: stable_g, nodes }) = generate_graph(&self.filter_text) {
            let mut new_g: IncludeGraph = Graph::from(&stable_g);

            let mut order: f32 = 0.0;
            for idx in nodes {
                if let Some(node) = new_g.node_mut(idx) {
                    let angle = order * GOLDEN_ANGLE_RADIANS;
                    let radius = 30.0 * order.sqrt();
                    node.set_location(egui::pos2(radius * angle.cos(), radius * angle.sin()));
                    order += 1.0;
                }
            }

            self.g = new_g;
            self.physics_initialized = false;
            self.selected = None;
            self.selection = None;
            self.layout_settling = true;
            self.fit_pending = true;
            self.analyze_graph();
            self.recolor();
        }
    }

    fn analyze_graph(&mut self) {
        let node_indices: Vec<NodeIndex> = self.g.g().node_indices().collect();
        let node_count = node_indices.len();

        let mut node_position = vec![0usize; node_count];
        for (position, &node_index) in node_indices.iter().enumerate() {
            node_position[node_index.index()] = position;
        }
        let labels: Vec<String> = node_indices
            .iter()
            .map(|&node_index| {
                self.g
                    .node(node_index)
                    .map(|node| node.payload().label.to_string())
                    .unwrap_or_default()
            })
            .collect();

        let mut forward = vec![Vec::new(); node_count];
        let mut reverse = vec![Vec::new(); node_count];
        let mut edge_list = Vec::new();
        for edge_index in self.g.g().edge_indices() {
            if let Some((source, target)) = self.g.g().edge_endpoints(edge_index) {
                let (source_pos, target_pos) = (node_position[source.index()], node_position[target.index()]);
                forward[source_pos].push(target_pos);
                reverse[target_pos].push(source_pos);
                edge_list.push((edge_index, Edge { source: source_pos, target: target_pos }));
            }
        }

        let in_degree: Vec<usize> = reverse.iter().map(Vec::len).collect();
        let out_degree: Vec<usize> = forward.iter().map(Vec::len).collect();

        let cycle_edges = find_cycle_edges(&forward);
        let mut cycle_nodes: HashSet<usize> = HashSet::new();
        for edge in &cycle_edges {
            cycle_nodes.insert(edge.source);
            cycle_nodes.insert(edge.target);
        }
        let mut cycle_edge_list: Vec<Edge> = cycle_edges.iter().copied().collect();
        cycle_edge_list.sort_by(|left, right| {
            labels[left.source].cmp(&labels[right.source]).then_with(|| labels[left.target].cmp(&labels[right.target]))
        });
        let files_in_cycles = cycle_nodes.len();

        let most_included = (0..node_count).max_by_key(|&position| in_degree[position]).unwrap_or(0);
        let most_includes = (0..node_count).max_by_key(|&position| out_degree[position]).unwrap_or(0);

        let transitive_includers: Vec<usize> = (0..node_count)
            .map(|position| bfs_depths(position, &reverse).len().saturating_sub(1))
            .collect();
        let transitive_includes: Vec<usize> = (0..node_count)
            .map(|position| bfs_depths(position, &forward).len().saturating_sub(1))
            .collect();

        // Standard-library headers are excluded from the rebuild-impact rank.
        let impact_ranking = top_ranking(&transitive_includers, &labels, RANKING_RESULT_MAX_COUNT, is_standard_header);
        let compile_weight_ranking = top_ranking(&transitive_includes, &labels, RANKING_RESULT_MAX_COUNT, |_| false);

        // Degree-scaled node radius.
        for position in 0..node_count {
            let degree = in_degree[position] + out_degree[position];
            let radius = 4.0 + 2.5 * (degree as f32).sqrt();
            if let Some(node) = self.g.node_mut(node_indices[position]) {
                node.payload_mut().radius = radius;
            }
        }

        self.stats = GlobalGraphStats {
            most_included,
            most_includes,
            files_in_cycles,
        };
        self.impact_ranking = impact_ranking;
        self.compile_weight_ranking = compile_weight_ranking;
        self.node_count = node_count;
        self.node_indices = node_indices;
        self.node_position = node_position;
        self.position_labels = labels;
        self.position_forward = forward;
        self.position_reverse = reverse;
        self.position_in_degree = in_degree;
        self.position_out_degree = out_degree;
        self.edge_list = edge_list;
        self.cycle_edges = cycle_edges;
        self.cycle_edge_list = cycle_edge_list;
    }

    fn compute_selection(&mut self, selected_position: usize) {
        let includer_depth = bfs_depths(selected_position, &self.position_reverse);
        let include_depth = bfs_depths(selected_position, &self.position_forward);

        let mut subgraph: HashSet<usize> = includer_depth.keys().copied().collect();
        subgraph.extend(include_depth.keys().copied());

        let mut cycle_set: HashSet<usize> = HashSet::new();
        for edge in &self.cycle_edges {
            if subgraph.contains(&edge.source) && subgraph.contains(&edge.target) {
                cycle_set.insert(edge.source);
                cycle_set.insert(edge.target);
            }
        }

        self.selection = Some(SelectionInfo {
            selected_position,
            includer_depth,
            include_depth,
            subgraph,
            cycle_set,
        });
    }

    fn recolor(&mut self) {
        let mut node_colors: Vec<(NodeIndex, Color32)> = Vec::with_capacity(self.node_count);
        let mut edge_colors: Vec<(EdgeIndex, Color32)> = Vec::with_capacity(self.edge_list.len());

        match &self.selection {
            None => {
                for position in 0..self.node_count {
                    node_colors.push((self.node_indices[position], BASE_NODE));
                }
                for &(edge_index, _) in &self.edge_list {
                    edge_colors.push((edge_index, color_base_edge()));
                }
            }
            Some(selection) => {
                let includer_span = (selection
                    .includer_depth
                    .values()
                    .copied()
                    .max()
                    .unwrap_or(1)
                    .saturating_sub(1))
                .max(1) as f32;
                let include_span = (selection
                    .include_depth
                    .values()
                    .copied()
                    .max()
                    .unwrap_or(1)
                    .saturating_sub(1))
                .max(1) as f32;

                for position in 0..self.node_count {
                    let color = if !selection.subgraph.contains(&position) {
                        HIDDEN
                    } else if position == selection.selected_position {
                        SELECTED
                    } else if selection.cycle_set.contains(&position) {
                        CYCLE
                    } else if let Some(&level) = selection.includer_depth.get(&position) {
                        color_fade(level as f32, includer_span, GREEN, GREEN_DIM)
                    } else if let Some(&level) = selection.include_depth.get(&position) {
                        color_fade(level as f32, include_span, YELLOW, YELLOW_DIM)
                    } else {
                        BASE_NODE
                    };
                    node_colors.push((self.node_indices[position], color));
                }

                for &(edge_index, Edge { source, target }) in &self.edge_list {
                    let both = selection.subgraph.contains(&source) && selection.subgraph.contains(&target);
                    let color = if !both {
                        HIDDEN
                    } else if self.cycle_edges.contains(&Edge { source, target }) {
                        CYCLE
                    } else if selection.includer_depth.contains_key(&source)
                        && selection.includer_depth.contains_key(&target)
                    {
                        let level = selection.includer_depth[&source].max(selection.includer_depth[&target]) as f32;
                        color_fade(level, includer_span, GREEN, GREEN_DIM)
                    } else if selection.include_depth.contains_key(&source)
                        && selection.include_depth.contains_key(&target)
                    {
                        let level = selection.include_depth[&source].max(selection.include_depth[&target]) as f32;
                        color_fade(level, include_span, YELLOW, YELLOW_DIM)
                    } else {
                        color_faint_edge()
                    };
                    edge_colors.push((edge_index, color));
                }
            }
        }

        for (node_index, color) in node_colors {
            if let Some(node) = self.g.node_mut(node_index) {
                node.set_color(color);
            }
        }
        for (edge_index, color) in edge_colors {
            if let Some(edge) = self.g.edge_mut(edge_index) {
                *edge.payload_mut() = color;
            }
        }
    }

    fn set_selection_flags(&mut self, target: Option<NodeIndex>) {
        let node_indices = self.node_indices.clone();
        for node_index in node_indices {
            if let Some(node) = self.g.node_mut(node_index) {
                node.set_selected(Some(node_index) == target);
            }
        }
        self.g.set_selected_nodes(target.into_iter().collect());
    }

    fn apply_select(&mut self, idx: NodeIndex) {
        self.set_selection_flags(Some(idx));
        self.selected = Some(idx);
        if let Some(&position) = self.node_position.get(idx.index()) {
            self.compute_selection(position);
        }
        self.recolor();
    }

    fn apply_deselect(&mut self) {
        self.set_selection_flags(None);
        self.selected = None;
        self.selection = None;
        self.recolor();
    }

    fn sync_click_selection(&mut self) {
        let observed = self.g.selected_nodes().first().copied();
        if observed != self.selected {
            match observed {
                Some(idx) => self.apply_select(idx),
                None => self.apply_deselect(),
            }
        }
    }

    fn rank_matches(&self, query: &str) -> Vec<InfoRow> {
        let query_lower = query.to_lowercase();
        let mut scored: Vec<(bool, bool, usize, usize)> = Vec::new();
        for position in 0..self.node_count {
            let label_lower = self.position_labels[position].to_lowercase();
            if !query_lower.is_empty() && !label_lower.contains(&query_lower) {
                continue;
            }
            scored.push((
                label_lower != query_lower,
                !label_lower.starts_with(&query_lower),
                self.position_labels[position].len(),
                position,
            ));
        }
        scored.sort();
        scored
            .into_iter()
            .map(|entry| InfoRow {
                label: self.position_labels[entry.3].clone(),
                node: self.node_indices[entry.3],
            })
            .collect()
    }

    fn build_info_columns(&self) -> Vec<InfoColumn> {
        let Some(selection) = &self.selection else {
            return Vec::new();
        };

        let make_rows = |positions: &[usize]| {
            let mut rows: Vec<InfoRow> = positions
                .iter()
                .map(|&position| InfoRow {
                    label: self.position_labels[position].clone(),
                    node: self.node_indices[position],
                })
                .collect();
            rows.sort_by(|left, right| left.label.cmp(&right.label));
            rows
        };
        let selected = selection.selected_position;
        let cycle_members: Vec<usize> = selection.cycle_set.iter().copied().collect();
        vec![
            InfoColumn { title: "Impact (includers)".to_string(), rows: make_rows(&selection.ancestors()) },
            InfoColumn { title: "Direct includers".to_string(), rows: make_rows(&self.position_reverse[selected]) },
            InfoColumn { title: "Direct includes".to_string(), rows: make_rows(&self.position_forward[selected]) },
            InfoColumn { title: "Transitive includes".to_string(), rows: make_rows(&selection.descendants()) },
            InfoColumn { title: "Cycle members".to_string(), rows: make_rows(&cycle_members) },
        ]
    }
}

impl App for KernelGraphApp {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        let typing = ctx.memory(|m| m.focused().is_some());
        let (key_info, key_stats, key_fit, key_find) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::I),
                i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::F),
                i.key_pressed(egui::Key::P) && i.modifiers.command,
            )
        });
        if key_find {
            self.finder_open = true;
        }
        if !typing {
            if key_info {
                self.show_info = !self.show_info;
            }
            if key_stats {
                self.show_stats = !self.show_stats;
            }
            if key_fit {
                self.fit_pending = true;
            }
        }

        if let Some(idx) = self.request.take() {
            self.apply_select(idx);
        }

        egui::Panel::top("controls").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Include graph");
                if ui.button("Fit view (F)").clicked() {
                    self.fit_pending = true;
                }
                if ui.button("Statistics (S)").clicked() {
                    self.show_stats = !self.show_stats;
                }
                if ui.button("Info (I)").clicked() {
                    self.show_info = !self.show_info;
                }
                if ui.button("Find (Ctrl-P)").clicked() {
                    self.finder_open = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Directories (comma-separated prefixes):");
                ui.text_edit_singleline(&mut self.filter_text);
                if ui.button("Filter & recreate").clicked() {
                    self.reload_graph();
                }
            });
        });

        egui::Panel::left("Selection")
            .resizable(true)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let Some(selection) = &self.selection else {
                        ui.label("Click a file to view its impact.");
                        return;
                    };
                    ui.strong(format!("Selected: {}", self.position_labels[selection.selected_position]));
                    ui.label(format!("Impact: {} file(s) recompile", selection.count_dependents()));
                    ui.label(format!(
                        "Direct includers: {}",
                        self.position_in_degree[selection.selected_position]
                    ));
                    ui.label(format!(
                        "Direct includes: {}",
                        self.position_out_degree[selection.selected_position]
                    ));
                    ui.label(format!("Transitive includes: {}", selection.count_transitive_includes()));
                    ui.label(format!("Include depth: {} level(s)", selection.max_includer_depth()));
                    if !selection.cycle_set.is_empty() {
                        ui.colored_label(
                            CYCLE,
                            format!("In include cycle ({} files)", selection.cycle_set.len()),
                        );
                    }
                });
            });

        // Graph view.
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let mut state = FruchtermanReingoldWithCenterGravityState::load(ui, None);
            if !self.physics_initialized {
                state.base.c_repulse = 20.0;
                state.base.c_attract = 0.001;
                state.base.k_scale = 20.0;
                state.base.dt = 1.0;
                state.base.is_running = true;
                state.base.step_count = 0;
                self.layout_settling = true;
                self.physics_initialized = true;
            }

            if self.layout_settling && state.base.step_count >= LAYOUT_STEPS_MAX_COUNT_MAX_STEPS {
                state.base.is_running = false;
                self.layout_settling = false;
                self.fit_pending = true;
            }

            if self.layout_settling && self.node_count > 0 {
                let percent = state.base.step_count.min(LAYOUT_STEPS_MAX_COUNT_MAX_STEPS) * 100 / LAYOUT_STEPS_MAX_COUNT_MAX_STEPS;
                ui.horizontal(|ui| ui.label(format!("Computing layout... {percent}%")));
            }
            let running = state.base.is_running;
            state.save(ui, None);

            let style = SettingsStyle::default()
                .with_labels_always(true)
                .with_node_stroke_hook(|selected, dragged, node_color, stroke, egui_style| {
                    let mut s = stroke;
                    s.color = node_color.unwrap_or(egui_style.visuals.widgets.inactive.fg_stroke.color);
                    if selected {
                        s.color = Color32::WHITE;
                        s.width = 2.5;
                    }
                    if dragged {
                        s.color = Color32::LIGHT_BLUE;
                    }
                    s
                });
            let nav = SettingsNavigation::default()
                // The view is fit once when the layout settles (and on demand via `fit_pending`,
                // e.g. the "Fit view" button); zoom/pan is disabled while the layout is running.
                .with_fit_to_screen_enabled(self.fit_pending)
                .with_zoom_and_pan_enabled(!self.layout_settling)
                // Larger zoom step per event: egui_graphs zooms a fixed step per Ctrl+scroll /
                // pinch event (magnitude ignored), so a bigger step keeps zoom responsive even
                // when the frame rate is low on large graphs.
                .with_zoom_speed(0.25);
            let interaction = SettingsInteraction::default()
                .with_node_selection_enabled(true)
                .with_dragging_enabled(true);

                ui.add(&mut GraphView::<_, _, _, _, IncludeNodeShape, ColoredEdgeShape, FruchtermanReingoldWithCenterGravityState, LayoutForceDirected<FruchtermanReingoldWithCenterGravity>>
                    ::new(&mut self.g)
                    .with_styles(&style)
                    .with_navigations(&nav)
                    .with_interactions(&interaction),
            );

            // Only keep animating while the layout is still being computed; once paused,
            // the graph is static (and egui still repaints on user interaction).
            self.fit_pending = false; // The one-shot fit (if any) has been applied this frame.
            if running {
                ui.ctx().request_repaint();
            }

            self.sync_click_selection();
        });

        // Graph stats window (toggled with S): global, codebase-wide analysis.
        if self.show_stats {
            let mut open = true;
            let mut clicked: Option<NodeIndex> = None;
            egui::Window::new("Graph stats")
                .open(&mut open)
                .resizable(true)
                .default_width(440.0)
                .show(&ctx, |ui| {
                    if self.node_count == 0 {
                        ui.label("No files. Adjust the directory filter and recreate.");
                        return;
                    }
                    let stats = &self.stats;
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.label(format!("File count: {}", self.node_count));
                        if ui
                            .selectable_label(
                                false,
                                format!(
                                    "Most included: {} ({} direct includers)",
                                    self.position_labels[stats.most_included],
                                    self.position_in_degree[stats.most_included]
                                ),
                            )
                            .clicked()
                        {
                            clicked = Some(self.node_indices[stats.most_included]);
                        }
                        if ui
                            .selectable_label(
                                false,
                                format!(
                                    "Most includes: {} ({} direct includes)",
                                    self.position_labels[stats.most_includes],
                                    self.position_out_degree[stats.most_includes]
                                ),
                            )
                            .clicked()
                        {
                            clicked = Some(self.node_indices[stats.most_includes]);
                        }

                        ui.separator();
                        ui.strong("Highest rebuild impact");
                        for &RankEntry { position, count } in &self.impact_ranking {
                            let text = format!("{} - {} files include", self.position_labels[position], count);
                            if ui.selectable_label(false, text).clicked() {
                                clicked = Some(self.node_indices[position]);
                            }
                        }

                        ui.separator();
                        ui.strong("Heaviest to compile");
                        for &RankEntry { position, count } in &self.compile_weight_ranking {
                            let text = format!("{} - {} headers included", self.position_labels[position], count);
                            if ui.selectable_label(false, text).clicked() {
                                clicked = Some(self.node_indices[position]);
                            }
                        }

                        if !self.cycle_edge_list.is_empty() {
                            ui.separator();
                            ui.colored_label(
                                CYCLE,
                                format!(
                                    "Cycles: {} back-edge(s), {} files",
                                    self.cycle_edges.len(),
                                    stats.files_in_cycles
                                ),
                            );
                            for &Edge { source, target } in &self.cycle_edge_list {
                                let text =
                                    format!("{} and {}", self.position_labels[source], self.position_labels[target]);
                                if ui.selectable_label(false, text).clicked() {
                                    clicked = Some(self.node_indices[source]);
                                }
                            }
                        }
                    });
                });
            self.show_stats = open;
            if let Some(idx) = clicked {
                self.request = Some(idx);
            }
        }

        // Info window about the selected node.
        if self.show_info {
            let mut open = true;
            let columns = self.build_info_columns();

            let mut filters = std::mem::take(&mut self.info_filters);
            filters.resize(columns.len(), String::new());

            let mut clicked: Option<NodeIndex> = None;
            egui::Window::new("Include info")
                .open(&mut open)
                .resizable(true)
                .default_width(820.0)
                .show(&ctx, |ui| {
                    if columns.is_empty() {
                        ui.label("Please, select a node first!");
                        return;
                    }
                    ui.horizontal_top(|ui| {
                        for (column_index, column) in columns.iter().enumerate() {
                            ui.vertical(|ui| {
                                ui.set_min_width(150.0);
                                ui.strong(format!("{} ({})", column.title, column.rows.len()));

                                let filter = &mut filters[column_index];
                                ui.add(egui::TextEdit::singleline(filter).hint_text("filter..."));

                                let filter_lower = filter.to_lowercase();
                                egui::ScrollArea::vertical()
                                    .id_salt(("infocol", column_index))
                                    .max_height(440.0)
                                    .show(ui, |ui| {
                                        for row in &column.rows {
                                            if !filter_lower.is_empty() && !row.label.to_lowercase().contains(&filter_lower) {
                                                continue;
                                            }
                                            if ui.selectable_label(false, row.label.as_str()).clicked() {
                                                clicked = Some(row.node);
                                            }
                                        }
                                    });
                            });
                        }
                    });
                });
            self.info_filters = filters;
            self.show_info = open;
            if let Some(idx) = clicked {
                self.request = Some(idx);
            }
        }

        // Ctrl-P finder.
        if self.finder_open {
            let mut open = true;
            let mut query = std::mem::take(&mut self.finder_query);
            let ranked = self.rank_matches(&query);
            let mut clicked: Option<NodeIndex> = None;
            egui::Window::new("Find file (Ctrl-P)")
                .open(&mut open)
                .resizable(true)
                .default_width(460.0)
                .show(&ctx, |ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut query).hint_text("type a file name..."),
                    );
                    response.request_focus();
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for row in ranked.iter().take(300) {
                            if ui.selectable_label(false, row.label.as_str()).clicked() {
                                clicked = Some(row.node);
                            }
                        }
                    });
                    if enter && let Some(row) = ranked.first() {
                        clicked = Some(row.node);
                    }
                });
            self.finder_query = query;
            self.finder_open = open;
            if let Some(idx) = clicked {
                self.request = Some(idx);
                self.finder_open = false;
            }
        }
    }
}

fn bfs_depths(start: usize, adjacency: &[Vec<usize>]) -> HashMap<usize, usize> {
    let mut depth = HashMap::new();
    depth.insert(start, 0usize);
    let mut queue = VecDeque::new();
    queue.push_back(start);
    while let Some(node) = queue.pop_front() {
        let next_depth = depth[&node] + 1;
        for &neighbor in &adjacency[node] {
            if let hash_map::Entry::Vacant(entry) = depth.entry(neighbor) {
                entry.insert(next_depth);
                queue.push_back(neighbor);
            }
        }
    }
    depth
}

const STANDARD_HEADERS: &[&str] = &[
    // C standard library (<xxx.h>).
    "assert.h", "complex.h", "ctype.h", "errno.h", "fenv.h", "float.h", "inttypes.h", "iso646.h",
    "limits.h", "locale.h", "math.h", "setjmp.h", "signal.h", "stdalign.h", "stdarg.h",
    "stdatomic.h", "stdbool.h", "stddef.h", "stdint.h", "stdio.h", "stdlib.h", "stdnoreturn.h",
    "string.h", "tgmath.h", "threads.h", "time.h", "uchar.h", "wchar.h", "wctype.h",
    // C++ wrappers for the C library (<cxxx>).
    "cassert", "ccomplex", "cctype", "cerrno", "cfenv", "cfloat", "cinttypes", "ciso646",
    "climits", "clocale", "cmath", "csetjmp", "csignal", "cstdalign", "cstdarg", "cstdbool",
    "cstddef", "cstdint", "cstdio", "cstdlib", "cstring", "ctgmath", "ctime", "cuchar", "cwchar",
    "cwctype",
    // C++ standard library (STL), no extension.
    "algorithm", "any", "array", "atomic", "barrier", "bit", "bitset", "charconv", "chrono",
    "codecvt", "compare", "complex", "concepts", "condition_variable", "coroutine", "deque",
    "exception", "execution", "filesystem", "format", "forward_list", "fstream", "functional",
    "future", "initializer_list", "iomanip", "ios", "iosfwd", "iostream", "istream", "iterator",
    "latch", "limits", "list", "locale", "map", "memory", "memory_resource", "mutex", "new",
    "numbers", "numeric", "optional", "ostream", "queue", "random", "ranges", "ratio", "regex",
    "scoped_allocator", "semaphore", "set", "shared_mutex", "source_location", "span", "sstream",
    "stack", "stdexcept", "stop_token", "streambuf", "string", "string_view", "syncstream",
    "system_error", "thread", "tuple", "type_traits", "typeindex", "typeinfo", "unordered_map",
    "unordered_set", "utility", "valarray", "variant", "vector", "version",
];

fn is_standard_header(label: &str) -> bool {
    STANDARD_HEADERS.contains(&label)
}

fn top_ranking(
    values: &[usize],
    labels: &[String],
    limit: usize,
    exclude: impl Fn(&str) -> bool,
) -> Vec<RankEntry> {
    let mut ranked: Vec<RankEntry> = values
        .iter()
        .copied()
        .enumerate()
        .filter(|&(position, count)| count > 0 && !exclude(&labels[position]))
        .map(|(position, count)| RankEntry { position, count })
        .collect();
    ranked.sort_by(|left, right| {
        right.count.cmp(&left.count).then_with(|| labels[left.position].cmp(&labels[right.position]))
    });
    ranked.truncate(limit);
    ranked
}

// Detect directed cycles over a dense forward-adjacency list using an iterative DFS.
//
// Returns the set of "back edges" - edges `(node, neighbor)` where `neighbor` is still on the
// current DFS stack, which means the edge closes a directed cycle.
fn find_cycle_edges(forward: &[Vec<usize>]) -> HashSet<Edge> {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unvisited,
        OnStack,
        Done,
    }

    let node_count = forward.len();
    let mut state = vec![State::Unvisited; node_count];
    let mut back_edges: HashSet<Edge> = HashSet::new();

    for root in 0..node_count {
        if state[root] != State::Unvisited {
            continue;
        }
        state[root] = State::OnStack;

        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&(node, child_index)) = work.last() {
            if child_index < forward[node].len() {
                work.last_mut().unwrap().1 += 1;
                let neighbor = forward[node][child_index];
                match state[neighbor] {
                    State::Unvisited => {
                        state[neighbor] = State::OnStack;
                        work.push((neighbor, 0));
                    }
                    State::OnStack => {
                        // Neighbor is an ancestor on the current path. The edge closes a cycle.
                        back_edges.insert(Edge { source: node, target: neighbor });
                    }
                    State::Done => {} // Forward or cross edge: not part of a cycle.
                }
            } else {
                state[node] = State::Done;
                work.pop();
            }
        }
    }

    back_edges
}

fn generate_graph(filter_input: &str) -> Result<LoadedGraph, rusqlite::Error> {
    let mut g = StableGraph::new();
    let conn = Connection::open("kernel_graph.db")?;
    let mut id_map: HashMap<i32, NodeIndex> = HashMap::new();
    let mut nodes = Vec::new();

    let filters: Vec<&str> = filter_input
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if filters.is_empty() {
        return Ok(LoadedGraph { graph: g, nodes });
    }

    let mut cond_path = Vec::new();
    let mut cond_f_source = Vec::new();
    let mut cond_f_target = Vec::new();
    for f in filters {
        cond_path.push(format!("path LIKE '{f}%'"));
        cond_f_source.push(format!("f_source.path LIKE '{f}%'"));
        cond_f_target.push(format!("f_target.path LIKE '{f}%'"));
    }
    let where_path = cond_path.join(" OR ");
    let where_f_source = cond_f_source.join(" OR ");
    let where_f_target = cond_f_target.join(" OR ");

    let query_files = format!(
        "SELECT id, path FROM Files WHERE {where_path}
         UNION
         SELECT f_target.id, f_target.path
         FROM Edges e
         JOIN Files f_source ON e.source_id = f_source.id
         JOIN Files f_target ON e.target_id = f_target.id
         WHERE {where_f_source}"
    );

    let mut stmt_files = conn.prepare(&query_files)?;
    let file_iter = stmt_files.query_map([], |row| Ok((row.get::<_, i32>(0)?, row.get::<_, String>(1)?)))?;
    for result in file_iter {
        let (id, path) = result?;
        let node_idx = g.add_node(NodeData {
            radius: 4.0,
            label: Arc::from(path.as_str()),
        });
        nodes.push(node_idx);
        id_map.insert(id, node_idx);
    }

    // Load every edge incident to a selected file on EITHER end (not just outgoing edges). The
    // both endpoints in `id_map` guard below keeps only edges between displayed nodes, so this
    // yields the complete induced subgraph - otherwise a reverse edge whose source falls outside
    // the prefix would be dropped and cycles straddling the filter boundary would be missed.
    let query_edges = format!(
        "SELECT e.source_id, e.target_id
         FROM Edges e
         JOIN Files f_source ON e.source_id = f_source.id
         JOIN Files f_target ON e.target_id = f_target.id
         WHERE ({where_f_source}) OR ({where_f_target})"
    );
    let mut stmt_edges = conn.prepare(&query_edges)?;
    let edge_iter = stmt_edges.query_map([], |row| Ok((row.get::<_, i32>(0)?, row.get::<_, i32>(1)?)))?;
    for result in edge_iter {
        let (source_id, target_id) = result?;
        if let (Some(&s_idx), Some(&t_idx)) = (id_map.get(&source_id), id_map.get(&target_id)) {
            g.add_edge(s_idx, t_idx, color_base_edge());
        }
    }

    Ok(LoadedGraph { graph: g, nodes })
}

fn main() {
    run_native(
        "Include Graph Visualizer",
        NativeOptions::default(),
        Box::new(|cc| Ok(Box::new(KernelGraphApp::new(cc)))),
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_none_in_a_dag() {
        // 0 -> 1 -> 2 (no cycle): no back edges.
        let forward = vec![vec![1], vec![2], vec![]];
        assert!(find_cycle_edges(&forward).is_empty());
    }

    #[test]
    fn cycles_detect_three_cycle() {
        // 0 -> 1 -> 2 -> 0: DFS closes the cycle on the 2 -> 0 back edge.
        let forward = vec![vec![1], vec![2], vec![0]];
        let back = find_cycle_edges(&forward);
        assert_eq!(back.len(), 1);
        assert!(back.contains(&Edge { source: 2, target: 0 }));
    }

    #[test]
    fn cycles_detect_two_cycle_and_ignore_tail() {
        // 0 <-> 1 (cycle), 1 -> 2 (tail). One back edge closes the cycle; the tail is acyclic.
        let forward = vec![vec![1], vec![0, 2], vec![]];
        let back = find_cycle_edges(&forward);
        assert_eq!(back.len(), 1);
        assert!(back.contains(&Edge { source: 1, target: 0 }));
    }

    #[test]
    fn cycles_detect_disjoint_cycles() {
        // Two independent cycles: {0,1} and {2,3}; one back edge each.
        let forward = vec![vec![1], vec![0], vec![3], vec![2]];
        let back = find_cycle_edges(&forward);
        assert_eq!(back.len(), 2);
        assert!(back.contains(&Edge { source: 1, target: 0 }));
        assert!(back.contains(&Edge { source: 3, target: 2 }));
    }

    #[test]
    fn cycles_detect_self_loop() {
        // A self-include (0 -> 0) is a trivial cycle.
        let forward = vec![vec![0]];
        let back = find_cycle_edges(&forward);
        assert!(back.contains(&Edge { source: 0, target: 0 }));
    }

    #[test]
    fn top_ranking_orders_by_count_then_label_and_drops_zeros() {
        let labels = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        // Values: a = 5, b = 0 (dropped), c = 5 (same as a, label order wins), d = 9.
        let values = vec![5, 0, 5, 9];
        let ranked = top_ranking(&values, &labels, 10, |_| false);
        assert_eq!(
            ranked,
            vec![
                RankEntry { position: 3, count: 9 },
                RankEntry { position: 0, count: 5 },
                RankEntry { position: 2, count: 5 },
            ]
        );
    }

    #[test]
    fn top_ranking_respects_limit() {
        let labels = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let values = vec![1, 2, 3];
        let ranked = top_ranking(&values, &labels, 2, |_| false);
        assert_eq!(
            ranked,
            vec![
                RankEntry { position: 2, count: 3 },
                RankEntry { position: 1, count: 2 },
            ]
        );
    }

    #[test]
    fn top_ranking_excludes_by_predicate() {
        let labels = vec!["vector".to_string(), "engine.h".to_string(), "stdio.h".to_string()];
        let values = vec![100, 3, 50];
        let ranked = top_ranking(&values, &labels, 10, is_standard_header);
        assert_eq!(ranked, vec![RankEntry { position: 1, count: 3 }]);
    }
}
