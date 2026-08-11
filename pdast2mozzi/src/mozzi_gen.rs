//! Mozzi (Arduino) sketch generator from pdast AST — emits a self-contained
//! `.ino` file with no libpd, no runtime patch loading: the whole patch is
//! compiled in, matching the philosophy of `pdast2wclap` (this generator
//! borrows that crate's architecture wholesale — see its module docs for the
//! long version) but retargets codegen to Mozzi's own C++ API instead of a
//! CLAP host ABI.
//!
//! # Design notes
//!
//! - Sub-patches/abstractions are fully flattened (graph-spliced with id
//!   remapping and `$1`/`$2` substitution) before codegen — same approach as
//!   `pdast2wclap::flatten_patch`, copied and adapted here per this
//!   codebase's own convention of copying this logic per-generator rather
//!   than sharing it (see `DEVELOPMENT.md`).
//! - There is no `PdState` struct/pointer here: a Mozzi sketch has exactly
//!   one instance, so every node's persistent value lives in a plain global
//!   variable (`pd_nID`). This is simpler than `pdast2wclap`'s `st->nID`
//!   indirection and still gives every node forward-reference- and
//!   cycle-safe storage for free — a cyclic read just sees last pass's
//!   value.
//! - **Signal domain** (tilde objects): pulled continuously, recomputed once
//!   per `updateAudio()` call (one call = one sample), matching Mozzi's own
//!   execution model almost exactly.
//! - **Control domain**: a genuine message-passing graph (not a recomputed
//!   dataflow expression), for the same reasons `pdast2wclap` uses one — a
//!   recompute pass cannot express "only the outlet that fired propagates"
//!   (route/select), "cold inlets hold their last value" (`[f]`/`[+ 1]`
//!   counters), or right-to-left `trigger` ordering. Each control node gets
//!   a `pd_nX_inK()` inlet handler and `pd_nX_outJ()` outlet fan-out
//!   function; a message delivered to the hot inlet (K=0) computes and
//!   pushes onward depth-first.
//! - Mozzi's `updateControl()` is the natural home for anything that's
//!   driven by *time* rather than by an incoming message — `metro`,
//!   `delay`/`del`, `pipe`, `timer` are implemented with a real
//!   `EventDelay` per node, checked once per control tick (so their timing
//!   resolution is bounded by `MOZZI_CONTROL_RATE`, not sample-accurate —
//!   an intentional, documented trade for staying idiomatic; see README).
//! - MIDI/param integration is a small `pd_*` hook-function surface
//!   (`pd_note_on`, `pd_control_change`, `pd_set_param`, ...), generated
//!   only when the patch actually uses the matching object. Mozzi itself
//!   has no MIDI input, so these are the seam a sketch's own MIDI library
//!   (or potentiometer-reading code) calls into — see README for examples.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use pdast::types::{Canvas, Connection, Node, NodeKind, SubPatchContent, Token};

// ── Flattening (sub-patch inlining with $ substitution) ────────────────────

fn substitute_dollars(args: &[Token], call_args: &[Token]) -> Vec<Token> {
    args.iter()
        .map(|t| match t {
            Token::Dollar(n) => call_args
                .get(*n as usize - 1)
                .cloned()
                .unwrap_or(Token::Float(0.0)),
            Token::DollarZero => Token::Float(0.0),
            other => other.clone(),
        })
        .collect()
}

pub fn flatten_patch(canvas: &Canvas) -> (Vec<Node>, Vec<Connection>) {
    let mut f = FlattenerWithBoundaries::new();
    f.run(canvas, &[]);
    f.resolve_boundaries();
    (f.nodes, f.connections)
}

struct FlattenerWithBoundaries {
    next_id: u32,
    nodes: Vec<Node>,
    connections: Vec<Connection>,
    boundaries: HashMap<u32, (BTreeMap<u32, u32>, BTreeMap<u32, u32>)>,
}

impl FlattenerWithBoundaries {
    fn new() -> Self {
        FlattenerWithBoundaries {
            next_id: 0,
            nodes: Vec::new(),
            connections: Vec::new(),
            boundaries: HashMap::new(),
        }
    }

    fn run(
        &mut self,
        canvas: &Canvas,
        call_args: &[Token],
    ) -> (BTreeMap<u32, u32>, BTreeMap<u32, u32>) {
        let mut id_map: HashMap<u32, u32> = HashMap::new();
        let mut inlet_ids: BTreeMap<u32, u32> = BTreeMap::new();
        let mut outlet_ids: BTreeMap<u32, u32> = BTreeMap::new();

        for node in &canvas.nodes {
            let new_id = self.next_id;
            self.next_id += 1;
            id_map.insert(node.id, new_id);

            match &node.kind {
                NodeKind::Obj { name, args } => {
                    let sub_args = substitute_dollars(args, call_args);
                    if name == "inlet" || name == "inlet~" {
                        inlet_ids.insert(inlet_ids.len() as u32, new_id);
                    }
                    if name == "outlet" || name == "outlet~" {
                        outlet_ids.insert(outlet_ids.len() as u32, new_id);
                    }
                    self.nodes.push(Node {
                        id: new_id,
                        x: node.x,
                        y: node.y,
                        kind: NodeKind::Obj {
                            name: name.clone(),
                            args: sub_args,
                        },
                    });
                }
                NodeKind::SubPatch {
                    content: SubPatchContent::Inline(inner),
                    args,
                    ..
                } => {
                    let sub_args = substitute_dollars(args, call_args);
                    let (in_b, out_b) = self.run(inner, &sub_args);
                    self.boundaries.insert(new_id, (in_b, out_b));
                }
                other => {
                    self.nodes.push(Node {
                        id: new_id,
                        x: node.x,
                        y: node.y,
                        kind: other.clone(),
                    });
                }
            }
        }

        for c in &canvas.connections {
            if let (Some(&src), Some(&dst)) = (id_map.get(&c.src_node), id_map.get(&c.dst_node)) {
                self.connections.push(Connection {
                    src_node: src,
                    src_outlet: c.src_outlet,
                    dst_node: dst,
                    dst_inlet: c.dst_inlet,
                });
            }
        }

        (inlet_ids, outlet_ids)
    }

    fn resolve_boundaries(&mut self) {
        if self.boundaries.is_empty() {
            return;
        }
        for c in &mut self.connections {
            if let Some((_, out_b)) = self.boundaries.get(&c.src_node) {
                if let Some(&real) = out_b.get(&c.src_outlet) {
                    c.src_node = real;
                    c.src_outlet = 0;
                }
            }
            if let Some((in_b, _)) = self.boundaries.get(&c.dst_node) {
                if let Some(&real) = in_b.get(&c.dst_inlet) {
                    c.dst_node = real;
                    c.dst_inlet = 0;
                }
            }
        }
    }
}

// ── Object classification ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Domain {
    Signal,
    Control,
}

fn domain_of(name: &str) -> Domain {
    if name.ends_with('~') {
        Domain::Signal
    } else {
        Domain::Control
    }
}

fn outlet_count(node: &Node) -> usize {
    match &node.kind {
        NodeKind::Obj { name, args } => match name.as_str() {
            "dac~" | "send" | "s" | "delwrite~" | "send~" => 0,
            "notein" => 3,
            "moses" | "swap" => 2,
            "ctlin" => {
                if args.first().is_some_and(|t| matches!(t, Token::Float(_))) {
                    1
                } else {
                    2
                }
            }
            "sel" | "select" | "route" => {
                let n = args.iter().filter(|t| matches!(t, Token::Float(_))).count();
                n.max(1) + 1
            }
            "pack" | "unpack" => args.len().max(2),
            "trigger" | "t" => args.len().max(1),
            _ => 1,
        },
        NodeKind::Gui(_) => 1,
        NodeKind::FloatAtom { .. } | NodeKind::SymbolAtom { .. } | NodeKind::Msg { .. } => 1,
        _ => 1,
    }
}

fn topo_order(node_ids: &[u32], connections: &[Connection]) -> Vec<u32> {
    let id_set: HashSet<u32> = node_ids.iter().copied().collect();
    let mut in_deg: HashMap<u32, usize> = node_ids.iter().map(|&id| (id, 0)).collect();
    let mut adj: HashMap<u32, Vec<u32>> = node_ids.iter().map(|&id| (id, vec![])).collect();

    for c in connections {
        if id_set.contains(&c.src_node) && id_set.contains(&c.dst_node) {
            adj.entry(c.src_node).or_default().push(c.dst_node);
            *in_deg.entry(c.dst_node).or_insert(0) += 1;
        }
    }

    let mut seed: Vec<u32> = in_deg
        .iter()
        .filter(|&(_, &d)| d == 0)
        .map(|(&id, _)| id)
        .collect();
    seed.sort_unstable();
    let mut queue: VecDeque<u32> = seed.into();

    let mut sorted = Vec::new();
    while let Some(node) = queue.pop_front() {
        sorted.push(node);
        let mut nexts: Vec<u32> = adj.get(&node).cloned().unwrap_or_default();
        nexts.sort_unstable();
        for next in nexts {
            let d = in_deg.entry(next).or_insert(1);
            *d = d.saturating_sub(1);
            if *d == 0 {
                queue.push_back(next);
            }
        }
    }
    for &id in node_ids {
        if !sorted.contains(&id) {
            sorted.push(id);
        }
    }
    sorted
}

// ── send/receive/value bus ──────────────────────────────────────────────────

#[derive(Default)]
struct BusEntry {
    senders: Vec<u32>,
    receivers: Vec<u32>,
}

fn bus_name(args: &[Token]) -> Option<String> {
    args.iter().find_map(|t| {
        if let Token::Symbol(s) = t {
            Some(s.clone())
        } else {
            None
        }
    })
}

fn collect_bus_map(nodes: &[Node]) -> BTreeMap<String, BusEntry> {
    let mut map: BTreeMap<String, BusEntry> = BTreeMap::new();
    for node in nodes {
        let NodeKind::Obj { name, args } = &node.kind else {
            continue;
        };
        let Some(bname) = bus_name(args) else {
            continue;
        };
        let entry = map.entry(bname).or_default();
        match name.as_str() {
            "send" | "s" => entry.senders.push(node.id),
            "receive" | "r" | "value" => entry.receivers.push(node.id),
            _ => {}
        }
    }
    map
}

fn collect_signal_bus_map(nodes: &[Node]) -> BTreeMap<String, BusEntry> {
    let mut map: BTreeMap<String, BusEntry> = BTreeMap::new();
    for node in nodes {
        let NodeKind::Obj { name, args } = &node.kind else {
            continue;
        };
        let Some(bname) = bus_name(args) else {
            continue;
        };
        let entry = map.entry(bname).or_default();
        match name.as_str() {
            "send~" => entry.senders.push(node.id),
            "receive~" => entry.receivers.push(node.id),
            _ => {}
        }
    }
    map
}

// ── Param table ──────────────────────────────────────────────────────────────

pub struct ParamInfo {
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub target_ids: Vec<u32>,
}

fn collect_params(nodes: &[Node]) -> Vec<ParamInfo> {
    let mut by_name: BTreeMap<String, ParamInfo> = BTreeMap::new();
    let bus_map = collect_bus_map(nodes);

    for node in nodes {
        match &node.kind {
            NodeKind::Gui(g) => {
                if let Some(name) = &g.receive {
                    let e = by_name.entry(name.clone()).or_insert_with(|| ParamInfo {
                        name: name.clone(),
                        min: g.min,
                        max: g.max,
                        default: g.default_value,
                        target_ids: vec![],
                    });
                    e.target_ids.push(node.id);
                }
            }
            NodeKind::FloatAtom {
                receive: Some(name),
                min,
                max,
                ..
            } => {
                let e = by_name.entry(name.clone()).or_insert_with(|| ParamInfo {
                    name: name.clone(),
                    min: *min,
                    max: *max,
                    default: 0.0,
                    target_ids: vec![],
                });
                e.target_ids.push(node.id);
            }
            NodeKind::Obj { name, args } if matches!(name.as_str(), "receive" | "r" | "value") => {
                let Some(bname) = bus_name(args) else {
                    continue;
                };
                let has_sender = bus_map.get(&bname).is_some_and(|e| !e.senders.is_empty());
                if has_sender {
                    continue;
                }
                let e = by_name.entry(bname.clone()).or_insert_with(|| ParamInfo {
                    name: bname.clone(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    target_ids: vec![],
                });
                e.target_ids.push(node.id);
            }
            _ => {}
        }
    }

    by_name.into_values().collect()
}

// ── Delay lines (delwrite~/delread~/vd~) ────────────────────────────────────
//
// Buffers are keyed by PD name, not node id, so delread~/vd~ can reference a
// same-named delwrite~ regardless of topological order. v1 simplification
// (documented in README, same spirit as pdast2faust/pdast2wclap's own
// documented gap for this exact object pair): the read tap time is fixed to
// the delwrite~'s own `maxms` argument — delread~'s own ms argument and
// vd~'s dynamic modulation input aren't applied.

fn collect_delay_lines(nodes: &[&Node]) -> BTreeMap<String, f64> {
    let mut map: BTreeMap<String, f64> = BTreeMap::new();
    for node in nodes {
        if let NodeKind::Obj { name, args } = &node.kind {
            if name == "delwrite~" {
                let Some(Token::Symbol(dname)) = args.first() else {
                    continue;
                };
                let maxms = args
                    .get(1)
                    .and_then(|t| {
                        if let Token::Float(v) = t {
                            Some(*v)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(1000.0);
                let e = map.entry(dname.clone()).or_insert(0.0);
                if maxms > *e {
                    *e = maxms;
                }
            }
        }
    }
    map
}

/// A compile-time-constant C++ expression for "N ms worth of audio samples",
/// resolved against the real `MOZZI_AUDIO_RATE` macro at *sketch* compile
/// time (not baked in as a number here — we don't know the target board's
/// rate) and clamped to a sane AVR-SRAM-friendly range so a careless
/// `delwrite~ buf 5000` doesn't try to allocate an unreasonable buffer.
fn delay_samples_expr(maxms: f64) -> String {
    format!(
        "((int)((({maxms}) / 1000.0) * MOZZI_AUDIO_RATE) > 2048 ? 2048 : ((int)((({maxms}) / 1000.0) * MOZZI_AUDIO_RATE) < 2 ? 2 : (int)((({maxms}) / 1000.0) * MOZZI_AUDIO_RATE)))"
    )
}

// ── C++ identifier helpers ──────────────────────────────────────────────────

fn field(id: u32) -> String {
    format!("pd_n{id}")
}
fn field_o(id: u32, outlet: u32) -> String {
    if outlet == 0 {
        field(id)
    } else {
        format!("pd_n{id}_o{outlet}")
    }
}
fn inlet_field(id: u32, inlet: u32) -> String {
    format!("pd_n{id}_i{inlet}")
}
fn recv_fn(id: u32, inlet: u32) -> String {
    format!("pd_n{id}_in{inlet}")
}
fn send_fn(id: u32, outlet: u32) -> String {
    format!("pd_n{id}_out{outlet}")
}
fn sanitize_c_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
fn escape_c_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Format an f64 as a valid C++ `float` literal. Plain `{v}f` interpolation
/// breaks for whole numbers (`440f` has no decimal point or exponent, so
/// it's not a valid floating-point-literal per the C++ grammar — compilers
/// parse it as an integer constant with an invalid suffix); this always
/// includes a decimal point.
fn cf(v: f64) -> String {
    if !v.is_finite() {
        return "0.0f".to_string();
    }
    if v.fract() == 0.0 {
        format!("{v:.1}f")
    } else {
        format!("{v}f")
    }
}

// ── Emission result ──────────────────────────────────────────────────────────

struct EmittedNode {
    domain: Domain,
    /// Global variable declarations (persistent state for this node).
    state_fields: String,
    /// `setup()` initializer statements for this node's state.
    init: String,
    /// For Domain::Signal: a statement appended to `updateAudio()` every
    /// call. For Domain::Control: the body of this node's *hot* inlet
    /// handler (fires when a message arrives at inlet 0).
    compute: String,
    /// Statements appended to `updateControl()` unconditionally, every
    /// control tick — only non-empty for time-driven sources
    /// (metro/delay/pipe/timer).
    tick: String,
    /// One-time top-level declarations (Mozzi unit generator instances)
    /// specific to this node, e.g. `Oscil<...> pd_osc_n3(SIN2048_DATA);`.
    globals: String,
}

fn simple_control(id: u32, expr: &str) -> EmittedNode {
    let f = field(id);
    EmittedNode {
        domain: Domain::Control,
        state_fields: format!("float {f};\n"),
        init: format!("{f} = 0.0f;\n"),
        compute: format!("  {}(pd_msg_f({expr}));\n", send_fn(id, 0)),
        tick: String::new(),
        globals: String::new(),
    }
}

fn empty_node(domain: Domain) -> EmittedNode {
    EmittedNode {
        domain,
        state_fields: String::new(),
        init: String::new(),
        compute: String::new(),
        tick: String::new(),
        globals: String::new(),
    }
}

// ── Main generator ────────────────────────────────────────────────────────────

pub struct MozziGenerator {
    pub warnings: Vec<String>,
    node_latch_count: HashMap<u32, u32>,
    cold_aliases_hot: HashSet<u32>,
    declared_delay_lines: HashSet<String>,
    need_noise_fn: bool,
    extra_includes: BTreeSet<String>,
}

impl MozziGenerator {
    pub fn new() -> Self {
        MozziGenerator {
            warnings: Vec::new(),
            node_latch_count: HashMap::new(),
            cold_aliases_hot: HashSet::new(),
            declared_delay_lines: HashSet::new(),
            need_noise_fn: false,
            extra_includes: BTreeSet::new(),
        }
    }

    pub fn generate(&mut self, canvas: &Canvas) -> String {
        self.declared_delay_lines.clear();
        let (nodes, connections) = flatten_patch(canvas);

        let active: Vec<&Node> = nodes
            .iter()
            .filter(|n| {
                !matches!(
                    n.kind,
                    NodeKind::Text { .. } | NodeKind::Array { .. } | NodeKind::Graph { .. }
                )
            })
            .collect();
        let active_ids: Vec<u32> = active.iter().map(|n| n.id).collect();
        let node_by_id: HashMap<u32, &Node> = active.iter().map(|n| (n.id, *n)).collect();

        let bus_map = collect_bus_map(&nodes);
        let signal_bus_map = collect_signal_bus_map(&nodes);
        let params = collect_params(&nodes);

        let order = topo_order(&active_ids, &connections);

        let mut node_outlets: HashMap<u32, usize> = HashMap::new();
        for n in &active {
            node_outlets.insert(n.id, outlet_count(n));
        }

        let mut signal_node_ids: HashSet<u32> = HashSet::new();
        for n in &active {
            if let NodeKind::Obj { name, .. } = &n.kind {
                if domain_of(name) == Domain::Signal {
                    signal_node_ids.insert(n.id);
                }
            }
        }

        let notein_id: Option<u32> = active
            .iter()
            .find(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name == "notein"))
            .map(|n| n.id);
        let ctlin_nodes: Vec<(u32, Option<i32>)> = active
            .iter()
            .filter_map(|n| {
                if let NodeKind::Obj { name, args } = &n.kind {
                    if name == "ctlin" {
                        let filter = args.first().and_then(|t| {
                            if let Token::Float(v) = t {
                                Some(*v as i32)
                            } else {
                                None
                            }
                        });
                        return Some((n.id, filter));
                    }
                }
                None
            })
            .collect();
        let bendin_ids: Vec<u32> = active
            .iter()
            .filter(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name == "bendin"))
            .map(|n| n.id)
            .collect();
        let touchin_ids: Vec<u32> = active
            .iter()
            .filter(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name == "touchin"))
            .map(|n| n.id)
            .collect();
        let pgmin_ids: Vec<u32> = active
            .iter()
            .filter(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name == "pgmin"))
            .map(|n| n.id)
            .collect();
        let has_adc = active
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name == "adc~"));

        let delay_lines = collect_delay_lines(&active);

        let mut state_fields = String::new();
        let mut init_stmts = String::new();
        let mut signal_stmts = String::new();
        let mut tick_stmts = String::new();
        let mut node_globals = String::new();
        let mut hot_bodies: BTreeMap<u32, String> = BTreeMap::new();
        let mut dac_l: Vec<String> = Vec::new();
        let mut dac_r: Vec<String> = Vec::new();

        for &id in &order {
            let Some(&node) = node_by_id.get(&id) else {
                continue;
            };
            let incoming: Vec<&Connection> = {
                let mut v: Vec<&Connection> =
                    connections.iter().filter(|c| c.dst_node == id).collect();
                v.sort_by_key(|c| (c.dst_inlet, c.src_node));
                v
            };

            let emitted = self.emit_node(
                node,
                &incoming,
                &node_outlets,
                &signal_node_ids,
                &bus_map,
                &signal_bus_map,
                &delay_lines,
                &mut dac_l,
                &mut dac_r,
            );

            state_fields.push_str(&emitted.state_fields);
            init_stmts.push_str(&emitted.init);
            node_globals.push_str(&emitted.globals);
            tick_stmts.push_str(&emitted.tick);
            match emitted.domain {
                Domain::Signal => signal_stmts.push_str(&emitted.compute),
                Domain::Control => {
                    hot_bodies.insert(id, emitted.compute);
                }
            }
        }

        let dispatch =
            self.render_dispatch(&hot_bodies, &connections, &node_by_id, &signal_node_ids);

        let mut loadbang_stmts = String::new();
        for n in &active {
            if matches!(&n.kind, NodeKind::Obj{name,..} if name == "loadbang") {
                loadbang_stmts.push_str(&format!("  {}(pd_msg_bang());\n", send_fn(n.id, 0)));
            }
        }

        if has_adc {
            self.warnings.push(
                "adc~ is not implemented (audio input is board-specific) — emitted as a zero stub"
                    .into(),
            );
        }

        render_output(RenderInput {
            state_fields,
            node_globals,
            init_stmts,
            signal_stmts,
            tick_stmts,
            dispatch,
            loadbang_stmts,
            dac_l,
            dac_r,
            params: &params,
            notein_id,
            ctlin_nodes,
            bendin_ids,
            touchin_ids,
            pgmin_ids,
            extra_includes: &self.extra_includes,
            need_noise_fn: self.need_noise_fn,
        })
    }

    /// Emit the message-passing plumbing: one `pd_nX_outJ` fan-out function
    /// per control-node outlet, and one `pd_nX_inK` handler per inlet. See
    /// module docs and `pdast2wclap::render_dispatch` (same idea, no `st`
    /// pointer since there's only ever one instance here).
    fn render_dispatch(
        &self,
        hot_bodies: &BTreeMap<u32, String>,
        connections: &[Connection],
        node_by_id: &HashMap<u32, &Node>,
        signal_node_ids: &HashSet<u32>,
    ) -> String {
        let mut decls = String::new();
        let mut defs = String::new();

        let mut msg_ids: Vec<u32> = self.node_latch_count.keys().copied().collect();
        msg_ids.sort_unstable();

        for &id in &msg_ids {
            let n_in = self.node_latch_count.get(&id).copied().unwrap_or(0).max(1);
            for k in 0..n_in {
                decls.push_str(&format!("static void {}(PdMsg m);\n", recv_fn(id, k)));
            }
            let n_out = node_by_id.get(&id).map(|n| outlet_count(n)).unwrap_or(1);
            for j in 0..n_out {
                decls.push_str(&format!(
                    "static void {}(PdMsg m);\n",
                    send_fn(id, j as u32)
                ));
            }
        }

        for &id in &msg_ids {
            let n_latch = self.node_latch_count.get(&id).copied().unwrap_or(0);
            let n_in = n_latch.max(1);
            let n_out = node_by_id.get(&id).map(|n| outlet_count(n)).unwrap_or(1);

            for j in 0..n_out {
                let j = j as u32;
                defs.push_str(&format!("static void {}(PdMsg m) {{\n", send_fn(id, j)));
                defs.push_str(&format!("  if (m.len >= 1) {} = m.a[0];\n", field_o(id, j)));
                let mut targets: Vec<&Connection> = connections
                    .iter()
                    .filter(|c| c.src_node == id && c.src_outlet == j)
                    .filter(|c| !signal_node_ids.contains(&c.dst_node))
                    .collect();
                if targets.is_empty() {
                    defs.push_str("  (void)m;\n}\n\n");
                    continue;
                }
                targets.sort_by_key(|c| (std::cmp::Reverse(c.dst_node), c.dst_inlet));
                defs.push_str("  if (pd_depth >= PD_MAX_DEPTH) return;\n  pd_depth++;\n");
                for c in targets {
                    let dst_in = self
                        .node_latch_count
                        .get(&c.dst_node)
                        .copied()
                        .unwrap_or(0)
                        .max(1);
                    let k = c.dst_inlet.min(dst_in - 1);
                    defs.push_str(&format!("  {}(m);\n", recv_fn(c.dst_node, k)));
                }
                defs.push_str("  pd_depth--;\n}\n\n");
            }

            for k in 0..n_in {
                defs.push_str(&format!("static void {}(PdMsg m) {{\n", recv_fn(id, k)));
                if k == 0 {
                    if n_latch > 1 {
                        defs.push_str(&format!(
                            "  for (int _i = 1; _i < m.len && _i < {n_latch}; _i++) {{\n    switch (_i) {{\n"
                        ));
                        for ci in 1..n_latch {
                            defs.push_str(&format!(
                                "      case {ci}: {} = m.a[_i]; break;\n",
                                inlet_field(id, ci)
                            ));
                        }
                        defs.push_str("      default: break;\n    }\n  }\n");
                    }
                    if n_latch > 0 {
                        defs.push_str(&format!(
                            "  if (m.len >= 1) {} = m.a[0];\n",
                            inlet_field(id, 0)
                        ));
                    }
                    let body = hot_bodies.get(&id).map(String::as_str).unwrap_or("");
                    if body.trim().is_empty() {
                        defs.push_str("  (void)m;\n");
                    } else {
                        defs.push_str(body);
                    }
                } else {
                    let target = if self.cold_aliases_hot.contains(&id) {
                        0
                    } else {
                        k
                    };
                    defs.push_str(&format!(
                        "  if (m.len >= 1) {} = m.a[0];\n",
                        inlet_field(id, target)
                    ));
                }
                defs.push_str("}\n\n");
            }
        }

        format!("{decls}\n{defs}")
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_node(
        &mut self,
        node: &Node,
        incoming: &[&Connection],
        node_outlets: &HashMap<u32, usize>,
        signal_node_ids: &HashSet<u32>,
        bus_map: &BTreeMap<String, BusEntry>,
        signal_bus_map: &BTreeMap<String, BusEntry>,
        delay_lines: &BTreeMap<String, f64>,
        dac_l: &mut Vec<String>,
        dac_r: &mut Vec<String>,
    ) -> EmittedNode {
        let id = node.id;

        let is_msg_node = match &node.kind {
            NodeKind::Obj { name, .. } => domain_of(name) == Domain::Control,
            _ => true,
        };

        let latch_defaults: RefCell<BTreeMap<u32, String>> = RefCell::new(BTreeMap::new());

        let input_expr = |inlet: u32, default_lit: &str| -> String {
            if is_msg_node {
                latch_defaults
                    .borrow_mut()
                    .entry(inlet)
                    .or_insert_with(|| default_lit.to_string());
                return inlet_field(id, inlet);
            }
            let mut matches: Vec<&&Connection> =
                incoming.iter().filter(|c| c.dst_inlet == inlet).collect();
            if matches.is_empty() {
                return default_lit.to_string();
            }
            let field_of = |c: &Connection| -> String {
                let n_out = node_outlets.get(&c.src_node).copied().unwrap_or(1) as u32;
                field_o(c.src_node, if n_out > 1 { c.src_outlet } else { 0 })
            };
            if matches.len() == 1 || !signal_node_ids.contains(&matches[0].src_node) {
                return field_of(matches[0]);
            }
            matches.sort_by_key(|c| c.src_node);
            let terms: Vec<String> = matches.iter().map(|c| field_of(c)).collect();
            format!("({})", terms.join(" + "))
        };

        let mut emitted = match &node.kind {
            NodeKind::Gui(g) => {
                let f = field(id);
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\n"),
                    init: format!("{f} = {};\n", g.default_value),
                    compute: String::new(),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            NodeKind::FloatAtom { .. } | NodeKind::SymbolAtom { .. } => {
                let f = field(id);
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\n"),
                    init: format!("{f} = 0.0f;\n"),
                    compute: String::new(),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            NodeKind::Msg { messages } => {
                let atoms: Vec<f64> = messages
                    .first()
                    .map(|m| {
                        m.iter()
                            .map(|t| if let Token::Float(v) = t { *v } else { 0.0 })
                            .collect()
                    })
                    .unwrap_or_default();
                let f = field(id);
                let first = atoms.first().copied().unwrap_or(0.0);
                let mut body = format!("  PdMsg _o;\n  _o.len = {};\n", atoms.len().min(8));
                for (i, v) in atoms.iter().take(8).enumerate() {
                    body.push_str(&format!("  _o.a[{i}] = {};\n", cf(*v)));
                }
                if atoms.is_empty() {
                    body = "  PdMsg _o = pd_msg_bang();\n".to_string();
                }
                body.push_str(&format!("  {}(_o);\n", send_fn(id, 0)));
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\n"),
                    init: format!("{f} = {};\n", cf(first)),
                    compute: body,
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            NodeKind::Obj { name, args } => self.emit_obj(
                name,
                args,
                id,
                incoming,
                bus_map,
                signal_bus_map,
                delay_lines,
                &input_expr,
                dac_l,
                dac_r,
            ),
            _ => empty_node(Domain::Control),
        };

        let latches = latch_defaults.borrow();
        for (inlet, default) in latches.iter() {
            let lf = inlet_field(id, *inlet);
            emitted.state_fields.push_str(&format!("float {lf};\n"));
            emitted.init.push_str(&format!("{lf} = {default};\n"));
        }
        if is_msg_node {
            self.node_latch_count
                .insert(id, latches.keys().max().map(|m| m + 1).unwrap_or(0));
        }
        drop(latches);
        emitted
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_obj(
        &mut self,
        name: &str,
        args: &[Token],
        id: u32,
        incoming: &[&Connection],
        bus_map: &BTreeMap<String, BusEntry>,
        signal_bus_map: &BTreeMap<String, BusEntry>,
        delay_lines: &BTreeMap<String, f64>,
        input_expr: &dyn Fn(u32, &str) -> String,
        dac_l: &mut Vec<String>,
        dac_r: &mut Vec<String>,
    ) -> EmittedNode {
        let f = field(id);
        let farg = |i: usize| -> f64 {
            args.get(i)
                .and_then(|t| {
                    if let Token::Float(v) = t {
                        Some(*v)
                    } else {
                        None
                    }
                })
                .unwrap_or(0.0)
        };
        let fargf = |i: usize| -> String { format!("{}", farg(i)) };

        match name {
            // ── send / receive / value (control bus) ────────────────────────
            "send" | "s" => {
                let receivers: Vec<u32> = bus_name(args)
                    .and_then(|b| bus_map.get(&b))
                    .map(|e| e.receivers.clone())
                    .unwrap_or_default();
                let _ = input_expr(0, "0.0f");
                let compute: String = receivers
                    .iter()
                    .map(|r| format!("  {}(m);\n", send_fn(*r, 0)))
                    .collect();
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\n"),
                    init: format!("{f} = 0.0f;\n"),
                    compute,
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "receive" | "r" | "value" => {
                if bus_name(args).is_none() {
                    return empty_node(Domain::Control);
                }
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\n"),
                    init: format!("{f} = 0.0f;\n"),
                    compute: String::new(),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "send~" => {
                let has_receiver = bus_name(args)
                    .and_then(|b| signal_bus_map.get(&b))
                    .is_some_and(|e| !e.receivers.is_empty());
                let in0 = input_expr(0, "0");
                let compute = if has_receiver {
                    format!("  {f} = {in0};\n")
                } else {
                    String::new()
                };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute,
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "receive~" => {
                if bus_name(args).is_none() {
                    return empty_node(Domain::Signal);
                }
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute: String::new(),
                    tick: String::new(),
                    globals: String::new(),
                }
            }

            // ── sub-patch boundary (resolved by flattening; passthrough) ────
            "inlet" | "outlet" => {
                let _ = input_expr(0, "0.0f");
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\n"),
                    init: format!("{f} = 0.0f;\n"),
                    compute: format!("  {}(m);\n", send_fn(id, 0)),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "inlet~" | "outlet~" => {
                let a = input_expr(0, "0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute: format!("  {f} = {a};\n"),
                    tick: String::new(),
                    globals: String::new(),
                }
            }

            // ── control math ─────────────────────────────────────────────────
            "+" | "-" | "*" | "/" | "max" | "min" | "mod" | "pow" => {
                let a = input_expr(0, "0.0f");
                let b = input_expr(1, &cf(if args.is_empty() { 0.0 } else { farg(0) }));
                let op = match name {
                    "+" => format!("({a}) + ({b})"),
                    "-" => format!("({a}) - ({b})"),
                    "*" => format!("({a}) * ({b})"),
                    "/" => format!("(({b}) != 0.0f ? ({a}) / ({b}) : 0.0f)"),
                    "max" => format!("(({a}) > ({b}) ? ({a}) : ({b}))"),
                    "min" => format!("(({a}) < ({b}) ? ({a}) : ({b}))"),
                    "mod" => format!("fmodf({a}, ({b}) != 0.0f ? ({b}) : 1.0f)"),
                    "pow" => format!("powf({a}, {b})"),
                    _ => unreachable!(),
                };
                simple_control(id, &op)
            }
            "mtof" => {
                self.extra_includes.insert("#include <mozzi_midi.h>".into());
                let a = input_expr(0, "60.0f");
                simple_control(id, &format!("mtof((float)({a}))"))
            }
            "ftom" => {
                self.extra_includes.insert("#include <mozzi_midi.h>".into());
                let a = input_expr(0, "440.0f");
                simple_control(id, &format!("ftom((float)({a}))"))
            }
            "sin" => {
                let a = input_expr(0, "0.0f");
                simple_control(id, &format!("sinf(({a}) * 6.283185307f)"))
            }
            "cos" => {
                let a = input_expr(0, "0.0f");
                simple_control(id, &format!("cosf(({a}) * 6.283185307f)"))
            }
            "atan" => {
                let a = input_expr(0, "0.0f");
                simple_control(id, &format!("atanf({a})"))
            }
            "atan2" => {
                let a = input_expr(0, "0.0f");
                let b = input_expr(1, "0.0f");
                simple_control(id, &format!("atan2f({a}, {b})"))
            }
            "abs" => {
                let a = input_expr(0, "0.0f");
                simple_control(id, &format!("fabsf({a})"))
            }
            "sqrt" => {
                let a = input_expr(0, "0.0f");
                simple_control(id, &format!("(({a}) > 0.0f ? sqrtf({a}) : 0.0f)"))
            }
            "log" => {
                let a = input_expr(0, "0.0f");
                simple_control(id, &format!("(({a}) > 0.0f ? logf({a}) : 0.0f)"))
            }
            "exp" => {
                let a = input_expr(0, "0.0f");
                simple_control(id, &format!("expf({a})"))
            }
            "wrap" => {
                let a = input_expr(0, "0.0f");
                simple_control(id, &format!("(({a}) - floorf({a}))"))
            }
            "clip" => {
                let a = input_expr(0, "0.0f");
                let lo = input_expr(1, &cf(if args.is_empty() { 0.0 } else { farg(0) }));
                let hi = input_expr(2, &cf(if args.len() < 2 { 1.0 } else { farg(1) }));
                simple_control(
                    id,
                    &format!("(({a}) < ({lo}) ? ({lo}) : (({a}) > ({hi}) ? ({hi}) : ({a})))"),
                )
            }
            "int" | "i" => {
                let a = input_expr(0, "0.0f");
                simple_control(id, &format!("(float)(int32_t)({a})"))
            }
            "f" | "float" => {
                let default = cf(if args.is_empty() { 0.0 } else { farg(0) });
                let a = input_expr(0, &default);
                let _ = input_expr(1, &default);
                self.cold_aliases_hot.insert(id);
                simple_control(id, &a)
            }
            ">" | "<" | ">=" | "<=" | "==" | "!=" | "&&" | "||" => {
                let a = input_expr(0, "0.0f");
                let b = input_expr(1, &cf(if args.is_empty() { 0.0 } else { farg(0) }));
                let op = name;
                simple_control(id, &format!("(({a}) {op} ({b}) ? 1.0f : 0.0f)"))
            }
            "!" => {
                let a = input_expr(0, "0.0f");
                simple_control(id, &format!("(({a}) == 0.0f ? 1.0f : 0.0f)"))
            }
            "dbtorms" => {
                let a = input_expr(0, "0.0f");
                simple_control(
                    id,
                    &format!("(({a}) > 0.0f ? powf(10.0f, (({a}) - 100.0f) / 20.0f) : 0.0f)"),
                )
            }
            "rmstodb" => {
                let a = input_expr(0, "0.0f");
                simple_control(
                    id,
                    &format!("(({a}) > 0.0f ? 100.0f + 20.0f * log10f({a}) : 0.0f)"),
                )
            }
            "dbtopow" => {
                let a = input_expr(0, "0.0f");
                simple_control(
                    id,
                    &format!("(({a}) > 0.0f ? powf(10.0f, (({a}) - 100.0f) / 10.0f) : 0.0f)"),
                )
            }
            "powtodb" => {
                let a = input_expr(0, "0.0f");
                simple_control(
                    id,
                    &format!("(({a}) > 0.0f ? 100.0f + 10.0f * log10f({a}) : 0.0f)"),
                )
            }

            // ── routing ───────────────────────────────────────────────────────
            "moses" => {
                let a = input_expr(0, "0.0f");
                let thresh = input_expr(1, &fargf(0));
                let o0 = field(id);
                let o1 = field_o(id, 1);
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {o0};\nfloat {o1};\n"),
                    init: format!("{o0} = 0.0f; {o1} = 0.0f;\n"),
                    compute: format!(
                        "  {{ float _v = {a}, _t = {thresh};\n    if (_v < _t) {lo}(pd_msg_f(_v)); else {hi}(pd_msg_f(_v));\n  }}\n",
                        lo = send_fn(id, 0),
                        hi = send_fn(id, 1)
                    ),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "spigot" => {
                let _ = input_expr(0, "0.0f");
                let gate = input_expr(1, &fargf(0));
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\n"),
                    init: format!("{f} = 0.0f;\n"),
                    compute: format!("  if (({gate}) != 0.0f) {}(m);\n", send_fn(id, 0)),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "sel" | "select" => {
                let a = input_expr(0, "0.0f");
                let targets: Vec<f64> = if args.is_empty() {
                    vec![0.0]
                } else {
                    args.iter()
                        .filter_map(|t| {
                            if let Token::Float(v) = t {
                                Some(*v)
                            } else {
                                None
                            }
                        })
                        .collect()
                };
                let mut compute = format!("  float _v = {a};\n");
                let mut state_fields = String::new();
                let mut init = String::new();
                for (i, t) in targets.iter().enumerate() {
                    let fld = field_o(id, i as u32);
                    state_fields.push_str(&format!("float {fld};\n"));
                    init.push_str(&format!("{fld} = 0.0f;\n"));
                    compute.push_str(&format!(
                        "  if (_v == ({})) {{ {}(pd_msg_bang()); return; }}\n",
                        cf(*t),
                        send_fn(id, i as u32)
                    ));
                }
                let pass_fld = field_o(id, targets.len() as u32);
                state_fields.push_str(&format!("float {pass_fld};\n"));
                init.push_str(&format!("{pass_fld} = 0.0f;\n"));
                compute.push_str(&format!(
                    "  {}(pd_msg_f(_v));\n",
                    send_fn(id, targets.len() as u32)
                ));
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "change" => {
                let a = input_expr(0, "0.0f");
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\nfloat {f}_prev;\nint {f}_seen;\n"),
                    init: format!("{f} = 0.0f; {f}_prev = 0.0f; {f}_seen = 0;\n"),
                    compute: format!(
                        "  {{ float _v = {a};\n    if (!{f}_seen || _v != {f}_prev) {{ {f}_seen = 1; {f}_prev = _v; {}(pd_msg_f(_v)); }}\n  }}\n",
                        send_fn(id, 0)
                    ),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "route" => {
                let targets: Vec<f64> = args
                    .iter()
                    .filter_map(|t| {
                        if let Token::Float(v) = t {
                            Some(*v)
                        } else {
                            None
                        }
                    })
                    .collect();
                let targets = if targets.is_empty() {
                    vec![0.0]
                } else {
                    targets
                };
                let mut compute = String::from(
                    "  PdMsg _rest;\n  _rest.len = m.len > 0 ? m.len - 1 : 0;\n  for (int _i = 0; _i < _rest.len && _i + 1 < PD_MSG_MAX; _i++) _rest.a[_i] = m.a[_i + 1];\n  if (_rest.len == 0) _rest = pd_msg_bang();\n  float _sel = m.len > 0 ? m.a[0] : 0.0f;\n",
                );
                let mut state_fields = String::new();
                let mut init = String::new();
                for (i, t) in targets.iter().enumerate() {
                    let fld = field_o(id, i as u32);
                    state_fields.push_str(&format!("float {fld};\n"));
                    init.push_str(&format!("{fld} = 0.0f;\n"));
                    compute.push_str(&format!(
                        "  if (_sel == ({})) {{ {}(_rest); return; }}\n",
                        cf(*t),
                        send_fn(id, i as u32)
                    ));
                }
                let pass_fld = field_o(id, targets.len() as u32);
                state_fields.push_str(&format!("float {pass_fld};\n"));
                init.push_str(&format!("{pass_fld} = 0.0f;\n"));
                compute.push_str(&format!("  {}(m);\n", send_fn(id, targets.len() as u32)));
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "trigger" | "t" => {
                let a = input_expr(0, "0.0f");
                let n = args.len().max(1);
                let mut state_fields = String::new();
                let mut init = String::new();
                let mut compute = format!("  float _v = {a};\n");
                for i in 0..n {
                    let fld = field_o(id, i as u32);
                    state_fields.push_str(&format!("float {fld};\n"));
                    init.push_str(&format!("{fld} = 0.0f;\n"));
                }
                for i in (0..n).rev() {
                    let is_bang =
                        matches!(args.get(i), Some(Token::Symbol(sy)) if sy == "b" || sy == "bang");
                    let msg = if is_bang {
                        "pd_msg_bang()".to_string()
                    } else {
                        "pd_msg_f(_v)".to_string()
                    };
                    compute.push_str(&format!("  {}({msg});\n", send_fn(id, i as u32)));
                }
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "line" => {
                let target = input_expr(0, "0.0f");
                let ramp_ms = cf(if args.is_empty() { 20.0 } else { farg(0) });
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\n"),
                    init: format!("{f} = 0.0f;\n"),
                    compute: format!(
                        "  {{ float _c = 1.0f - expf(-1000.0f / (({ramp_ms}) > 0.001f ? ({ramp_ms}) : 0.001f) / MOZZI_CONTROL_RATE); if (_c > 1.0f) _c = 1.0f; {f} += _c * (({target}) - {f}); }}\n"
                    ),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "random" => {
                let range = cf(if args.is_empty() { 1.0 } else { farg(0) });
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("uint32_t {f}_seed;\nfloat {f};\n"),
                    init: format!("{f}_seed = 12345u + {id}u; {f} = 0.0f;\n"),
                    compute: format!(
                        "  {{ {f}_seed = {f}_seed * 1103515245u + 12345u; float _r = (float)(({f}_seed >> 8) & 0x7fffffu) / (float)0x800000; {}(pd_msg_f(floorf(_r * ({range})))); }}\n",
                        send_fn(id, 0)
                    ),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "pack" => {
                let n = args.len().max(2);
                let mut state_fields = String::new();
                let mut init = String::new();
                let mut compute = String::new();
                for i in 0..n {
                    let fld = field_o(id, i as u32);
                    let default = cf(args
                        .get(i)
                        .and_then(|t| {
                            if let Token::Float(v) = t {
                                Some(*v)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0.0));
                    let v = input_expr(i as u32, &default);
                    state_fields.push_str(&format!("float {fld};\n"));
                    init.push_str(&format!("{fld} = 0.0f;\n"));
                    compute.push_str(&format!("  _o.a[{i}] = {v};\n"));
                }
                let compute = format!(
                    "  PdMsg _o;\n  _o.len = {n};\n{compute}  {}(_o);\n",
                    send_fn(id, 0)
                );
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "unpack" => {
                let n = args.len().max(2);
                for i in 0..n {
                    let _ = input_expr(i as u32, "0.0f");
                }
                let mut state_fields = String::new();
                let mut init = String::new();
                let mut compute = String::new();
                for i in 0..n {
                    let fld = field_o(id, i as u32);
                    state_fields.push_str(&format!("float {fld};\n"));
                    init.push_str(&format!("{fld} = 0.0f;\n"));
                }
                for i in (0..n).rev() {
                    compute.push_str(&format!(
                        "  {}(pd_msg_f({}));\n",
                        send_fn(id, i as u32),
                        inlet_field(id, i as u32)
                    ));
                }
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "swap" => {
                let a = input_expr(0, &fargf(0));
                let b = input_expr(1, &fargf(1));
                let o0 = field(id);
                let o1 = field_o(id, 1);
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {o0};\nfloat {o1};\n"),
                    init: format!("{o0} = 0.0f; {o1} = 0.0f;\n"),
                    compute: format!(
                        "  {{ float _a = {a}, _b = {b};\n    {o1f}(pd_msg_f(_a));\n    {o0f}(pd_msg_f(_b));\n  }}\n",
                        o1f = send_fn(id, 1),
                        o0f = send_fn(id, 0)
                    ),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "loadbang" => EmittedNode {
                domain: Domain::Control,
                state_fields: format!("float {f};\n"),
                init: format!("{f} = 0.0f;\n"),
                compute: String::new(),
                tick: String::new(),
                globals: String::new(),
            },

            // ── scheduler: metro / delay / pipe / timer ──────────────────────
            // Real Mozzi `EventDelay` per node, checked once per control tick
            // (see module docs: resolution is bounded by MOZZI_CONTROL_RATE,
            // not sample-accurate — an intentional trade for staying
            // idiomatic Mozzi rather than hand-rolling sample counters).
            "metro" => {
                self.extra_includes.insert("#include <EventDelay.h>".into());
                let period_ms = (if args.is_empty() { 200.0 } else { farg(0) }).max(1.0) as u32;
                let ed = format!("pd_ed_n{id}");
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\nbool {f}_running;\n"),
                    init: format!("{f} = 0.0f; {f}_running = false; {ed}.set({period_ms}u);\n"),
                    compute: format!(
                        "  if (m.len == 0 || m.a[0] != 0.0f) {{ if (!{f}_running) {{ {ed}.start(); {f}_running = true; }} }} else {{ {f}_running = false; }}\n"
                    ),
                    tick: format!(
                        "  if ({f}_running && {ed}.ready()) {{ {ed}.start(); {}(pd_msg_bang()); }}\n",
                        send_fn(id, 0)
                    ),
                    globals: format!("EventDelay {ed};\n"),
                }
            }
            "delay" | "del" => {
                self.extra_includes.insert("#include <EventDelay.h>".into());
                let delay_ms = (if args.is_empty() { 0.0 } else { farg(0) }).max(0.0) as u32;
                let ed = format!("pd_ed_n{id}");
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\nbool {f}_armed;\n"),
                    init: format!("{f} = 0.0f; {f}_armed = false; {ed}.set({delay_ms}u);\n"),
                    compute: format!("  {ed}.start(); {f}_armed = true;\n"),
                    tick: format!(
                        "  if ({f}_armed && {ed}.ready()) {{ {f}_armed = false; {}(pd_msg_bang()); }}\n",
                        send_fn(id, 0)
                    ),
                    globals: format!("EventDelay {ed};\n"),
                }
            }
            "pipe" => {
                self.extra_includes.insert("#include <EventDelay.h>".into());
                let delay_ms = (if args.is_empty() { 0.0 } else { farg(0) }).max(0.0) as u32;
                let ed = format!("pd_ed_n{id}");
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("float {f};\nfloat {f}_val;\nbool {f}_armed;\n"),
                    init: format!(
                        "{f} = 0.0f; {f}_val = 0.0f; {f}_armed = false; {ed}.set({delay_ms}u);\n"
                    ),
                    compute: format!(
                        "  {f}_val = m.len > 0 ? m.a[0] : 0.0f; {ed}.start(); {f}_armed = true;\n"
                    ),
                    tick: format!(
                        "  if ({f}_armed && {ed}.ready()) {{ {f}_armed = false; {}(pd_msg_f({f}_val)); }}\n",
                        send_fn(id, 0)
                    ),
                    globals: format!("EventDelay {ed};\n"),
                }
            }
            "timer" => EmittedNode {
                domain: Domain::Control,
                state_fields: format!("float {f};\nuint32_t {f}_ticks;\n"),
                init: format!("{f} = 0.0f; {f}_ticks = 0;\n"),
                compute: format!("  {}(pd_msg_f({f})); {f}_ticks = 0;\n", send_fn(id, 0)),
                tick: format!(
                    "  {f}_ticks++; {f} = (float){f}_ticks * (1000.0f / MOZZI_CONTROL_RATE);\n"
                ),
                globals: String::new(),
            },

            // ── MIDI sources (real events — see pd_note_on/pd_control_change
            //    etc. in the generated hook-function surface; a sketch's own
            //    MIDI library calls these, Mozzi itself has no MIDI input) ──
            "notein" | "ctlin" | "bendin" | "touchin" | "pgmin" => {
                let n_out = outlet_count_for(name, args);
                let mut state_fields = String::new();
                let mut init = String::new();
                for j in 0..n_out {
                    let fld = field_o(id, j as u32);
                    state_fields.push_str(&format!("float {fld};\n"));
                    init.push_str(&format!("{fld} = 0.0f;\n"));
                }
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute: String::new(),
                    tick: String::new(),
                    globals: String::new(),
                }
            }

            // ── oscillators / signal sources ─────────────────────────────────
            "osc~" => {
                self.extra_includes.insert("#include <Oscil.h>".into());
                self.extra_includes
                    .insert("#include <tables/sin2048_int8.h>".into());
                let freq = input_expr(0, &cf(if args.is_empty() { 0.0 } else { farg(0) }));
                let osc = format!("pd_osc_n{id}");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute: format!("  {osc}.setFreq((float)({freq})); {f} = {osc}.next();\n"),
                    tick: String::new(),
                    globals: format!(
                        "Oscil<SIN2048_NUM_CELLS, MOZZI_AUDIO_RATE> {osc}(SIN2048_DATA);\n"
                    ),
                }
            }
            "phasor~" => {
                // Unlike osc~/noise~, real PD's phasor~ is a *unipolar* 0..1
                // ramp (not an audio-amplitude signal), and idioms like
                // `[phasor~ 5] -> [*~ 20] -> [+~ 440]` (an LFO sweep) depend
                // on that range — so this stays a plain 0.0..~1.0 float
                // field rather than following the ±128 table-amplitude
                // convention the other audio-rate objects use. See README.
                let freq = input_expr(0, &cf(if args.is_empty() { 0.0 } else { farg(0) }));
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("float {f};\nuint32_t {f}_phase;\n"),
                    init: format!("{f} = 0.0f; {f}_phase = 0;\n"),
                    compute: format!(
                        "  {{ float _fr = (float)({freq}); uint32_t _inc = (uint32_t)((_fr / (float)MOZZI_AUDIO_RATE) * 4294967296.0f); {f}_phase += _inc; {f} = (float){f}_phase / 4294967296.0f; }}\n"
                    ),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "noise~" => {
                self.need_noise_fn = true;
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute: format!("  {f} = pd_noise_next();\n"),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "sig~" => {
                let a = input_expr(0, &cf(if args.is_empty() { 0.0 } else { farg(0) }));
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute: format!("  {f} = (int32_t)({a});\n"),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "+~" | "-~" | "*~" | "/~" => {
                let a = input_expr(0, "0");
                let b = input_expr(1, &cf(if args.is_empty() { 0.0 } else { farg(0) }));
                let op = match name {
                    "+~" => format!("((float)({a})) + ((float)({b}))"),
                    "-~" => format!("((float)({a})) - ((float)({b}))"),
                    "*~" => format!("((float)({a})) * ((float)({b}))"),
                    "/~" => {
                        format!("(((float)({b})) != 0.0f ? ((float)({a})) / ((float)({b})) : 0.0f)")
                    }
                    _ => unreachable!(),
                };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute: format!("  {f} = (int32_t)({op});\n"),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "abs~" => {
                let a = input_expr(0, "0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute: format!(
                        "  {{ int32_t _a = (int32_t)({a}); {f} = _a < 0 ? -_a : _a; }}\n"
                    ),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "clip~" => {
                // Audio-rate values here follow Mozzi's native table-amplitude
                // convention (roughly -128..127 per "unit"), not PD's
                // normalized -1..1 — so literal -1..1-style creation args are
                // rescaled by 128 to match. See README.
                let a = input_expr(0, "0");
                let lo = input_expr(1, &cf(if args.is_empty() { -1.0 } else { farg(0) }));
                let hi = input_expr(2, &cf(if args.len() < 2 { 1.0 } else { farg(1) }));
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute: format!(
                        "  {{ int32_t _a = (int32_t)({a}); int32_t _lo = (int32_t)(({lo}) * 128.0f); int32_t _hi = (int32_t)(({hi}) * 128.0f); {f} = constrain(_a, _lo, _hi); }}\n"
                    ),
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "lop~" | "hip~" => {
                let a = input_expr(0, "0");
                let fc = input_expr(1, &cf(if args.is_empty() { 1000.0 } else { farg(0) }));
                let lp_state = format!("{f}_lp");
                let body = if name == "lop~" {
                    format!(
                        "  {{ float _fc = (float)({fc}); float _c = 1.0f - expf(-6.2831853f * _fc / (float)MOZZI_AUDIO_RATE); if (_c < 0.0f) _c = 0.0f; if (_c > 1.0f) _c = 1.0f; {lp_state} += _c * (((float)({a})) - {lp_state}); {f} = (int32_t){lp_state}; }}\n"
                    )
                } else {
                    format!(
                        "  {{ float _fc = (float)({fc}); float _c = 1.0f - expf(-6.2831853f * _fc / (float)MOZZI_AUDIO_RATE); if (_c < 0.0f) _c = 0.0f; if (_c > 1.0f) _c = 1.0f; {lp_state} += _c * (((float)({a})) - {lp_state}); {f} = (int32_t)(((float)({a})) - {lp_state}); }}\n"
                    )
                };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\nfloat {lp_state};\n"),
                    init: format!("{f} = 0; {lp_state} = 0.0f;\n"),
                    compute: body,
                    tick: String::new(),
                    globals: String::new(),
                }
            }
            "line~" | "vline~" => {
                self.extra_includes.insert("#include <Line.h>".into());
                let target = input_expr(0, "0");
                let ramp_ms = cf(if args.is_empty() { 20.0 } else { farg(0) });
                let ln = format!("pd_ln_n{id}");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!(
                        "int32_t {f};\nint32_t {f}_prevtarget;\nbool {f}_first;\n"
                    ),
                    init: format!("{f} = 0; {f}_prevtarget = 0; {f}_first = true;\n"),
                    compute: format!(
                        "  {{ int32_t _t = (int32_t)({target}); if ({f}_first || _t != {f}_prevtarget) {{ int32_t _steps = (int32_t)((({ramp_ms}) / 1000.0f) * MOZZI_AUDIO_RATE); if (_steps < 1) _steps = 1; {ln}.set({f}, _t, _steps); {f}_prevtarget = _t; {f}_first = false; }} {f} = {ln}.next(); }}\n"
                    ),
                    tick: String::new(),
                    globals: format!("Line<int32_t> {ln};\n"),
                }
            }

            // ── delay lines (shared buffer keyed by name) ────────────────────
            "delwrite~" => {
                self.extra_includes
                    .insert("#include <AudioDelayFeedback.h>".into());
                let Some(Token::Symbol(dname)) = args.first() else {
                    self.warnings
                        .push("delwrite~ needs a name argument — emitted as a zero stub".into());
                    return empty_node(Domain::Signal);
                };
                let c = sanitize_c_name(dname);
                let inp = input_expr(0, "0");
                let first_time = self.declared_delay_lines.insert(dname.clone());
                let maxms = delay_lines.get(dname).copied().unwrap_or(1000.0);
                let (state_fields, init, globals) = if first_time {
                    (
                        format!("int32_t pd_dl_{c};\n"),
                        format!("pd_dl_{c} = 0; pd_dl_n{id}.setFeedbackLevel(0);\n"),
                        format!(
                            "AudioDelayFeedback<{}> pd_dl_n{id};\n",
                            delay_samples_expr(maxms)
                        ),
                    )
                } else {
                    (String::new(), String::new(), String::new())
                };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields,
                    init,
                    compute: format!(
                        "  pd_dl_{c} = pd_dl_n{id}.next((int)({inp}), {});\n",
                        delay_samples_expr(maxms)
                    ),
                    tick: String::new(),
                    globals,
                }
            }
            "delread~" | "vd~" => {
                let Some(Token::Symbol(dname)) = args.first() else {
                    self.warnings.push(format!(
                        "{name} needs a name argument — emitted as a zero stub"
                    ));
                    return empty_node(Domain::Signal);
                };
                if !delay_lines.contains_key(dname) {
                    self.warnings.push(format!(
                        "{name} {dname}: no matching delwrite~ found — emitted as a zero stub"
                    ));
                    return empty_node(Domain::Signal);
                }
                let c = sanitize_c_name(dname);
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute: format!("  {f} = pd_dl_{c};\n"),
                    tick: String::new(),
                    globals: String::new(),
                }
            }

            // ── audio I/O ─────────────────────────────────────────────────────
            "dac~" => {
                let l = input_expr(0, "0");
                dac_l.push(l);
                if incoming.iter().any(|c| c.dst_inlet == 1) {
                    let r = input_expr(1, "0");
                    dac_r.push(r);
                }
                empty_node(Domain::Signal)
            }
            "adc~" => {
                self.warnings
                    .push("adc~ is not implemented — emitted as a zero stub".into());
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("int32_t {f};\n"),
                    init: format!("{f} = 0;\n"),
                    compute: String::new(),
                    tick: String::new(),
                    globals: String::new(),
                }
            }

            // ── unsupported: stub + warning ──────────────────────────────────
            _ => {
                self.warnings.push(format!(
                    "object '{name}' is not supported — emitted as a stub"
                ));
                let domain = domain_of(name);
                match domain {
                    Domain::Signal => EmittedNode {
                        domain,
                        state_fields: format!("int32_t {f};\n"),
                        init: format!("{f} = 0;\n"),
                        compute: String::new(),
                        tick: String::new(),
                        globals: String::new(),
                    },
                    Domain::Control => EmittedNode {
                        domain,
                        state_fields: format!("float {f};\n"),
                        init: format!("{f} = 0.0f;\n"),
                        compute: String::new(),
                        tick: String::new(),
                        globals: String::new(),
                    },
                }
            }
        }
    }
}

impl Default for MozziGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Outlet count for the MIDI source objects — mirrors `outlet_count()`'s
/// logic for `notein`/`ctlin`, extracted so `emit_obj` (which only has the
/// node's name/args, not the full `Node`) can reuse it.
fn outlet_count_for(name: &str, args: &[Token]) -> usize {
    match name {
        "notein" => 3,
        "ctlin" => {
            if args.first().is_some_and(|t| matches!(t, Token::Float(_))) {
                1
            } else {
                2
            }
        }
        _ => 1,
    }
}

// ── Final render ─────────────────────────────────────────────────────────────

struct RenderInput<'a> {
    state_fields: String,
    node_globals: String,
    init_stmts: String,
    signal_stmts: String,
    tick_stmts: String,
    dispatch: String,
    loadbang_stmts: String,
    dac_l: Vec<String>,
    dac_r: Vec<String>,
    params: &'a [ParamInfo],
    notein_id: Option<u32>,
    ctlin_nodes: Vec<(u32, Option<i32>)>,
    bendin_ids: Vec<u32>,
    touchin_ids: Vec<u32>,
    pgmin_ids: Vec<u32>,
    extra_includes: &'a BTreeSet<String>,
    need_noise_fn: bool,
}

fn render_output(inp: RenderInput) -> String {
    // AudioOutput's actual type (MonoOutput vs StereoOutput) is a
    // compile-time Mozzi config choice, not something the return statement
    // alone can decide — a sketch that `return`s a StereoOutput while
    // AudioOutput resolves to MonoOutput fails to compile. So this needs to
    // be set here, matching whether dac~ has anything wired to inlet 1.
    let stereo = !inp.dac_r.is_empty();

    let mut out = String::new();
    out.push_str("// Generated by pdast2mozzi — do not edit\n");
    out.push_str("// Adjust MOZZI_CONTROL_RATE below to taste before compiling.\n");
    out.push_str("#define MOZZI_CONTROL_RATE 64\n");
    if stereo {
        out.push_str("#define MOZZI_AUDIO_CHANNELS MOZZI_STEREO\n");
    }
    out.push_str("#include <Mozzi.h>\n");
    for inc in inp.extra_includes {
        out.push_str(inc);
        out.push('\n');
    }
    out.push_str("#include <math.h>\n#include <stdint.h>\n\n");

    out.push_str(
        "#define PD_MSG_MAX 8\n#define PD_MAX_DEPTH 64\n\ntypedef struct { int len; float a[PD_MSG_MAX]; } PdMsg;\n\nstatic inline PdMsg pd_msg_bang(void) { PdMsg m; m.len = 0; return m; }\nstatic inline PdMsg pd_msg_f(float v) { PdMsg m; m.len = 1; m.a[0] = v; return m; }\n\nstatic int pd_depth __attribute__((unused)) = 0;\n\n",
    );

    if inp.need_noise_fn {
        out.push_str(
            "static uint32_t pd_noise_state = 2463534242UL;\nstatic inline int32_t pd_noise_next() {\n  pd_noise_state ^= pd_noise_state << 13;\n  pd_noise_state ^= pd_noise_state >> 17;\n  pd_noise_state ^= pd_noise_state << 5;\n  return (int32_t)(pd_noise_state % 256) - 128;\n}\n\n",
        );
    }

    out.push_str("// ── unit generator instances ──────────────────────────────\n");
    out.push_str(&inp.node_globals);
    out.push('\n');

    out.push_str("// ── node state ────────────────────────────────────────────\n");
    out.push_str(&inp.state_fields);
    out.push('\n');

    out.push_str(&inp.dispatch);

    let dac_l_sum = if inp.dac_l.is_empty() {
        "0".to_string()
    } else {
        inp.dac_l.join(" + ")
    };
    let dac_r_sum = if inp.dac_r.is_empty() {
        "0".to_string()
    } else {
        inp.dac_r.join(" + ")
    };

    out.push_str("void setup() {\n  startMozzi();\n");
    out.push_str(&inp.init_stmts);
    out.push_str(&inp.loadbang_stmts);
    out.push_str("}\n\n");

    out.push_str("void updateControl() {\n");
    out.push_str(&inp.tick_stmts);
    out.push_str("}\n\n");

    out.push_str("AudioOutput updateAudio() {\n");
    out.push_str(&inp.signal_stmts);
    if stereo {
        out.push_str(&format!(
            "  return StereoOutput::fromNBit(9, constrain((int32_t)({dac_l_sum}), -256, 255), constrain((int32_t)({dac_r_sum}), -256, 255));\n"
        ));
    } else {
        out.push_str(&format!(
            "  return MonoOutput::fromNBit(9, constrain((int32_t)({dac_l_sum}), -256, 255));\n"
        ));
    }
    out.push_str("}\n\n");

    out.push_str("void loop() {\n  audioHook();\n}\n\n");

    // ── MIDI / param hook-function surface ──────────────────────────────────
    // Mozzi has no built-in MIDI input — wire these to your own MIDI library,
    // e.g. `MIDI.setHandleNoteOn([](byte ch, byte note, byte vel){ pd_note_on(note, vel); });`
    out.push_str("// ── pd_* hooks: call these from your own MIDI library / pot-reading code ──\n");

    out.push_str("void pd_note_on(byte note, byte velocity) {\n");
    match inp.notein_id {
        Some(nid) => out.push_str(&format!(
            "  {chan}(pd_msg_f(1.0f));\n  {vel}(pd_msg_f((float)velocity));\n  {pitch}(pd_msg_f((float)note));\n",
            chan = send_fn(nid, 2), vel = send_fn(nid, 1), pitch = send_fn(nid, 0)
        )),
        None => out.push_str("  (void)note; (void)velocity;\n"),
    }
    out.push_str("}\n\n");

    out.push_str("void pd_note_off(byte note, byte velocity) {\n");
    match inp.notein_id {
        Some(nid) => out.push_str(&format!(
            "  (void)velocity;\n  {vel}(pd_msg_f(0.0f));\n  {pitch}(pd_msg_f((float)note));\n",
            vel = send_fn(nid, 1),
            pitch = send_fn(nid, 0)
        )),
        None => out.push_str("  (void)note; (void)velocity;\n"),
    }
    out.push_str("}\n\n");

    out.push_str("void pd_control_change(byte controller, byte value) {\n");
    if inp.ctlin_nodes.is_empty() {
        out.push_str("  (void)controller; (void)value;\n");
    } else {
        for &(nid, filter) in &inp.ctlin_nodes {
            match filter {
                Some(cc) => out.push_str(&format!(
                    "  if (controller == {cc}) {{ {f}(pd_msg_f((float)value)); }}\n",
                    f = send_fn(nid, 0)
                )),
                None => out.push_str(&format!(
                    "  {fo1}(pd_msg_f((float)controller));\n  {f}(pd_msg_f((float)value));\n",
                    f = send_fn(nid, 0),
                    fo1 = send_fn(nid, 1)
                )),
            }
        }
    }
    out.push_str("}\n\n");

    out.push_str("void pd_pitch_bend(int value) {\n");
    if inp.bendin_ids.is_empty() {
        out.push_str("  (void)value;\n");
    } else {
        for &nid in &inp.bendin_ids {
            out.push_str(&format!(
                "  {f}(pd_msg_f((float)value));\n",
                f = send_fn(nid, 0)
            ));
        }
    }
    out.push_str("}\n\n");

    out.push_str("void pd_touch(byte value) {\n");
    if inp.touchin_ids.is_empty() {
        out.push_str("  (void)value;\n");
    } else {
        for &nid in &inp.touchin_ids {
            out.push_str(&format!(
                "  {f}(pd_msg_f((float)value));\n",
                f = send_fn(nid, 0)
            ));
        }
    }
    out.push_str("}\n\n");

    out.push_str("void pd_program_change(byte value) {\n");
    if inp.pgmin_ids.is_empty() {
        out.push_str("  (void)value;\n");
    } else {
        for &nid in &inp.pgmin_ids {
            out.push_str(&format!(
                "  {f}(pd_msg_f((float)value));\n",
                f = send_fn(nid, 0)
            ));
        }
    }
    out.push_str("}\n\n");

    out.push_str("void pd_set_param(int index, float value) {\n  switch (index) {\n");
    for (i, p) in inp.params.iter().enumerate() {
        for &tid in &p.target_ids {
            out.push_str(&format!(
                "    case {i}: {}(pd_msg_f(value)); break;\n",
                send_fn(tid, 0)
            ));
        }
        if p.target_ids.is_empty() {
            out.push_str(&format!("    case {i}: break;\n"));
        }
    }
    out.push_str("    default: break;\n  }\n}\n\n");

    out.push_str("float pd_get_param(int index) {\n  switch (index) {\n");
    for (i, p) in inp.params.iter().enumerate() {
        if let Some(&tid) = p.target_ids.first() {
            out.push_str(&format!("    case {i}: return {};\n", field(tid)));
        }
    }
    out.push_str("    default: return 0.0f;\n  }\n}\n\n");

    out.push_str(
        "struct PdParamInfo { const char* name; float min; float max; float default_value; };\n",
    );
    out.push_str(&format!(
        "const int PD_NUM_PARAMS = {};\n",
        inp.params.len()
    ));
    out.push_str("const PdParamInfo PD_PARAMS[] = {\n");
    for p in inp.params {
        out.push_str(&format!(
            "  {{ \"{}\", {}, {}, {} }},\n",
            escape_c_string(&p.name),
            cf(p.min),
            cf(p.max),
            cf(p.default)
        ));
    }
    if inp.params.is_empty() {
        out.push_str("  { \"\", 0, 0, 0 }, // unused: PD_NUM_PARAMS is 0\n");
    }
    out.push_str("};\n");

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdast::parse_patch_no_loader;

    fn generate(src: &str) -> (String, Vec<String>) {
        let result = parse_patch_no_loader(src).unwrap();
        let mut g = MozziGenerator::new();
        let ino = g.generate(&result.patch.root);
        (ino, g.warnings)
    }

    #[test]
    fn sine_patch_uses_oscil_and_mono_output() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/fixtures/sine.pd"
        ))
        .unwrap();
        let (ino, _warnings) = generate(&src);
        assert!(ino.contains("Oscil<SIN2048_NUM_CELLS, MOZZI_AUDIO_RATE>"));
        assert!(ino.contains("updateAudio"));
        // sine.pd wires *~ into both dac~ inlets, so this is stereo.
        assert!(ino.contains("StereoOutput::fromNBit"));
        assert!(ino.contains("startMozzi();"));
        // Regression: AudioOutput's actual type (Mono vs Stereo) is a
        // compile-time Mozzi config choice, not decided by the return
        // statement alone — a stereo return needs this #define or it
        // fails to compile against the real Mozzi library.
        assert!(ino.contains("#define MOZZI_AUDIO_CHANNELS MOZZI_STEREO"));
        let channels_idx = ino.find("#define MOZZI_AUDIO_CHANNELS").unwrap();
        let include_idx = ino.find("#include <Mozzi.h>").unwrap();
        assert!(channels_idx < include_idx, "MOZZI_AUDIO_CHANNELS must be defined before #include <Mozzi.h>");
    }

    #[test]
    fn midi_patch_emits_note_hooks() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/fixtures/midi.pd"
        ))
        .unwrap();
        let (ino, _warnings) = generate(&src);
        assert!(ino.contains("void pd_note_on(byte note, byte velocity)"));
        assert!(!ino.contains("(void)note; (void)velocity;\n}"));
    }

    #[test]
    fn control_patch_generates_without_panicking() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/fixtures/control.pd"
        ))
        .unwrap();
        let (ino, _warnings) = generate(&src);
        assert!(ino.contains("void updateControl()"));
        assert!(ino.contains("void loop()"));
    }

    #[test]
    fn mono_dac_does_not_emit_stereo_channels_define() {
        let src = "#N canvas 0 0 400 300 12;\n#X obj 10 10 osc~ 440;\n#X obj 10 40 dac~;\n#X connect 0 0 1 0;\n";
        let (ino, _warnings) = generate(src);
        assert!(ino.contains("MonoOutput::fromNBit"));
        assert!(!ino.contains("MOZZI_AUDIO_CHANNELS"));
    }

    #[test]
    fn substitute_dollars_replaces_by_position() {
        let args = vec![
            Token::Dollar(1),
            Token::Symbol("hz".into()),
            Token::Dollar(2),
        ];
        let call_args = vec![Token::Float(440.0), Token::Float(2.0)];
        let out = substitute_dollars(&args, &call_args);
        assert_eq!(out[0], Token::Float(440.0));
        assert_eq!(out[1], Token::Symbol("hz".into()));
        assert_eq!(out[2], Token::Float(2.0));
    }

    #[test]
    fn unsupported_object_warns_but_does_not_panic() {
        let src = "#N canvas 0 0 400 300 12;\n#X obj 10 10 some_bogus_object~;\n";
        let (_ino, warnings) = generate(src);
        assert!(warnings.iter().any(|w| w.contains("some_bogus_object~")));
    }

    #[test]
    fn phasor_stays_unipolar_float_not_table_amplitude_scale() {
        // Regression: phasor~ must stay a plain 0..1 float (real PD range),
        // not the ±128 table-amplitude convention osc~/noise~ use — an LFO
        // idiom like [phasor~ 5]->[*~ 20]->[+~ 440] depends on it.
        let src = "#N canvas 0 0 400 300 12;\n#X obj 10 10 phasor~ 5;\n";
        let (ino, _warnings) = generate(src);
        assert!(ino.contains("float pd_n0;"));
        assert!(!ino.contains("pd_n0 = -128;"));
        assert!(ino.contains("pd_n0 = (float)pd_n0_phase / 4294967296.0f;"));
    }
}
