//! C code generator from pdast AST — emits a self-contained "DSP unit" that
//! implements the fixed `pd_*` ABI documented in `pd_wclap.h`. A separate,
//! hand-written CLAP "runtime shim" (poketrack/plugins/pd2wclap/runtime-shim.c)
//! links against this generated code and provides the actual CLAP plugin
//! surface (clap_entry, audio/note/params extensions, event-time-splitting
//! process loop).
//!
//! # Design notes (vs. pdast2faust, which this borrows its shape from)
//!
//! - Every node's latest scalar output(s) are stored in a field on a single
//!   persistent `PdState` struct (`st->nID` / `st->nID_o1` for multi-outlet
//!   nodes), addressed by pointer rather than by local SSA variable. This
//!   means forward references AND feedback (cycles) are both automatically
//!   well-defined: reading a not-yet-updated-this-pass source just reads
//!   last pass's value, which is exactly the correct one-step-delay
//!   cycle-breaking behaviour — no explicit back-edge detection needed.
//! - The **signal domain** (tilde objects) is pull-based and recomputed every
//!   audio sample inside `pd_signal_step()`.
//! - The **control domain** (everything else) is a genuine **message-passing**
//!   graph, not a recomputed dataflow expression. Each object emits
//!   `pd_nX_inK()` inlet handlers and `pd_nX_outJ()` outlet fan-outs; a
//!   message delivered to a hot inlet computes and pushes onward depth-first,
//!   while cold inlets merely latch. That is what makes `route`/`select`
//!   fire exactly one outlet (leaving every other branch's latched state
//!   untouched), `trigger` fire right-to-left, message boxes emit their own
//!   contents when banged, `change` actually filter repeats, and a list
//!   arriving at a hot inlet distribute across the inlets to its right.
//!   A recompute pass fundamentally cannot express any of these, because it
//!   has no notion of "which wire delivered something" — only "what does
//!   each wire currently read".
//! - Outlet fan-out keeps each node's `st->nX` value field current, so the
//!   pull-based signal graph reads control values without being message-aware.
//!   Recursion is bounded by a PD-style depth guard (`PD_MAX_DEPTH`).
//! - Sub-patches/abstractions are fully flattened (graph-spliced with id
//!   remapping and real `$1`/`$2` substitution) before codegen, rather than
//!   pdast2faust's expression-string-splicing approach (which is confirmed
//!   to drop the inner canvas's bindings and produce dangling references).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use pdast::types::{Canvas, Connection, Node, NodeKind, SubPatchContent, Token};

// ── Flattening (sub-patch inlining with $ substitution) ────────────────────
//
// Sub-patches/abstractions are fully graph-spliced (ids remapped into one
// global namespace, $1/$2 substituted, inlet~/outlet~ boundaries rewired)
// rather than string-spliced the way pdast2faust does it — see module docs.

/// Substitute `Token::Dollar(n)`/`Token::DollarZero` in `args` using the
/// caller's `call_args` (1-based: $1 -> call_args[0]). Falls back to Float(0)
/// when the index is out of range, matching PD's own behaviour for missing args.
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

/// Flatten a whole patch (root canvas + all inline sub-patches) into one
/// combined node/connection list with a single, unique id space.
pub fn flatten_patch(canvas: &Canvas) -> (Vec<Node>, Vec<Connection>) {
    let mut f = FlattenerWithBoundaries::new();
    f.run(canvas, &[]);
    f.resolve_boundaries();
    (f.nodes, f.connections)
}

// The real, working flattener (the two-part sketch above is folded into this
// single implementation to keep boundary bookkeeping straightforward).
struct FlattenerWithBoundaries {
    next_id: u32,
    nodes: Vec<Node>,
    connections: Vec<Connection>,
    /// subpatch placeholder id -> (inlet idx -> node id, outlet idx -> node id)
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
                    // Placeholder kept out of self.nodes entirely — it has
                    // no compute semantics of its own once inlined.
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

    /// Rewrite every connection that touches an inlined-subpatch placeholder
    /// id to instead touch that subpatch's inlet~/outlet~ node directly.
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

/// Number of outlets a node produces (mirrors pdast2faust's outlet_count,
/// extended with our own supported object set).
fn outlet_count(node: &Node) -> usize {
    match &node.kind {
        NodeKind::Obj { name, args } => match name.as_str() {
            "dac~" | "send" | "s" | "print" | "delwrite~" | "send~" | "throw~" => 0,
            "notein" => 3,
            "vcf~" | "moses" | "swap" => 2,
            "ctlin" => {
                if args.first().is_some_and(|t| matches!(t, Token::Float(_))) {
                    1
                } else {
                    2
                }
            }
            "sel" | "select" | "route" => {
                let n = args.iter().filter(|t| matches!(t, Token::Float(_))).count();
                (n.max(1)) + 1
            }
            "pack" | "unpack" => args.len().max(2),
            "trigger" | "t" => args.len().max(1),
            // voice number, pitch, velocity — as in PD
            "poly" => 3,
            _ => 1,
        },
        NodeKind::Gui(_) => 1,
        NodeKind::FloatAtom { .. } | NodeKind::SymbolAtom { .. } | NodeKind::Msg { .. } => 1,
        _ => 1,
    }
}

// ── Topological order (see module docs: cycles are handled for free by
//    always reading persistent state, so this needs no explicit
//    cycle-breaking beyond deterministic tie-breaking) ─────────────────────

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
    // Cycle members (never reached 0 in-degree): append in id order. Since
    // every node's output is read via persistent state (st->nID) rather
    // than a fresh local, this is well-defined (one-block-delayed feedback)
    // rather than a dangling reference.
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

/// Signal-rate `send~`/`receive~` bus: a completely separate namespace from
/// `collect_bus_map`'s control-rate `send`/`receive`/`value` (same as real
/// PD — `[send~ foo]` and `[send foo]` never alias each other), so it's kept
/// in its own map rather than sharing keys.
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

/// `throw~`/`catch~` submix bus: like `collect_signal_bus_map` but every
/// `throw~` sharing a name is meant to be *summed* by the matching `catch~`
/// (PD's audio submix bus), not just mirrored from a single sender.
fn collect_throw_bus_map(nodes: &[Node]) -> BTreeMap<String, BusEntry> {
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
            "throw~" => entry.senders.push(node.id),
            "catch~" => entry.receivers.push(node.id),
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
    /// Node id whose persistent state field should be seeded from
    /// pd_set_param() — for a GUI object this is the GUI node itself; for a
    /// bare `receive`/`r`/`value` with no matching sender this is that node.
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
                    continue; // driven internally by a send — not an external param
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
// Buffers are keyed by their PD name (not node id) so a delread~/vd~ can
// reference the same buffer as a same-named delwrite~ regardless of
// topological order. Maps name -> max delay in ms (the largest maxms seen
// across all delwrite~s sharing that name, used to size the buffer).

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

// ── Arrays / tables ──────────────────────────────────────────────────────────
//
// `NodeKind::Array` lives inside a `NodeKind::Graph`'s nested canvas (Pd's
// array/table window). Sizes are known at codegen time (unlike delay lines,
// which need the runtime sample rate), so each array becomes a fixed-size
// `double[]` field directly on `PdState`, keyed by name and seeded from the
// patch's saved data (or zeros if none was saved).
pub struct ArrayInfo {
    pub size: u32,
    pub data: Vec<f64>,
}

fn collect_arrays(nodes: &[Node]) -> BTreeMap<String, ArrayInfo> {
    let mut map: BTreeMap<String, ArrayInfo> = BTreeMap::new();
    for node in nodes {
        if let NodeKind::Graph { content } = &node.kind {
            for inner in &content.nodes {
                if let NodeKind::Array {
                    name, size, data, ..
                } = &inner.kind
                {
                    map.insert(
                        name.clone(),
                        ArrayInfo {
                            size: (*size).max(1),
                            data: data.clone(),
                        },
                    );
                }
            }
        }
    }
    map
}

// ── C identifier helpers ────────────────────────────────────────────────────

fn field(id: u32) -> String {
    format!("n{id}")
}
fn field_o(id: u32, outlet: u32) -> String {
    if outlet == 0 {
        field(id)
    } else {
        format!("n{id}_o{outlet}")
    }
}

/// Latch field holding the last message value received on a node's inlet.
/// Cold inlets read this; it's what makes them hold their value between
/// messages, exactly as a real PD inlet does.
fn inlet_field(id: u32, inlet: u32) -> String {
    format!("n{id}_i{inlet}")
}

/// Name of the emitted C function that delivers a message to a node's inlet.
fn recv_fn(id: u32, inlet: u32) -> String {
    format!("pd_n{id}_in{inlet}")
}

/// Name of the emitted C function that fans a message out from a node's
/// outlet to every inlet wired to it.
fn send_fn(id: u32, outlet: u32) -> String {
    format!("pd_n{id}_out{outlet}")
}

/// Turn an arbitrary PD symbol (array/delay-line name) into a valid C
/// identifier fragment.
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

// ── Main generator ────────────────────────────────────────────────────────────

pub struct WclapGenerator {
    pub warnings: Vec<String>,
    /// Delay-line names whose shared buffer fields have already been
    /// declared/init'd/freed by a `delwrite~` node — a second `delwrite~`
    /// with the same name (unusual, but legal PD) must reuse them rather
    /// than re-declaring the same struct fields.
    declared_delay_lines: HashSet<String>,
    /// Message-node id -> how many inlet latch fields it declared (0 for a
    /// pure source like `notein`). Bounds both cold-inlet storage and how far
    /// a list arriving at the hot inlet is distributed across cold inlets.
    
    node_latch_count: HashMap<u32, u32>,
    /// Objects whose cold inlet writes the *same* cell the hot inlet outputs
    /// (`f`/`float`, `i`/`int`): PD's float box holds a single value, so a
    /// cold-inlet write must be visible to the next hot-inlet bang.
    cold_aliases_hot: HashSet<u32>,
}

impl WclapGenerator {
    pub fn new() -> Self {
        WclapGenerator {
            warnings: Vec::new(),
            declared_delay_lines: HashSet::new(),
            node_latch_count: HashMap::new(),
            cold_aliases_hot: HashSet::new(),
        }
    }

    pub fn generate(&mut self, canvas: &Canvas) -> String {
        self.declared_delay_lines.clear();
        let (nodes, connections) = flatten_patch(canvas);

        let active: Vec<&Node> = nodes
            .iter()
            .filter(|n| !matches!(n.kind, NodeKind::Text { .. } | NodeKind::Array { .. }))
            .collect();
        let active_ids: Vec<u32> = active.iter().map(|n| n.id).collect();
        let node_by_id: HashMap<u32, &Node> = active.iter().map(|n| (n.id, *n)).collect();

        let bus_map = collect_bus_map(&nodes);
        let signal_bus_map = collect_signal_bus_map(&nodes);
        let throw_bus_map = collect_throw_bus_map(&nodes);
        let params = collect_params(&nodes);

        let order = topo_order(&active_ids, &connections);

        let mut node_outlets: HashMap<u32, usize> = HashMap::new();
        for n in &active {
            node_outlets.insert(n.id, outlet_count(n));
        }

        // Signal-domain source ids — used by input_expr below to decide
        // fan-in behavior: real PD automatically *sums* multiple signal
        // connections landing on the same inlet (this is how virtually
        // every dac~/mixing point in real patches works — wire N voices
        // straight into one dac~ and PD sums them), but does no such thing
        // for control connections (control fan-in is a discrete "last
        // message wins" in real PD, which doesn't translate to this
        // continuous model anyway, so it keeps the simpler "first
        // connection wins" behavior it already had).
        let mut signal_node_ids: HashSet<u32> = HashSet::new();
        for n in &active {
            if let NodeKind::Obj { name, .. } = &n.kind {
                if domain_of(name) == Domain::Signal {
                    signal_node_ids.insert(n.id);
                }
            }
        }

        let has_audio_in = active
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name=="adc~"));
        let notein_id: Option<u32> = active
            .iter()
            .find(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name=="notein"))
            .map(|n| n.id);
        let has_note_in = notein_id.is_some();

        // ctlin optionally filters to one controller# (first creation arg);
        // with none it reports every CC on 2 outlets (value, controller#).
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
            .filter(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name=="bendin"))
            .map(|n| n.id)
            .collect();
        let touchin_ids: Vec<u32> = active
            .iter()
            .filter(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name=="touchin"))
            .map(|n| n.id)
            .collect();
        let pgmin_ids: Vec<u32> = active
            .iter()
            .filter(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name=="pgmin"))
            .map(|n| n.id)
            .collect();

        let delay_lines = collect_delay_lines(&active);
        let arrays = collect_arrays(&nodes);

        let mut state_fields = String::new();
        let mut init_stmts = String::new();
        let mut destroy_stmts = String::new();
        let mut signal_stmts = String::new();
        let mut hot_bodies: BTreeMap<u32, String> = BTreeMap::new();
        let mut dac_l: Vec<String> = Vec::new();
        let mut dac_r: Vec<String> = Vec::new();

        for (name, info) in &arrays {
            let c = sanitize_c_name(name);
            state_fields.push_str(&format!("  double arr_{c}[{}];\n", info.size));
            if info.data.is_empty() {
                init_stmts.push_str(&format!("  memset(st->arr_{c}, 0, sizeof(st->arr_{c}));\n"));
            } else {
                let vals: Vec<String> = (0..info.size as usize)
                    .map(|i| format!("{}", info.data.get(i).copied().unwrap_or(0.0)))
                    .collect();
                init_stmts.push_str(&format!(
                    "  {{ static const double _init_{c}[] = {{ {} }};\n    memcpy(st->arr_{c}, _init_{c}, sizeof(_init_{c}));\n  }}\n",
                    vals.join(", ")
                ));
            }
        }

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
                &throw_bus_map,
                &delay_lines,
                &arrays,
                &mut dac_l,
                &mut dac_r,
                &mut destroy_stmts,
            );

            state_fields.push_str(&emitted.state_fields);
            init_stmts.push_str(&emitted.init);
            match emitted.domain {
                Domain::Signal => signal_stmts.push_str(&emitted.compute),
                // For a message node `compute` is the *body of its hot-inlet
                // handler*, not a statement in a global recompute pass.
                Domain::Control => {
                    hot_bodies.insert(id, emitted.compute);
                }
            }
        }

        let dispatch = self.render_dispatch(&hot_bodies, &connections, &node_by_id, &signal_node_ids);

        // `loadbang` fires a single bang the first time audio is rendered —
        // the closest thing this ABI has to "patch finished loading".
        let mut loadbang_stmts = String::new();
        let loadbang_ids: Vec<u32> = active
            .iter()
            .filter(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name == "loadbang"))
            .map(|n| n.id)
            .collect();
        if !loadbang_ids.is_empty() {
            loadbang_stmts.push_str("  if (!st->_loadbanged) {\n    st->_loadbanged = 1;\n");
            for lid in loadbang_ids {
                loadbang_stmts.push_str(&format!("    {}(st, pd_msg_bang());\n", send_fn(lid, 0)));
            }
            loadbang_stmts.push_str("  }\n");
        }

        render_output(RenderInput {
            state_fields,
            init_stmts,
            destroy_stmts,
            signal_stmts,
            dispatch,
            loadbang_stmts,
            dac_l,
            dac_r,
            params: &params,
            has_audio_in,
            has_note_in,
            notein_id,
            ctlin_nodes,
            bendin_ids,
            touchin_ids,
            pgmin_ids,
        })
    }

    /// Emit the message-passing plumbing: one `pd_nX_outJ` fan-out function
    /// per message-node outlet, and one `pd_nX_inK` handler per inlet.
    ///
    /// This is what makes the control graph genuinely message-driven rather
    /// than a recomputed dataflow expression. The two properties that fall
    /// out of it, and that a recompute pass fundamentally cannot express:
    ///
    /// - **Only the outlet that actually fired propagates.** `route`/`sel`
    ///   send on one outlet; every other downstream branch keeps whatever it
    ///   last latched instead of being re-evaluated against the new input.
    /// - **Cold inlets hold.** An inlet only changes when a message arrives
    ///   on it, so the classic `[f ]`/`[+ 1]` counter and `poly`'s per-voice
    ///   latching behave as they do in PD.
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

        // Forward-declare every handler first: the graph is generally cyclic
        // in C terms (fan-out order is arbitrary), so definitions can't be
        // topologically ordered.
        for &id in &msg_ids {
            let n_in = self.node_latch_count.get(&id).copied().unwrap_or(0).max(1);
            for k in 0..n_in {
                decls.push_str(&format!(
                    "static void {}(PdState* st, PdMsg m);\n",
                    recv_fn(id, k)
                ));
            }
            let n_out = node_by_id.get(&id).map(|n| outlet_count(n)).unwrap_or(1);
            for j in 0..n_out {
                decls.push_str(&format!(
                    "static void {}(PdState* st, PdMsg m);\n",
                    send_fn(id, j as u32)
                ));
            }
        }

        for &id in &msg_ids {
            // A node may declare no latches at all (a pure source such as
            // `notein`), but every message node still needs a hot inlet.
            let n_latch = self.node_latch_count.get(&id).copied().unwrap_or(0);
            let n_in = n_latch.max(1);
            let n_out = node_by_id.get(&id).map(|n| outlet_count(n)).unwrap_or(1);

            for j in 0..n_out {
                let j = j as u32;
                defs.push_str(&format!(
                    "static void {}(PdState* st, PdMsg m) {{\n",
                    send_fn(id, j)
                ));
                // Keep the outlet's value field current so the *signal* graph
                // (which is pull-based and reads these directly) sees control
                // changes without needing to be message-aware at all.
                defs.push_str(&format!(
                    "  if (m.len >= 1) st->{} = m.a[0];\n",
                    field_o(id, j)
                ));
                let mut targets: Vec<&Connection> = connections
                    .iter()
                    .filter(|c| c.src_node == id && c.src_outlet == j)
                    // A tilde destination needs no delivery: it pulls from the
                    // value field written just above.
                    .filter(|c| !signal_node_ids.contains(&c.dst_node))
                    .collect();
                if targets.is_empty() {
                    defs.push_str("  (void)st; (void)m;\n}\n\n");
                    continue;
                }
                // PD fans a single outlet out in an unspecified order; fix it
                // to descending destination id so output is deterministic.
                targets.sort_by_key(|c| (std::cmp::Reverse(c.dst_node), c.dst_inlet));
                defs.push_str("  if (st->_depth >= PD_MAX_DEPTH) return;\n  st->_depth++;\n");
                for c in targets {
                    let dst_in = self
                        .node_latch_count
                        .get(&c.dst_node)
                        .copied()
                        .unwrap_or(0)
                        .max(1);
                    // Never deliver past the inlets the destination actually has.
                    let k = c.dst_inlet.min(dst_in - 1);
                    defs.push_str(&format!("  {}(st, m);\n", recv_fn(c.dst_node, k)));
                }
                defs.push_str("  st->_depth--;\n}\n\n");
            }

            for k in 0..n_in {
                defs.push_str(&format!(
                    "static void {}(PdState* st, PdMsg m) {{\n",
                    recv_fn(id, k)
                ));
                if k == 0 {
                    // A list arriving at the hot inlet is spread across the
                    // cold inlets to its right before the hot one triggers —
                    // PD's real list-distribution rule.
                    if n_latch > 1 {
                        defs.push_str(&format!(
                            "  for (int _i = 1; _i < m.len && _i < {n_latch}; _i++) {{\n    switch (_i) {{\n"
                        ));
                        for ci in 1..n_latch {
                            defs.push_str(&format!(
                                "      case {ci}: st->{} = m.a[_i]; break;\n",
                                inlet_field(id, ci)
                            ));
                        }
                        defs.push_str("      default: break;\n    }\n  }\n");
                    }
                    if n_latch > 0 {
                        defs.push_str(&format!(
                            "  if (m.len >= 1) st->{} = m.a[0];\n",
                            inlet_field(id, 0)
                        ));
                    }
                    let body = hot_bodies.get(&id).map(String::as_str).unwrap_or("");
                    if body.trim().is_empty() {
                        defs.push_str("  (void)st; (void)m;\n");
                    } else {
                        defs.push_str(body);
                    }
                } else {
                    // Cold inlet: latch only, never triggers output.
                    let target = if self.cold_aliases_hot.contains(&id) { 0 } else { k };
                    defs.push_str(&format!(
                        "  if (m.len >= 1) st->{} = m.a[0];\n",
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
        throw_bus_map: &BTreeMap<String, BusEntry>,
        delay_lines: &BTreeMap<String, f64>,
        arrays: &BTreeMap<String, ArrayInfo>,
        dac_l: &mut Vec<String>,
        dac_r: &mut Vec<String>,
        destroy_stmts: &mut String,
    ) -> EmittedNode {
        let id = node.id;

        // Is this a message-domain (control) object? Tilde objects pull their
        // inputs continuously from the source's output field; everything else
        // is message-driven and *latches* each connected inlet, which is what
        // gives cold inlets their "hold the last value received" behaviour.
        // Note this is deliberately keyed on the object *name*, not on the
        // Domain the arm ends up returning: metro/delay/pipe/timer are
        // message-driven objects whose timing merely runs per-sample, so they
        // want latched inlets too.
        let is_msg_node = match &node.kind {
            NodeKind::Obj { name, .. } => domain_of(name) == Domain::Control,
            _ => true,
        };

        // Records, for each connected inlet of a message node, the literal the
        // latch field should be initialised to (the creation-arg default), so
        // an un-messaged cold inlet still reads as its documented default.
        let latch_defaults: RefCell<BTreeMap<u32, String>> = RefCell::new(BTreeMap::new());

        // Resolve the C expression feeding a given inlet: for a message node,
        // that inlet's latch field (or the creation-arg default when nothing
        // is wired to it); for a tilde node, the connected source's output
        // field, else the default. Multiple signal-domain connections landing
        // on the same tilde inlet are summed (matches real PD auto-mixing
        // several signals into one inlet — most commonly dac~).
        let input_expr = |inlet: u32, default_lit: &str| -> String {
            // A message node always reads through a latch, even for an inlet
            // with nothing wired to it: PD delivers a *list* arriving at the
            // hot inlet across the following inlets, so an unconnected cold
            // inlet is still a real, writable destination. (This is what makes
            // `[pack f f] -> [poly]` fill poly's velocity inlet with no second
            // patch cord, exactly as it does in PD.)
            if is_msg_node {
                latch_defaults
                    .borrow_mut()
                    .entry(inlet)
                    .or_insert_with(|| default_lit.to_string());
                return format!("st->{}", inlet_field(id, inlet));
            }
            let mut matches: Vec<&&Connection> =
                incoming.iter().filter(|c| c.dst_inlet == inlet).collect();
            if matches.is_empty() {
                return default_lit.to_string();
            }
            let field_of = |c: &Connection| -> String {
                let n_out = node_outlets.get(&c.src_node).copied().unwrap_or(1) as u32;
                format!(
                    "st->{}",
                    field_o(c.src_node, if n_out > 1 { c.src_outlet } else { 0 })
                )
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
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = {};\n", g.default_value),
                    compute: String::new(), // driven externally via pd_set_param(), not recomputed
                }
            }

            NodeKind::FloatAtom { .. } | NodeKind::SymbolAtom { .. } => {
                let f = field(id);
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: String::new(),
                }
            }

            // A message box now behaves like PD's: any message arriving at
            // its inlet makes it emit its own literal contents. That is what
            // lets `[sel 0]` -> `[1 10(` / `[0 200(` -> one `vline~` inlet
            // work — two boxes can share a downstream inlet, because each
            // only fires when actually triggered.
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
                    body.push_str(&format!("  _o.a[{i}] = {v};\n"));
                }
                if atoms.is_empty() {
                    body = "  PdMsg _o = pd_msg_bang();\n".to_string();
                }
                body.push_str(&format!("  {}(st, _o);\n", send_fn(id, 0)));
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = {first};\n"),
                    compute: body,
                }
            }

            NodeKind::Obj { name, args } => self.emit_obj(
                name,
                args,
                id,
                node_outlets,
                bus_map,
                signal_bus_map,
                throw_bus_map,
                delay_lines,
                arrays,
                &input_expr,
                dac_l,
                dac_r,
                destroy_stmts,
            ),

            _ => EmittedNode {
                domain: Domain::Control,
                state_fields: String::new(),
                init: String::new(),
                compute: String::new(),
            },
        };

        // Declare a latch field per connected inlet of a message node, seeded
        // with that inlet's creation-arg default so a cold inlet that never
        // receives a message still reads as documented.
        let latches = latch_defaults.borrow();
        for (inlet, default) in latches.iter() {
            let lf = inlet_field(id, *inlet);
            emitted.state_fields.push_str(&format!("  double {lf};\n"));
            emitted
                .init
                .push_str(&format!("  st->{lf} = {default};\n"));
        }
        if is_msg_node {
            // How many latch fields exist. May be 0 for a pure source
            // (`notein`, `receive`, `loadbang`) that reads no inlet at all —
            // such a node still gets a hot inlet handler, it just has nothing
            // to store.
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
        _node_outlets: &HashMap<u32, usize>,
        bus_map: &BTreeMap<String, BusEntry>,
        signal_bus_map: &BTreeMap<String, BusEntry>,
        throw_bus_map: &BTreeMap<String, BusEntry>,
        delay_lines: &BTreeMap<String, f64>,
        arrays: &BTreeMap<String, ArrayInfo>,
        input_expr: &dyn Fn(u32, &str) -> String,
        dac_l: &mut Vec<String>,
        dac_r: &mut Vec<String>,
        destroy_stmts: &mut String,
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

        match name {
            // ── send / receive / value ────────────────────────────────────
            "send" | "s" => {
                let receivers: Vec<u32> = bus_name(args)
                    .and_then(|b| bus_map.get(&b))
                    .map(|e| e.receivers.clone())
                    .unwrap_or_default();
                let _ = input_expr(0, "0.0");
                // Forward the message itself (bang/float/list all preserved)
                // to each receive on this bus.
                let compute: String = receivers
                    .iter()
                    .map(|r| format!("  {}(st, m);\n", send_fn(*r, 0)))
                    .collect();
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute,
                }
            }
            "receive" | "r" | "value" => {
                let Some(bname) = bus_name(args) else {
                    return EmittedNode {
                        domain: Domain::Control,
                        state_fields: String::new(),
                        init: String::new(),
                        compute: String::new(),
                    };
                };
                // Pure source either way: a matching `send` pushes into this
                // node's outlet directly, and a receive with no sender is a
                // registered param driven by pd_set_param.
                let _ = bus_map;
                // No sender: this is a registered param (see collect_params)
                // — pd_set_param() writes st->nID directly, no per-event
                // recompute needed.
                let _ = &bname;
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: String::new(),
                }
            }

            // ── send~ / receive~ (signal-rate bus — own namespace, never
            //    aliases a same-named control send/receive/value) ──────────
            "send~" => {
                let has_receiver = bus_name(args)
                    .and_then(|b| signal_bus_map.get(&b))
                    .is_some_and(|e| !e.receivers.is_empty());
                let in0 = input_expr(0, "0.0");
                let compute = if has_receiver {
                    format!("  st->{f} = {in0};\n")
                } else {
                    String::new() // dropped: no receiver, true sink
                };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute,
                }
            }
            "receive~" => {
                let Some(bname) = bus_name(args) else {
                    return EmittedNode {
                        domain: Domain::Signal,
                        state_fields: String::new(),
                        init: String::new(),
                        compute: String::new(),
                    };
                };
                let sender = signal_bus_map.get(&bname).and_then(|e| e.senders.first());
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: match sender {
                        Some(&s) => format!("  st->{f} = st->{};\n", field(s)),
                        None => String::new(), // no matching send~: silent
                    },
                }
            }

            // ── throw~ / catch~ (signal-rate submix bus: every throw~
            //    sharing a name is summed by the matching catch~) ──────────
            "throw~" => {
                let in0 = input_expr(0, "0.0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!("  st->{f} = {in0};\n"),
                }
            }
            "catch~" => {
                let Some(bname) = bus_name(args) else {
                    return EmittedNode {
                        domain: Domain::Signal,
                        state_fields: format!("  double {f};\n"),
                        init: format!("  st->{f} = 0.0;\n"),
                        compute: String::new(),
                    };
                };
                let throwers: Vec<u32> = throw_bus_map
                    .get(&bname)
                    .map(|e| e.senders.clone())
                    .unwrap_or_default();
                let sum = if throwers.is_empty() {
                    "0.0".to_string()
                } else {
                    throwers
                        .iter()
                        .map(|&t| format!("st->{}", field(t)))
                        .collect::<Vec<_>>()
                        .join(" + ")
                };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!("  st->{f} = {sum};\n"),
                }
            }

            // ── sub-patch/abstraction boundary objects ──────────────────────
            // flatten_patch's resolve_boundaries() rewrites every connection
            // that touched the `[pd name]`/abstraction placeholder to touch
            // these nodes directly instead (parent-side wires now target an
            // `inlet`/`inlet~` node's own inlet 0; sibling-side wires now
            // source from an `outlet`/`outlet~` node's own outlet 0) — but
            // that only fixes the *wiring*, not the value. The boundary node
            // itself still needs to actually carry the value across, i.e.
            // plain passthrough, same idiom as `f`/`float`.
            // Sub-patch boundaries are pure passthroughs: they must forward
            // the message *verbatim*, preserving bangs and lists. Collapsing
            // to a float here would truncate e.g. a `pitch velocity` pair
            // crossing into a voice abstraction down to just the pitch.
            "inlet" | "outlet" => {
                let _ = input_expr(0, "0.0");
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!("  {}(st, m);\n", send_fn(id, 0)),
                }
            }
            "inlet~" | "outlet~" => {
                let a = input_expr(0, "0.0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!("  st->{f} = {a};\n"),
                }
            }

            // ── control math ────────────────────────────────────────────────
            "+" | "-" | "*" | "/" | "max" | "min" | "mod" | "pow" => {
                let a = input_expr(0, "0.0");
                let b = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                let op = match name {
                    "+" => format!("({a}) + ({b})"),
                    "-" => format!("({a}) - ({b})"),
                    "*" => format!("({a}) * ({b})"),
                    "/" => format!("(({b}) != 0.0 ? ({a}) / ({b}) : 0.0)"),
                    "max" => format!("(({a}) > ({b}) ? ({a}) : ({b}))"),
                    "min" => format!("(({a}) < ({b}) ? ({a}) : ({b}))"),
                    "mod" => format!("fmod({a}, ({b}) != 0.0 ? ({b}) : 1.0)"),
                    "pow" => format!("pow({a}, {b})"),
                    _ => unreachable!(),
                };
                simple_control(id, &op)
            }
            "mtof" => {
                let a = input_expr(0, "60.0");
                simple_control(id, &format!("440.0 * pow(2.0, (({a}) - 69.0) / 12.0)"))
            }
            "ftom" => {
                let a = input_expr(0, "440.0");
                simple_control(
                    id,
                    &format!("69.0 + 12.0 * (log(({a}) / 440.0) / log(2.0))"),
                )
            }

            // ── trig / unary math — Pd's plain (non-signal) sin/cos take a
            //    phase in *cycles* (0..1), same convention as osc~/phasor~,
            //    not radians; atan/atan2 return radians directly. ──────────
            "sin" => {
                let a = input_expr(0, "0.0");
                simple_control(id, &format!("sin(({a}) * 6.283185307179586)"))
            }
            "cos" => {
                let a = input_expr(0, "0.0");
                simple_control(id, &format!("cos(({a}) * 6.283185307179586)"))
            }
            "atan" => {
                let a = input_expr(0, "0.0");
                simple_control(id, &format!("atan({a})"))
            }
            "atan2" => {
                let a = input_expr(0, "0.0");
                let b = input_expr(1, "0.0");
                simple_control(id, &format!("atan2({a}, {b})"))
            }
            "abs" => {
                let a = input_expr(0, "0.0");
                simple_control(id, &format!("fabs({a})"))
            }
            "sqrt" => {
                let a = input_expr(0, "0.0");
                simple_control(id, &format!("(({a}) > 0.0 ? sqrt({a}) : 0.0)"))
            }
            "log" => {
                let a = input_expr(0, "0.0");
                let base = if args.is_empty() { 1.0 } else { farg(0) };
                let expr = if base > 0.0 && base != 1.0 {
                    format!("(({a}) > 0.0 ? log({a}) / {base} : 0.0)", base = base.ln())
                } else {
                    format!("(({a}) > 0.0 ? log({a}) : 0.0)")
                };
                simple_control(id, &expr)
            }
            "exp" => {
                let a = input_expr(0, "0.0");
                simple_control(id, &format!("exp({a})"))
            }
            "wrap" => {
                let a = input_expr(0, "0.0");
                simple_control(id, &format!("(({a}) - floor({a}))"))
            }
            "clip" => {
                let a = input_expr(0, "0.0");
                let lo = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                let hi = input_expr(
                    2,
                    &format!("{}", if args.len() < 2 { 1.0 } else { farg(1) }),
                );
                simple_control(
                    id,
                    &format!("(({a}) < ({lo}) ? ({lo}) : (({a}) > ({hi}) ? ({hi}) : ({a})))"),
                )
            }
            "int" | "i" => {
                let a = input_expr(0, "0.0");
                simple_control(id, &format!("(double)(int64_t)({a})"))
            }
            // PD's float box: one storage cell. A message to the hot inlet
            // outputs it (a bang re-outputs the stored value unchanged); the
            // cold inlet writes the same cell without outputting — which is
            // what makes the classic `[f ]x[+ 1]` counter work.
            "f" | "float" => {
                let default = format!("{}", if args.is_empty() { 0.0 } else { farg(0) });
                let a = input_expr(0, &default);
                let _ = input_expr(1, &default);
                self.cold_aliases_hot.insert(id);
                simple_control(id, &a)
            }

            // ── comparisons / logic ─────────────────────────────────────────
            ">" | "<" | ">=" | "<=" | "==" | "!=" | "&&" | "||" => {
                let a = input_expr(0, "0.0");
                let b = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                let op = match name {
                    ">" => ">",
                    "<" => "<",
                    ">=" => ">=",
                    "<=" => "<=",
                    "==" => "==",
                    "!=" => "!=",
                    "&&" => "&&",
                    "||" => "||",
                    _ => unreachable!(),
                };
                simple_control(id, &format!("(({a}) {op} ({b}) ? 1.0 : 0.0)"))
            }
            "!" => {
                let a = input_expr(0, "0.0");
                simple_control(id, &format!("(({a}) == 0.0 ? 1.0 : 0.0)"))
            }

            // ── unit conversions ─────────────────────────────────────────────
            "dbtorms" => {
                let a = input_expr(0, "0.0");
                simple_control(
                    id,
                    &format!("(({a}) > 0.0 ? pow(10.0, (({a}) - 100.0) / 20.0) : 0.0)"),
                )
            }
            "rmstodb" => {
                let a = input_expr(0, "0.0");
                simple_control(
                    id,
                    &format!("(({a}) > 0.0 ? 100.0 + 20.0 * (log10({a})) : 0.0)"),
                )
            }
            "dbtopow" => {
                let a = input_expr(0, "0.0");
                simple_control(
                    id,
                    &format!("(({a}) > 0.0 ? pow(10.0, (({a}) - 100.0) / 10.0) : 0.0)"),
                )
            }
            "powtodb" => {
                let a = input_expr(0, "0.0");
                simple_control(
                    id,
                    &format!("(({a}) > 0.0 ? 100.0 + 10.0 * (log10({a})) : 0.0)"),
                )
            }

            // ── routing (continuous approximations — see README for what's
            //    lost vs. real discrete message/bang semantics) ────────────
            "moses" => {
                let a = input_expr(0, "0.0");
                let thresh = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                let o0 = field(id);
                let o1 = field_o(id, 1);
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {o0};\n  double {o1};\n"),
                    init: format!("  st->{o0} = 0.0; st->{o1} = 0.0;\n"),
                    // Exactly one outlet fires, as in PD.
                    compute: format!(
                        "  {{ double _v = {a}, _t = {thresh};\n    if (_v < _t) {lo}(st, pd_msg_f(_v)); else {hi}(st, pd_msg_f(_v));\n  }}\n",
                        lo = send_fn(id, 0),
                        hi = send_fn(id, 1)
                    ),
                }
            }
            "spigot" => {
                let _ = input_expr(0, "0.0");
                let gate = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    // Gate, not converter: pass the message through untouched.
                    compute: format!("  if (({gate}) != 0.0) {}(st, m);\n", send_fn(id, 0)),
                }
            }
            "sel" | "select" => {
                let a = input_expr(0, "0.0");
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
                // A match bangs that one outlet and nothing else; a
                // non-match passes the value out the rightmost outlet.
                let mut compute = format!("  double _v = {a};\n");
                let mut state_fields = String::new();
                let mut init = String::new();
                for (i, t) in targets.iter().enumerate() {
                    let fld = field_o(id, i as u32);
                    state_fields.push_str(&format!("  double {fld};\n"));
                    init.push_str(&format!("  st->{fld} = 0.0;\n"));
                    compute.push_str(&format!(
                        "  if (_v == ({t})) {{ {}(st, pd_msg_bang()); return; }}\n",
                        send_fn(id, i as u32)
                    ));
                }
                let pass_fld = field_o(id, targets.len() as u32);
                state_fields.push_str(&format!("  double {pass_fld};\n"));
                init.push_str(&format!("  st->{pass_fld} = 0.0;\n"));
                compute.push_str(&format!(
                    "  {}(st, pd_msg_f(_v));\n",
                    send_fn(id, targets.len() as u32)
                ));
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                }
            }
            "change" => {
                // Real PD semantics now that output is message-driven: only
                // emits when the value actually differs from the last one sent.
                let a = input_expr(0, "0.0");
                let f = field(id);
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {f};\n  double {f}_prev;\n  int {f}_seen;\n"),
                    init: format!("  st->{f} = 0.0; st->{f}_prev = 0.0; st->{f}_seen = 0;\n"),
                    compute: format!(
                        "  {{ double _v = {a};\n    if (!st->{f}_seen || _v != st->{f}_prev) {{ st->{f}_seen = 1; st->{f}_prev = _v; {}(st, pd_msg_f(_v)); }}\n  }}\n",
                        send_fn(id, 0)
                    ),
                }
            }
            // Like sel/select, but passes the matched *value* through (not
            // just a 1.0 flag), and passes non-matches to the last outlet —
            // matches real PD route semantics for numeric matching.
            "route" => {
                let a = input_expr(0, "0.0");
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
                // Real PD route: the incoming message's FIRST atom selects an
                // outlet, and what's emitted there is the REMAINDER of the
                // message (`1 60 100` into `[route 1 2]` sends `60 100` out
                // outlet 0). Only that one outlet fires — every other branch
                // keeps whatever it last latched, which is precisely what a
                // recompute pass could never express.
                let _ = &a;
                let mut compute = String::from(
                    "  PdMsg _rest;\n  _rest.len = m.len > 0 ? m.len - 1 : 0;\n  for (int _i = 0; _i < _rest.len && _i + 1 < PD_MSG_MAX; _i++) _rest.a[_i] = m.a[_i + 1];\n  if (_rest.len == 0) _rest = pd_msg_bang();\n  double _sel = m.len > 0 ? m.a[0] : 0.0;\n",
                );
                let mut state_fields = String::new();
                let mut init = String::new();
                for (i, t) in targets.iter().enumerate() {
                    let fld = field_o(id, i as u32);
                    state_fields.push_str(&format!("  double {fld};\n"));
                    init.push_str(&format!("  st->{fld} = 0.0;\n"));
                    compute.push_str(&format!(
                        "  if (_sel == ({t})) {{ {}(st, _rest); return; }}\n",
                        send_fn(id, i as u32)
                    ));
                }
                let pass_fld = field_o(id, targets.len() as u32);
                state_fields.push_str(&format!("  double {pass_fld};\n"));
                init.push_str(&format!("  st->{pass_fld} = 0.0;\n"));
                compute.push_str(&format!(
                    "  {}(st, m);\n",
                    send_fn(id, targets.len() as u32)
                ));
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                }
            }
            // Numeric fan-out: every outlet mirrors the input. Real PD's
            // trigger also converts per-outlet type (bang/symbol/float) and
            // fires right-to-left for ordered side effects — neither
            // applies to our scalar, continuously-recomputed model.
            "trigger" | "t" => {
                let a = input_expr(0, "0.0");
                let n = args.len().max(1);
                let mut state_fields = String::new();
                let mut init = String::new();
                let mut compute = format!("  double _v = {a};\n");
                for i in 0..n {
                    let fld = field_o(id, i as u32);
                    state_fields.push_str(&format!("  double {fld};\n"));
                    init.push_str(&format!("  st->{fld} = 0.0;\n"));
                }
                // Right to left, so a "t b f" latches the float before the
                // bang fires — the ordering the whole idiom depends on.
                for i in (0..n).rev() {
                    let is_bang = matches!(args.get(i), Some(Token::Symbol(sy)) if sy == "b" || sy == "bang");
                    let msg = if is_bang { "pd_msg_bang()".to_string() } else { "pd_msg_f(_v)".to_string() };
                    compute.push_str(&format!("  {}(st, {msg});\n", send_fn(id, i as u32)));
                }
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                }
            }
            // Control-rate ramp toward the input value over a creation-arg
            // time (ms), updated once per control recompute (block rate)
            // rather than per-sample — see line~ for the signal-rate version.
            "line" => {
                let target = input_expr(0, "0.0");
                let ramp_ms = if args.is_empty() { 20.0 } else { farg(0) };
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!(
                        "  {{ double _c = 1.0 - exp(-1000.0 / (({ramp_ms}) > 0.001 ? ({ramp_ms}) : 0.001) / st->sample_rate * 64.0); if (_c > 1.0) _c = 1.0; st->{f} += _c * (({target}) - st->{f}); }}\n"
                    ),
                }
            }
            // Draws a new value per incoming bang/message — real PD behaviour
            // now that the graph is message-driven.
            "random" => {
                let range = if args.is_empty() { 1.0 } else { farg(0) };
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  unsigned int {f}_seed;\n  double {f};\n"),
                    init: format!("  st->{f}_seed = 12345u + {id}u; st->{f} = 0.0;\n"),
                    compute: format!(
                        "  {{ st->{f}_seed = st->{f}_seed * 1103515245u + 12345u; double _r = (double)((st->{f}_seed >> 8) & 0x7fffff) / (double)0x800000; {}(st, pd_msg_f(floor(_r * ({range})))); }}\n",
                        send_fn(id, 0)
                    ),
                }
            }
            "pack" => {
                let n = args.len().max(2);
                let mut state_fields = String::new();
                let mut init = String::new();
                let mut compute = String::new();
                for i in 0..n {
                    let fld = field_o(id, i as u32);
                    let default = format!(
                        "{}",
                        args.get(i)
                            .and_then(|t| if let Token::Float(v) = t {
                                Some(*v)
                            } else {
                                None
                            })
                            .unwrap_or(0.0)
                    );
                    let v = input_expr(i as u32, &default);
                    state_fields.push_str(&format!("  double {fld};\n"));
                    init.push_str(&format!("  st->{fld} = 0.0;\n"));
                    compute.push_str(&format!("  _o.a[{i}] = {v};\n"));
                }
                // One list out the single outlet, exactly like PD's pack.
                let compute = format!(
                    "  PdMsg _o;\n  _o.len = {n};\n{compute}  {}(st, _o);\n",
                    send_fn(id, 0)
                );
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                }
            }
            "unpack" => {
                // Real list splitting: element i goes out outlet i, right to
                // left. The inbound list already landed in this node's inlet
                // latches via the hot-inlet distribution rule.
                let n = args.len().max(2);
                for i in 0..n {
                    let _ = input_expr(i as u32, "0.0");
                }
                let mut state_fields = String::new();
                let mut init = String::new();
                let mut compute = String::new();
                for i in 0..n {
                    let fld = field_o(id, i as u32);
                    state_fields.push_str(&format!("  double {fld};\n"));
                    init.push_str(&format!("  st->{fld} = 0.0;\n"));
                }
                for i in (0..n).rev() {
                    compute.push_str(&format!(
                        "  {}(st, pd_msg_f(st->{}));\n",
                        send_fn(id, i as u32),
                        inlet_field(id, i as u32)
                    ));
                }
                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                }
            }
            "swap" => {
                let default_a = format!("{}", if args.is_empty() { 0.0 } else { farg(0) });
                let default_b = format!("{}", if args.len() < 2 { 0.0 } else { farg(1) });
                let a = input_expr(0, &default_a);
                let b = input_expr(1, &default_b);
                let o0 = field(id);
                let o1 = field_o(id, 1);
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {o0};\n  double {o1};\n"),
                    init: format!("  st->{o0} = 0.0; st->{o1} = 0.0;\n"),
                    compute: format!(
                        "  {{ double _a = {a}, _b = {b};\n    {o1f}(st, pd_msg_f(_a));\n    {o0f}(st, pd_msg_f(_b));\n  }}\n",
                        o1f = send_fn(id, 1),
                        o0f = send_fn(id, 0)
                    ),
                }
            }
            // Pure source: pd_process bangs its outlet once, the first time
            // audio is rendered (see `loadbang_stmts`).
            "loadbang" => EmittedNode {
                domain: Domain::Control,
                state_fields: format!("  double {f};\n"),
                init: format!("  st->{f} = 0.0;\n"),
                compute: String::new(),
            },

            // ── poly: N-voice allocator, `poly <voices=16> <steal=0>` ───────
            // Watches inlet 0 (pitch) paired with inlet 1 (velocity, 0 =
            // note-off) for a genuine PD note event, same edge-detection
            // idiom as `change` (a real event is "the pair differs from
            // what it was last recompute", since notein's fields are
            // otherwise sample-and-held between pd_note_on/off calls).
            //
            // Real PD's `poly` outputs a single (voice#, pitch, velocity)
            // scalar triple meant to be fanned out with `[route 1 2 3 ...]`
            // — but this codegen has no list-typed outlets for `route` to
            // dispatch through (see README: symbol/list-typed routing isn't
            // supported), so that idiom doesn't translate. Instead, each
            // voice gets its own dedicated, continuously-held outlet triple:
            // outlet 3*i / 3*i+1 / 3*i+2 = pitch / gate / velocity of voice
            // i (0-based) — wire each one straight into its own `[voice]`
            // abstraction instance, no `route` needed.
            //
            // Retriggering an already-held pitch reuses its existing voice.
            // When every voice is busy: steal=0 silently drops the extra
            // note (no overflow outlet — same list-typed-output limitation
            // as above); steal!=0 steals whichever voice has been held
            // longest.
            // Real PD `poly`: three outlets — voice number, pitch, velocity —
            // fired right to left, exactly as PD does. Downstream you pack
            // them and dispatch with `[route 1 2 ... N]`; that idiom works
            // correctly now that only the matching route outlet propagates
            // and every other voice keeps its latched values.
            "poly" => {
                let n_voices = (if args.is_empty() { 16.0 } else { farg(0) }).max(1.0) as usize;
                let steal = args.len() > 1 && farg(1) != 0.0;
                let pitch = input_expr(0, "0.0");
                let vel = input_expr(1, "0.0");
                let vnote = format!("{f}_vnote");
                let vage = format!("{f}_vage");
                let age = format!("{f}_age");

                let state_fields = format!(
                    "  double {f};\n  double {o1};\n  double {o2};\n  double {vnote}[{n_voices}];\n  double {vage}[{n_voices}];\n  double {age};\n",
                    o1 = field_o(id, 1),
                    o2 = field_o(id, 2)
                );
                let init = format!(
                    "  st->{f} = 0.0; st->{o1} = 0.0; st->{o2} = 0.0;\n  for (int _i = 0; _i < {n_voices}; _i++) {{ st->{vnote}[_i] = -1.0; st->{vage}[_i] = 0.0; }}\n  st->{age} = 0.0;\n",
                    o1 = field_o(id, 1),
                    o2 = field_o(id, 2)
                );

                let steal_block = if steal {
                    format!(
                        "      if (_slot < 0) {{\n        int _oldest = 0; double _minage = st->{vage}[0];\n        for (int _i = 1; _i < {n_voices}; _i++) if (st->{vage}[_i] < _minage) {{ _minage = st->{vage}[_i]; _oldest = _i; }}\n        _slot = _oldest;\n      }}\n"
                    )
                } else {
                    String::new()
                };

                let compute = format!(
                    "  {{\n    double _pitch = {pitch};\n    double _vel = {vel};\n    int _slot = -1;\n    if (_vel != 0.0) {{\n      for (int _i = 0; _i < {n_voices}; _i++) if (st->{vnote}[_i] == _pitch) {{ _slot = _i; break; }}\n      if (_slot < 0) for (int _i = 0; _i < {n_voices}; _i++) if (st->{vnote}[_i] < 0.0) {{ _slot = _i; break; }}\n{steal_block}      if (_slot >= 0) {{\n        st->{age} += 1.0;\n        st->{vnote}[_slot] = _pitch;\n        st->{vage}[_slot] = st->{age};\n      }}\n    }} else {{\n      for (int _i = 0; _i < {n_voices}; _i++) if (st->{vnote}[_i] == _pitch) {{ _slot = _i; break; }}\n      if (_slot >= 0) st->{vnote}[_slot] = -1.0;\n    }}\n    if (_slot >= 0) {{\n      {send_vel}(st, pd_msg_f(_vel));\n      {send_pitch}(st, pd_msg_f(_pitch));\n      {send_voice}(st, pd_msg_f((double)(_slot + 1)));\n    }}\n  }}\n",
                    send_vel = send_fn(id, 2),
                    send_pitch = send_fn(id, 1),
                    send_voice = send_fn(id, 0)
                );

                EmittedNode {
                    domain: Domain::Control,
                    state_fields,
                    init,
                    compute,
                }
            }

            // ── MIDI note input (real events, wired by runtime-shim.c via
            //    pd_note_on/pd_note_off — NOT recomputed per control pass) ──
            "notein" => EmittedNode {
                domain: Domain::Control,
                state_fields: format!(
                    "  double {f};\n  double {o1};\n  double {o2};\n",
                    f = f,
                    o1 = field_o(id, 1),
                    o2 = field_o(id, 2)
                ),
                init: format!(
                    "  st->{f} = 0.0; st->{o1} = 0.0; st->{o2} = 1.0;\n",
                    f = f,
                    o1 = field_o(id, 1),
                    o2 = field_o(id, 2)
                ),
                compute: String::new(), // pushed by pd_note_on/off
            },

            // ── MIDI CC / bend / touch / program (real events, written
            //    externally by pd_control_change/pd_pitch_bend/pd_touch/
            //    pd_program_change) ─────────────────────────────────────────
            "ctlin" => {
                let has_filter = args.first().is_some_and(|t| matches!(t, Token::Float(_)));
                let o1 = field_o(id, 1);
                if has_filter {
                    EmittedNode {
                        domain: Domain::Control,
                        state_fields: format!("  double {f};\n"),
                        init: format!("  st->{f} = 0.0;\n"),
                        compute: String::new(),
                    }
                } else {
                    EmittedNode {
                        domain: Domain::Control,
                        state_fields: format!("  double {f};\n  double {o1};\n"),
                        init: format!("  st->{f} = 0.0; st->{o1} = 0.0;\n"),
                        compute: String::new(),
                    }
                }
            }
            "bendin" | "touchin" | "pgmin" => EmittedNode {
                domain: Domain::Control,
                state_fields: format!("  double {f};\n"),
                init: format!("  st->{f} = 0.0;\n"),
                compute: String::new(),
            },

            // ── dac~ / adc~ ──────────────────────────────────────────────────
            "dac~" => {
                dac_l.push(input_expr(0, "0.0"));
                dac_r.push(input_expr(1, "0.0"));
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: String::new(),
                    init: String::new(),
                    compute: String::new(),
                }
            }
            "adc~" => EmittedNode {
                domain: Domain::Signal,
                state_fields: format!(
                    "  double {f};\n  double {o1};\n",
                    f = f,
                    o1 = field_o(id, 1)
                ),
                init: format!(
                    "  st->{f} = 0.0; st->{o1} = 0.0;\n",
                    f = f,
                    o1 = field_o(id, 1)
                ),
                compute: format!(
                    "  st->{f} = (double)in_l_sample; st->{o1} = (double)in_r_sample;\n",
                    f = f,
                    o1 = field_o(id, 1)
                ),
            },

            // ── signal-rate oscillators / math / filters ────────────────────
            "osc~" => {
                let freq = input_expr(
                    0,
                    &format!("{}", if args.is_empty() { 440.0 } else { farg(0) }),
                );
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f}_phase;\n  double {f};\n"),
                    init: format!("  st->{f}_phase = 0.0; st->{f} = 0.0;\n"),
                    compute: format!(
                        "  st->{f} = sin(st->{f}_phase * 6.283185307179586);\n  st->{f}_phase += ({freq}) / st->sample_rate;\n  if (st->{f}_phase >= 1.0) st->{f}_phase -= 1.0;\n  if (st->{f}_phase < 0.0) st->{f}_phase += 1.0;\n"
                    ),
                }
            }
            "phasor~" => {
                let freq = input_expr(
                    0,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!(
                        "  st->{f} += ({freq}) / st->sample_rate;\n  if (st->{f} >= 1.0) st->{f} -= 1.0;\n  if (st->{f} < 0.0) st->{f} += 1.0;\n"
                    ),
                }
            }
            "noise~" => EmittedNode {
                domain: Domain::Signal,
                state_fields: format!("  unsigned int {f}_seed;\n  double {f};\n"),
                init: format!("  st->{f}_seed = 22222 + {id}u; st->{f} = 0.0;\n"),
                compute: format!(
                    "  st->{f}_seed = st->{f}_seed * 1103515245u + 12345u;\n  st->{f} = ((double)(st->{f}_seed & 0x7fffffff) / (double)0x7fffffff) * 2.0 - 1.0;\n"
                ),
            },
            "sig~" => {
                let v = input_expr(
                    0,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!("  st->{f} = {v};\n"),
                }
            }
            "+~" | "-~" | "*~" | "/~" => {
                let a = input_expr(0, "0.0");
                let b = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                let op = match name {
                    "+~" => format!("({a}) + ({b})"),
                    "-~" => format!("({a}) - ({b})"),
                    "*~" => format!("({a}) * ({b})"),
                    "/~" => format!("(({b}) != 0.0 ? ({a}) / ({b}) : 0.0)"),
                    _ => unreachable!(),
                };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!("  st->{f} = {op};\n"),
                }
            }
            "lop~" | "hip~" => {
                let inp = input_expr(0, "0.0");
                let cutoff = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                let coef =
                    format!("(1.0 - exp(-6.283185307179586 * ({cutoff}) / st->sample_rate))");
                let compute = if name == "lop~" {
                    format!(
                        "  {{ double _c = {coef}; st->{f} = st->{f} + _c * (({inp}) - st->{f}); }}\n"
                    )
                } else {
                    format!(
                        "  {{ double _c = {coef}; double _lp = st->{f}_lp + _c * (({inp}) - st->{f}_lp); st->{f}_lp = _lp; st->{f} = ({inp}) - _lp; }}\n"
                    )
                };
                let extra_state = if name == "hip~" {
                    format!("  double {f}_lp;\n")
                } else {
                    String::new()
                };
                let extra_init = if name == "hip~" {
                    format!("  st->{f}_lp = 0.0;\n")
                } else {
                    String::new()
                };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n{extra_state}"),
                    init: format!("  st->{f} = 0.0;\n{extra_init}"),
                    compute,
                }
            }
            // Resonant bandpass, approximating PD's vcf~ (state-variable
            // filter form) — 2 outlets: bandpass (o0) and a lowpass-ish
            // companion (o1), driven by a signal or constant center freq
            // and a creation-arg Q.
            "vcf~" => {
                let inp = input_expr(0, "0.0");
                let center = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                let q = if args.len() > 1 { farg(1) } else { 1.0 };
                let o1 = field_o(id, 1);
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!(
                        "  double {f};\n  double {o1};\n  double {f}_bp;\n  double {f}_lp;\n"
                    ),
                    init: format!(
                        "  st->{f} = 0.0; st->{o1} = 0.0; st->{f}_bp = 0.0; st->{f}_lp = 0.0;\n"
                    ),
                    compute: format!(
                        "  {{\n    double _f = 2.0 * sin(3.141592653589793 * ({center}) / st->sample_rate);\n    if (_f > 1.0) _f = 1.0;\n    double _q = ({q}) > 0.01 ? ({q}) : 0.01;\n    double _hp = ({inp}) - st->{f}_lp - (1.0 / _q) * st->{f}_bp;\n    st->{f}_bp += _f * _hp;\n    st->{f}_lp += _f * st->{f}_bp;\n    st->{f} = st->{f}_bp;\n    st->{o1} = st->{f}_lp;\n  }}\n"
                    ),
                }
            }
            // Resonant bandpass with unity-ish peak gain (Q-compensated),
            // 1 outlet — same SVF core as vcf~.
            "bp~" => {
                let inp = input_expr(0, "0.0");
                let center = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                let q = if args.len() > 1 { farg(1) } else { 1.0 };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n  double {f}_bp;\n  double {f}_lp;\n"),
                    init: format!("  st->{f} = 0.0; st->{f}_bp = 0.0; st->{f}_lp = 0.0;\n"),
                    compute: format!(
                        "  {{\n    double _f = 2.0 * sin(3.141592653589793 * ({center}) / st->sample_rate);\n    if (_f > 1.0) _f = 1.0;\n    double _q = ({q}) > 0.01 ? ({q}) : 0.01;\n    double _hp = ({inp}) - st->{f}_lp - (1.0 / _q) * st->{f}_bp;\n    st->{f}_bp += _f * _hp;\n    st->{f}_lp += _f * st->{f}_bp;\n    st->{f} = st->{f}_bp * _q;\n  }}\n"
                    ),
                }
            }
            // Direct-form biquad; coefficients are baked in from creation
            // args (b0 b1 b2 a1 a2) — PD drives them via a list message to
            // a single inlet, which our scalar-signal model can't carry, so
            // they're compile-time constants here rather than runtime-settable.
            "biquad~" => {
                let inp = input_expr(0, "0.0");
                let b0 = farg(0);
                let b1 = farg(1);
                let b2 = farg(2);
                let a1 = farg(3);
                let a2 = farg(4);
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n  double {f}_w1;\n  double {f}_w2;\n"),
                    init: format!("  st->{f} = 0.0; st->{f}_w1 = 0.0; st->{f}_w2 = 0.0;\n"),
                    compute: format!(
                        "  {{\n    double _x = {inp};\n    double _y = {b0} * _x + st->{f}_w1;\n    st->{f}_w1 = {b1} * _x - {a1} * _y + st->{f}_w2;\n    st->{f}_w2 = {b2} * _x - {a2} * _y;\n    st->{f} = _y;\n  }}\n"
                    ),
                }
            }
            "rzero~" => {
                let inp = input_expr(0, "0.0");
                let a = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n  double {f}_px;\n"),
                    init: format!("  st->{f} = 0.0; st->{f}_px = 0.0;\n"),
                    compute: format!(
                        "  {{ double _x = {inp}; st->{f} = _x - ({a}) * st->{f}_px; st->{f}_px = _x; }}\n"
                    ),
                }
            }
            "rpole~" => {
                let inp = input_expr(0, "0.0");
                let a = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    // RHS reads st->f's OLD value before the assignment
                    // completes — that's exactly y[n-1], no extra state needed.
                    compute: format!("  st->{f} = ({inp}) + ({a}) * st->{f};\n"),
                }
            }

            // ── signal-rate unary math ───────────────────────────────────────
            "abs~" => {
                let a = input_expr(0, "0.0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!("  st->{f} = fabs({a});\n"),
                }
            }
            "sqrt~" => {
                let a = input_expr(0, "0.0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!("  st->{f} = (({a}) > 0.0 ? sqrt({a}) : 0.0);\n"),
                }
            }
            "wrap~" => {
                let a = input_expr(0, "0.0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!("  st->{f} = (({a}) - floor({a}));\n"),
                }
            }
            "clip~" => {
                let a = input_expr(0, "0.0");
                let lo = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                let hi = input_expr(
                    2,
                    &format!("{}", if args.len() < 2 { 1.0 } else { farg(1) }),
                );
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!(
                        "  st->{f} = (({a}) < ({lo}) ? ({lo}) : (({a}) > ({hi}) ? ({hi}) : ({a})));\n"
                    ),
                }
            }

            // ── envelope / sample-hold — snapshot~ and samphold~ read a
            //    signal input directly (whatever pd_signal_step last wrote
            //    to that field), so they work from either domain. ─────────
            "line~" | "vline~" => {
                // Continuous one-pole ramp toward inlet 0's value over a
                // creation-arg ramp time (ms) — an honest approximation of
                // PD's target/time message pair (see README): not a
                // discrete-message-triggered multi-segment ramp. vline~'s
                // extra delay-before-ramp-starts message field isn't
                // representable here either, so it's treated identically
                // to line~.
                let target = input_expr(0, "0.0");
                let ramp_ms = if args.is_empty() { 20.0 } else { farg(0) };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!(
                        "  {{ double _c = 1.0 - exp(-1000.0 / (({ramp_ms}) > 0.001 ? ({ramp_ms}) : 0.001) / st->sample_rate); st->{f} += _c * (({target}) - st->{f}); }}\n"
                    ),
                }
            }
            "samphold~" => {
                let a = input_expr(0, "0.0");
                let trig = input_expr(1, "0.0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n  double {f}_ptrig;\n"),
                    init: format!("  st->{f} = 0.0; st->{f}_ptrig = 0.0;\n"),
                    compute: format!(
                        "  {{ double _t = {trig}; if (_t < st->{f}_ptrig) st->{f} = {a}; st->{f}_ptrig = _t; }}\n"
                    ),
                }
            }
            "snapshot~" => {
                // Approximation: continuously mirrors the input signal's
                // current sample rather than capturing only on a bang.
                let a = input_expr(0, "0.0");
                simple_control(id, &a)
            }
            "threshold~" => {
                // Approximation: a continuous gate (1.0 above threshold, 0.0
                // below) rather than a one-shot bang on crossing.
                let a = input_expr(0, "0.0");
                let thresh = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!("  st->{f} = (({a}) >= ({thresh}) ? 1.0 : 0.0);\n"),
                }
            }
            "env~" => {
                // Continuous one-pole RMS-to-dB follower (real env~ reports
                // periodically at an analysis-window rate; this updates
                // every sample instead, which is strictly higher-resolution).
                let a = input_expr(0, "0.0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n  double {f}_ms;\n"),
                    init: format!("  st->{f} = 0.0; st->{f}_ms = 0.0;\n"),
                    compute: format!(
                        "  {{ double _c = 1.0 - exp(-6.283185307179586 * 20.0 / st->sample_rate); double _x = {a}; st->{f}_ms += _c * (_x * _x - st->{f}_ms); double _rms = sqrt(st->{f}_ms); st->{f} = (_rms > 0.0 ? 100.0 + 20.0 * log10(_rms) : 0.0); }}\n"
                    ),
                }
            }

            // ── scheduler: metro / delay / pipe / timer ─────────────────────
            // These are control-domain objects in real PD, but need
            // sample-accurate timing, so their compute lives in
            // pd_signal_step (forced Domain::Signal) even though their
            // *output* is meant to be read as a control value — any
            // control-domain consumer just reads whatever pd_signal_step
            // last wrote, same as reading any other cross-domain field.
            //
            // None of these have a real "bang" input in our continuous
            // model, so each is driven by *edges* on inlet 0's value
            // instead: metro runs while it's nonzero (typically a toggle),
            // delay/pipe/timer (re)trigger on a rising edge or a value
            // change — see README for the full discrete-message caveat.
            "metro" => {
                let gate = input_expr(0, "0.0");
                let period_ms = if args.is_empty() { 200.0 } else { farg(0) };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n  double {f}_ctr;\n  double {f}_pg;\n"),
                    init: format!("  st->{f} = 0.0; st->{f}_ctr = 0.0; st->{f}_pg = 0.0;\n"),
                    compute: format!(
                        "  {{\n    double _gate = ({gate}) != 0.0 ? 1.0 : 0.0;\n    double _bang = 0.0;\n    double _period = (({period_ms}) / 1000.0) * st->sample_rate;\n    if (_period < 1.0) _period = 1.0;\n    if (_gate != 0.0) {{\n      if (st->{f}_pg == 0.0) {{ _bang = 1.0; st->{f}_ctr = 0.0; }}\n      else {{\n        st->{f}_ctr += 1.0;\n        if (st->{f}_ctr >= _period) {{ _bang = 1.0; st->{f}_ctr = 0.0; }}\n      }}\n    }} else {{ st->{f}_ctr = 0.0; }}\n    st->{f} = _bang;\n    st->{f}_pg = _gate;\n    if (_bang != 0.0) {}(st, pd_msg_bang());\n  }}\n",
                        send_fn(id, 0)
                    ),
                }
            }
            "delay" | "del" => {
                let gate = input_expr(0, "0.0");
                let delay_ms = if args.is_empty() { 0.0 } else { farg(0) };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!(
                        "  double {f};\n  double {f}_ctr;\n  double {f}_pg;\n  double {f}_armed;\n"
                    ),
                    init: format!(
                        "  st->{f} = 0.0; st->{f}_ctr = 0.0; st->{f}_pg = 0.0; st->{f}_armed = 0.0;\n"
                    ),
                    compute: format!(
                        "  {{\n    double _gate = ({gate}) != 0.0 ? 1.0 : 0.0;\n    double _bang = 0.0;\n    double _delay = (({delay_ms}) / 1000.0) * st->sample_rate;\n    if (_gate != 0.0 && st->{f}_pg == 0.0) {{ st->{f}_armed = 1.0; st->{f}_ctr = 0.0; }}\n    if (st->{f}_armed != 0.0) {{\n      st->{f}_ctr += 1.0;\n      if (st->{f}_ctr >= _delay) {{ _bang = 1.0; st->{f}_armed = 0.0; }}\n    }}\n    st->{f} = _bang;\n    st->{f}_pg = _gate;\n    if (_bang != 0.0) {}(st, pd_msg_bang());\n  }}\n",
                        send_fn(id, 0)
                    ),
                }
            }
            "pipe" => {
                let v = input_expr(0, "0.0");
                let delay_ms = if args.is_empty() { 0.0 } else { farg(0) };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!(
                        "  double {f};\n  double {f}_ctr;\n  double {f}_pv;\n  double {f}_val;\n  double {f}_armed;\n"
                    ),
                    init: format!(
                        "  st->{f} = 0.0; st->{f}_ctr = 0.0; st->{f}_pv = 0.0; st->{f}_val = 0.0; st->{f}_armed = 0.0;\n"
                    ),
                    compute: format!(
                        "  {{\n    double _v = {v};\n    double _out = 0.0;\n    int _fired = 0;\n    double _delay = (({delay_ms}) / 1000.0) * st->sample_rate;\n    if (_v != st->{f}_pv) {{ st->{f}_armed = 1.0; st->{f}_ctr = 0.0; st->{f}_val = _v; }}\n    if (st->{f}_armed != 0.0) {{\n      st->{f}_ctr += 1.0;\n      if (st->{f}_ctr >= _delay) {{ _out = st->{f}_val; st->{f}_armed = 0.0; _fired = 1; }}\n    }}\n    st->{f} = _out;\n    st->{f}_pv = _v;\n    if (_fired) {}(st, pd_msg_f(_out));\n  }}\n",
                        send_fn(id, 0)
                    ),
                }
            }
            "timer" => {
                let gate = input_expr(0, "0.0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n  double {f}_ctr;\n  double {f}_pg;\n"),
                    init: format!("  st->{f} = 0.0; st->{f}_ctr = 0.0; st->{f}_pg = 0.0;\n"),
                    compute: format!(
                        "  {{\n    double _gate = ({gate}) != 0.0 ? 1.0 : 0.0;\n    if (_gate != 0.0 && st->{f}_pg == 0.0) {{ st->{f}_ctr = 0.0; }} else {{ st->{f}_ctr += 1.0; }}\n    st->{f} = (st->{f}_ctr / st->sample_rate) * 1000.0;\n    st->{f}_pg = _gate;\n  }}\n"
                    ),
                }
            }

            // ── delay lines (shared buffer keyed by name, not node id) ──────
            "delwrite~" => {
                let Some(Token::Symbol(dname)) = args.first() else {
                    self.warnings
                        .push("delwrite~ needs a name argument — emitted as a zero stub".into());
                    return EmittedNode {
                        domain: Domain::Signal,
                        state_fields: String::new(),
                        init: String::new(),
                        compute: String::new(),
                    };
                };
                let c = sanitize_c_name(dname);
                let inp = input_expr(0, "0.0");
                let first_time = self.declared_delay_lines.insert(dname.clone());
                let maxms = delay_lines.get(dname).copied().unwrap_or(1000.0);
                let (state_fields, init, destroy) = if first_time {
                    (
                        format!(
                            "  double* dlbuf_{c};\n  int32_t dlsize_{c};\n  int32_t dlwidx_{c};\n"
                        ),
                        format!(
                            "  st->dlsize_{c} = (int32_t)(({maxms} / 1000.0) * st->sample_rate) + 1;\n  if (st->dlsize_{c} < 1) st->dlsize_{c} = 1;\n  st->dlbuf_{c} = (double*)calloc((size_t)st->dlsize_{c}, sizeof(double));\n  st->dlwidx_{c} = 0;\n"
                        ),
                        format!("  free(st->dlbuf_{c});\n"),
                    )
                } else {
                    (String::new(), String::new(), String::new())
                };
                destroy_stmts.push_str(&destroy);
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields,
                    init,
                    compute: format!(
                        "  st->dlbuf_{c}[st->dlwidx_{c}] = {inp};\n  st->dlwidx_{c} = (st->dlwidx_{c} + 1) % st->dlsize_{c};\n"
                    ),
                }
            }
            "delread~" | "vd~" => {
                let Some(Token::Symbol(dname)) = args.first() else {
                    self.warnings.push(format!(
                        "{name} needs a name argument — emitted as a zero stub"
                    ));
                    return EmittedNode {
                        domain: Domain::Signal,
                        state_fields: format!("  double {f};\n"),
                        init: format!("  st->{f} = 0.0;\n"),
                        compute: String::new(),
                    };
                };
                if !delay_lines.contains_key(dname) {
                    self.warnings.push(format!("{name} {dname}: no delwrite~ {dname} in this patch — emitted as a zero stub"));
                    return EmittedNode {
                        domain: Domain::Signal,
                        state_fields: format!("  double {f};\n"),
                        init: format!("  st->{f} = 0.0;\n"),
                        compute: String::new(),
                    };
                }
                let c = sanitize_c_name(dname);
                let delay_default = if name == "vd~" { 0.0 } else { farg(1) };
                let delay_expr = if name == "vd~" {
                    input_expr(0, &format!("{delay_default}"))
                } else {
                    format!("{delay_default}")
                };
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!(
                        "  {{\n    int32_t _samps = (int32_t)((({delay_expr}) / 1000.0) * st->sample_rate);\n    if (_samps < 0) _samps = 0;\n    if (_samps >= st->dlsize_{c}) _samps = st->dlsize_{c} - 1;\n    int32_t _ridx = st->dlwidx_{c} - 1 - _samps;\n    while (_ridx < 0) _ridx += st->dlsize_{c};\n    st->{f} = st->dlbuf_{c}[_ridx];\n  }}\n"
                    ),
                }
            }

            // ── arrays / tables ──────────────────────────────────────────────
            "tabread~" | "tabread" => {
                let Some(Token::Symbol(aname)) = args.first() else {
                    self.warnings.push(format!(
                        "{name} needs an array-name argument — emitted as a zero stub"
                    ));
                    return EmittedNode {
                        domain: domain_of(name),
                        state_fields: format!("  double {f};\n"),
                        init: format!("  st->{f} = 0.0;\n"),
                        compute: String::new(),
                    };
                };
                let Some(info) = arrays.get(aname) else {
                    self.warnings.push(format!("{name} {aname}: no array named '{aname}' in this patch — emitted as a zero stub"));
                    return EmittedNode {
                        domain: domain_of(name),
                        state_fields: format!("  double {f};\n"),
                        init: format!("  st->{f} = 0.0;\n"),
                        compute: String::new(),
                    };
                };
                let c = sanitize_c_name(aname);
                let idx = input_expr(0, "0.0");
                EmittedNode {
                    domain: domain_of(name),
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: format!(
                        "  {{\n    int32_t _i = (int32_t)({idx});\n    if (_i < 0) _i = 0;\n    if (_i >= {size}) _i = {size} - 1;\n    st->{f} = st->arr_{c}[_i];\n  }}\n",
                        size = info.size
                    ),
                }
            }
            // Continuously overwrites the array in a circular fashion —
            // an honest approximation of PD's bang-armed one-shot record
            // (real tabwrite~ starts on a bang, records once until full,
            // then stops; we have no discrete bang trigger to key off of).
            "tabwrite~" => {
                let Some(Token::Symbol(aname)) = args.first() else {
                    self.warnings.push(
                        "tabwrite~ needs an array-name argument — emitted as a zero stub".into(),
                    );
                    return EmittedNode {
                        domain: Domain::Signal,
                        state_fields: String::new(),
                        init: String::new(),
                        compute: String::new(),
                    };
                };
                let Some(info) = arrays.get(aname) else {
                    self.warnings.push(format!("tabwrite~ {aname}: no array named '{aname}' in this patch — emitted as a zero stub"));
                    return EmittedNode {
                        domain: Domain::Signal,
                        state_fields: String::new(),
                        init: String::new(),
                        compute: String::new(),
                    };
                };
                let c = sanitize_c_name(aname);
                let inp = input_expr(0, "0.0");
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  int32_t {f}_widx;\n"),
                    init: format!("  st->{f}_widx = 0;\n"),
                    compute: format!(
                        "  st->arr_{c}[st->{f}_widx] = {inp};\n  st->{f}_widx = (st->{f}_widx + 1) % {size};\n",
                        size = info.size
                    ),
                }
            }
            // Wavetable oscillator; linearly interpolated (real tabosc4~ is
            // 4-point/cubic — documented simplification).
            "tabosc4~" => {
                let Some(Token::Symbol(aname)) = args.first() else {
                    self.warnings.push(
                        "tabosc4~ needs an array-name argument — emitted as a zero stub".into(),
                    );
                    return EmittedNode {
                        domain: Domain::Signal,
                        state_fields: format!("  double {f};\n"),
                        init: format!("  st->{f} = 0.0;\n"),
                        compute: String::new(),
                    };
                };
                let Some(info) = arrays.get(aname) else {
                    self.warnings.push(format!("tabosc4~ {aname}: no array named '{aname}' in this patch — emitted as a zero stub"));
                    return EmittedNode {
                        domain: Domain::Signal,
                        state_fields: format!("  double {f};\n"),
                        init: format!("  st->{f} = 0.0;\n"),
                        compute: String::new(),
                    };
                };
                let c = sanitize_c_name(aname);
                let freq = input_expr(
                    0,
                    &format!("{}", if args.len() > 1 { farg(1) } else { 0.0 }),
                );
                EmittedNode {
                    domain: Domain::Signal,
                    state_fields: format!("  double {f};\n  double {f}_phase;\n"),
                    init: format!("  st->{f} = 0.0; st->{f}_phase = 0.0;\n"),
                    compute: format!(
                        "  {{\n    double _pos = st->{f}_phase * {size};\n    int32_t _i0 = (int32_t)_pos % {size};\n    int32_t _i1 = (_i0 + 1) % {size};\n    double _frac = _pos - floor(_pos);\n    st->{f} = st->arr_{c}[_i0] * (1.0 - _frac) + st->arr_{c}[_i1] * _frac;\n    st->{f}_phase += ({freq}) / st->sample_rate;\n    if (st->{f}_phase >= 1.0) st->{f}_phase -= 1.0;\n    if (st->{f}_phase < 0.0) st->{f}_phase += 1.0;\n  }}\n",
                        size = info.size
                    ),
                }
            }

            // ── unsupported: passthrough stub with a warning ────────────────
            other => {
                self.warnings.push(format!(
                    "no wclap codegen for '{other}' — emitted as a zero stub"
                ));
                EmittedNode {
                    domain: domain_of(other),
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = 0.0;\n"),
                    compute: String::new(),
                }
            }
        }
    }
}

/// A stateless single-outlet object with one hot inlet: on a message to
/// inlet 0 it recomputes `expr` (already written in terms of this node's own
/// inlet latch fields by `input_expr`) and sends the result out outlet 0.
/// `compute` here is the *body of the hot-inlet handler*, not a statement in
/// a global recompute pass — see `render_output`.
fn simple_control(id: u32, expr: &str) -> EmittedNode {
    let f = field(id);
    EmittedNode {
        domain: Domain::Control,
        state_fields: format!("  double {f};\n"),
        init: format!("  st->{f} = 0.0;\n"),
        compute: format!("  {}(st, pd_msg_f({expr}));\n", send_fn(id, 0)),
    }
}

struct EmittedNode {
    domain: Domain,
    state_fields: String,
    init: String,
    compute: String,
}

struct RenderInput<'a> {
    state_fields: String,
    init_stmts: String,
    destroy_stmts: String,
    signal_stmts: String,
    dispatch: String,
    loadbang_stmts: String,
    dac_l: Vec<String>,
    dac_r: Vec<String>,
    params: &'a [ParamInfo],
    has_audio_in: bool,
    has_note_in: bool,
    notein_id: Option<u32>,
    ctlin_nodes: Vec<(u32, Option<i32>)>,
    bendin_ids: Vec<u32>,
    touchin_ids: Vec<u32>,
    pgmin_ids: Vec<u32>,
}

fn render_output(inp: RenderInput) -> String {
    let mut out = String::new();
    out.push_str("// Generated by pdast2wclap — do not edit\n");
    out.push_str(
        "#include <math.h>\n#include <stdint.h>\n#include <stdlib.h>\n#include <string.h>\n#include \"pd_wclap.h\"\n\n",
    );

    // ── Message runtime ────────────────────────────────────────────────────
    // A PD control message. len==0 is a bang; len==1 a float; len>1 a list.
    // Symbols aren't represented (this codegen is numeric throughout), so a
    // symbol atom degrades to 0.0 — see README.
    out.push_str(
        "#define PD_MSG_MAX 8\n#define PD_MAX_DEPTH 64\n\ntypedef struct { int len; double a[PD_MSG_MAX]; } PdMsg;\n\nstatic inline PdMsg pd_msg_bang(void) { PdMsg m; m.len = 0; return m; }\nstatic inline PdMsg pd_msg_f(double v) { PdMsg m; m.len = 1; m.a[0] = v; return m; }\n\n",
    );

    out.push_str("struct PdState {\n  double sample_rate;\n  int _depth;\n  int _loadbanged;\n");
    out.push_str(&inp.state_fields);
    out.push_str("};\n\n");

    out.push_str(&inp.dispatch);

    out.push_str("PdState* pd_create(double sample_rate) {\n");
    out.push_str("  PdState* st = (PdState*)calloc(1, sizeof(PdState));\n");
    out.push_str("  st->sample_rate = sample_rate > 0 ? sample_rate : 48000.0;\n");
    out.push_str(&inp.init_stmts);
    out.push_str("  return st;\n}\n\n");

    out.push_str("void pd_destroy(PdState* st) {\n");
    out.push_str(&inp.destroy_stmts);
    out.push_str("  free(st);\n}\n\n");

    let dac_l_sum = if inp.dac_l.is_empty() {
        "0.0".to_string()
    } else {
        inp.dac_l.join(" + ")
    };
    let dac_r_sum = if inp.dac_r.is_empty() {
        "0.0".to_string()
    } else {
        inp.dac_r.join(" + ")
    };

    out.push_str(
        "static void pd_signal_step(PdState* st, float in_l_sample, float in_r_sample, float* out_l_sample, float* out_r_sample) {\n",
    );
    out.push_str(&inp.signal_stmts);
    out.push_str(&format!("  *out_l_sample = (float)({dac_l_sum});\n"));
    out.push_str(&format!("  *out_r_sample = (float)({dac_r_sum});\n"));
    out.push_str("}\n\n");

    // The control graph is message-driven: nothing is recomputed per block,
    // it only reacts to messages pushed by the entry points below and by
    // signal-domain objects that fire (metro/delay/pipe/timer).
    out.push_str(
        "void pd_process(PdState* st, const float* in_l, const float* in_r, float* out_l, float* out_r, uint32_t nframes) {\n",
    );
    out.push_str(&inp.loadbang_stmts);
    out.push_str(
        "  for (uint32_t i = 0; i < nframes; i++) {\n    float il = in_l ? in_l[i] : 0.0f;\n    float ir = in_r ? in_r[i] : 0.0f;\n    pd_signal_step(st, il, ir, &out_l[i], &out_r[i]);\n  }\n}\n\n",
    );

    // `notein` fires its outlets RIGHT TO LEFT, exactly as PD does:
    // channel, then velocity, then pitch. Order is load-bearing — downstream
    // objects must already have velocity latched by the time the pitch
    // message (the hot one) triggers them.
    out.push_str("void pd_note_on(PdState* st, int16_t key, double velocity01) {\n");
    match inp.notein_id {
        Some(nid) => {
            out.push_str(&format!(
                "  {chan}(st, pd_msg_f(1.0));\n  {vel}(st, pd_msg_f(velocity01 * 127.0));\n  {pitch}(st, pd_msg_f((double)key));\n",
                chan = send_fn(nid, 2),
                vel = send_fn(nid, 1),
                pitch = send_fn(nid, 0)
            ));
        }
        None => out.push_str("  (void)st; (void)key; (void)velocity01;\n"),
    }
    out.push_str("}\n\n");

    out.push_str("void pd_note_off(PdState* st, int16_t key, double velocity01) {\n");
    match inp.notein_id {
        Some(nid) => {
            out.push_str(&format!(
                "  (void)velocity01;\n  {vel}(st, pd_msg_f(0.0));\n  {pitch}(st, pd_msg_f((double)key));\n",
                vel = send_fn(nid, 1),
                pitch = send_fn(nid, 0)
            ));
        }
        None => out.push_str("  (void)st; (void)key; (void)velocity01;\n"),
    }
    out.push_str("}\n\n");

    out.push_str("void pd_control_change(PdState* st, int32_t controller, double value) {\n");
    if inp.ctlin_nodes.is_empty() {
        out.push_str("  (void)st; (void)controller; (void)value;\n");
    } else {
        for &(nid, filter) in &inp.ctlin_nodes {
            match filter {
                Some(cc) => out.push_str(&format!(
                    "  if (controller == {cc}) {{ {f}(st, pd_msg_f(value)); }}\n",
                    f = send_fn(nid, 0)
                )),
                None => out.push_str(&format!(
                    "  {fo1}(st, pd_msg_f((double)controller));\n  {f}(st, pd_msg_f(value));\n",
                    f = send_fn(nid, 0),
                    fo1 = send_fn(nid, 1)
                )),
            }
        }
    }
    out.push_str("}\n\n");

    out.push_str("void pd_pitch_bend(PdState* st, double value) {\n");
    if inp.bendin_ids.is_empty() {
        out.push_str("  (void)st; (void)value;\n");
    } else {
        for &nid in &inp.bendin_ids {
            out.push_str(&format!("  {f}(st, pd_msg_f(value));\n", f = send_fn(nid, 0)));
        }
    }
    out.push_str("}\n\n");

    out.push_str("void pd_touch(PdState* st, double value) {\n");
    if inp.touchin_ids.is_empty() {
        out.push_str("  (void)st; (void)value;\n");
    } else {
        for &nid in &inp.touchin_ids {
            out.push_str(&format!("  {f}(st, pd_msg_f(value));\n", f = send_fn(nid, 0)));
        }
    }
    out.push_str("}\n\n");

    out.push_str("void pd_program_change(PdState* st, double value) {\n");
    if inp.pgmin_ids.is_empty() {
        out.push_str("  (void)st; (void)value;\n");
    } else {
        for &nid in &inp.pgmin_ids {
            out.push_str(&format!("  {f}(st, pd_msg_f(value));\n", f = send_fn(nid, 0)));
        }
    }
    out.push_str("}\n\n");

    out.push_str("void pd_set_param(PdState* st, int32_t index, double value) {\n");
    out.push_str("  switch (index) {\n");
    for (i, p) in inp.params.iter().enumerate() {
        for &tid in &p.target_ids {
            out.push_str(&format!(
                "    case {i}: {}(st, pd_msg_f(value)); break;\n",
                send_fn(tid, 0)
            ));
        }
        if p.target_ids.is_empty() {
            out.push_str(&format!("    case {i}: break;\n"));
        }
    }
    out.push_str("    default: break;\n  }\n}\n\n");

    out.push_str("double pd_get_param(PdState* st, int32_t index) {\n  switch (index) {\n");
    for (i, p) in inp.params.iter().enumerate() {
        if let Some(&tid) = p.target_ids.first() {
            out.push_str(&format!("    case {i}: return st->{};\n", field(tid)));
        }
    }
    out.push_str("    default: return 0.0;\n  }\n}\n\n");

    out.push_str(&format!(
        "const int PD_NUM_PARAMS = {};\n",
        inp.params.len()
    ));
    out.push_str("const PdParamInfo PD_PARAMS[] = {\n");
    for p in inp.params {
        out.push_str(&format!(
            "  {{ \"{}\", {}, {}, {} }},\n",
            escape_c_string(&p.name),
            p.min,
            p.max,
            p.default
        ));
    }
    if inp.params.is_empty() {
        out.push_str("  { \"\", 0, 0, 0 }, // unused: PD_NUM_PARAMS is 0\n");
    }
    out.push_str("};\n\n");

    out.push_str(&format!(
        "const int PD_HAS_AUDIO_IN = {};\n",
        if inp.has_audio_in { 1 } else { 0 }
    ));
    out.push_str(&format!(
        "const int PD_HAS_NOTE_IN = {};\n",
        if inp.has_note_in { 1 } else { 0 }
    ));
    out.push_str(&format!(
        "const int PD_HAS_CTL_IN = {};\n",
        if inp.ctlin_nodes.is_empty() { 0 } else { 1 }
    ));
    out.push_str(&format!(
        "const int PD_HAS_BEND_IN = {};\n",
        if inp.bendin_ids.is_empty() { 0 } else { 1 }
    ));
    out.push_str(&format!(
        "const int PD_HAS_TOUCH_IN = {};\n",
        if inp.touchin_ids.is_empty() { 0 } else { 1 }
    ));
    out.push_str(&format!(
        "const int PD_HAS_PGM_IN = {};\n",
        if inp.pgmin_ids.is_empty() { 0 } else { 1 }
    ));

    out
}

fn escape_c_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdast::parse_patch_no_loader;

    fn generate_c(src: &str) -> (String, Vec<String>) {
        let result = parse_patch_no_loader(src).unwrap();
        let mut g = WclapGenerator::new();
        let c = g.generate(&result.patch.root);
        (c, g.warnings)
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
    fn substitute_dollars_out_of_range_falls_back_to_zero() {
        let args = vec![Token::Dollar(5)];
        let out = substitute_dollars(&args, &[]);
        assert_eq!(out[0], Token::Float(0.0));
    }

    #[test]
    fn topo_order_respects_forward_edges() {
        // 0 -> 1 -> 2
        let conns = vec![
            Connection {
                src_node: 0,
                src_outlet: 0,
                dst_node: 1,
                dst_inlet: 0,
            },
            Connection {
                src_node: 1,
                src_outlet: 0,
                dst_node: 2,
                dst_inlet: 0,
            },
        ];
        let order = topo_order(&[0, 1, 2], &conns);
        let pos = |id: u32| order.iter().position(|&x| x == id).unwrap();
        assert!(pos(0) < pos(1));
        assert!(pos(1) < pos(2));
    }

    #[test]
    fn topo_order_survives_a_cycle() {
        // 0 -> 1 -> 0 (feedback loop) — must not panic/loop forever, and
        // must still return both ids exactly once.
        let conns = vec![
            Connection {
                src_node: 0,
                src_outlet: 0,
                dst_node: 1,
                dst_inlet: 0,
            },
            Connection {
                src_node: 1,
                src_outlet: 0,
                dst_node: 0,
                dst_inlet: 0,
            },
        ];
        let order = topo_order(&[0, 1], &conns);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1]);
    }

    #[test]
    fn bare_receive_with_no_sender_becomes_one_param() {
        let src = "#N canvas 0 50 450 300 12;\r\n\
                    #X obj 20 20 r foo;\r\n\
                    #X obj 20 60 r foo;\r\n";
        let (c, _warn) = generate_c(src);
        assert!(c.contains("const int PD_NUM_PARAMS = 1;"), "{c}");
        assert!(c.contains("\"foo\""), "{c}");
    }

    #[test]
    fn receive_with_matching_send_is_not_a_param() {
        let src = "#N canvas 0 50 450 300 12;\r\n\
                    #X msg 20 20 1;\r\n\
                    #X obj 20 60 s foo;\r\n\
                    #X obj 20 100 r foo;\r\n\
                    #X connect 0 0 1 0;\r\n";
        let (c, _warn) = generate_c(src);
        assert!(c.contains("const int PD_NUM_PARAMS = 0;"), "{c}");
    }

    #[test]
    fn osc_to_dac_generates_signal_step_and_no_note_in() {
        let src = "#N canvas 0 50 450 300 12;\r\n\
                    #X obj 20 20 osc~ 440;\r\n\
                    #X obj 20 60 dac~;\r\n\
                    #X connect 0 0 1 0;\r\n\
                    #X connect 0 0 1 1;\r\n";
        let (c, warn) = generate_c(src);
        assert!(warn.is_empty(), "unexpected warnings: {warn:?}");
        assert!(c.contains("pd_signal_step"));
        assert!(c.contains("sin(st->n0_phase"));
        assert!(c.contains("const int PD_HAS_NOTE_IN = 0;"));
        assert!(c.contains("const int PD_HAS_AUDIO_IN = 0;"));
    }

    #[test]
    fn notein_sets_has_note_in_and_wires_pitch_field() {
        let src = "#N canvas 0 50 450 300 12;\r\n\
                    #X obj 20 20 notein;\r\n\
                    #X obj 20 60 mtof;\r\n\
                    #X obj 20 100 osc~;\r\n\
                    #X obj 20 140 dac~;\r\n\
                    #X connect 0 0 1 0;\r\n\
                    #X connect 1 0 2 0;\r\n\
                    #X connect 2 0 3 0;\r\n\
                    #X connect 2 0 3 1;\r\n";
        let (c, _warn) = generate_c(src);
        assert!(c.contains("const int PD_HAS_NOTE_IN = 1;"));
        // pd_note_on pushes real messages from notein's own outlets, right to
        // left (channel, velocity, pitch) exactly as PD fires them.
        assert!(c.contains("pd_n0_out2(st, pd_msg_f(1.0));"), "{c}");
        assert!(
            c.contains("pd_n0_out1(st, pd_msg_f(velocity01 * 127.0));"),
            "{c}"
        );
        assert!(c.contains("pd_n0_out0(st, pd_msg_f((double)key));"), "{c}");
        let vel_at = c.find("pd_n0_out1").unwrap();
        let pitch_at = c.find("pd_n0_out0(st, pd_msg_f((double)key))").unwrap();
        assert!(
            vel_at < pitch_at,
            "velocity must be latched before pitch triggers: {c}"
        );
    }

    // Timing correctness for metro/delay/pipe is verified separately by
    // compiling the generated C and running it (see PR description) — a
    // 5ms metro produces exact 240-sample gaps at 48kHz, and delay fires
    // exactly once on a rising edge. This test just locks down that the
    // scheduler objects land in the *signal* domain (sample-accurate
    // timing) even though they're not tilde objects, and that their state
    // includes edge-detection fields.
    #[test]
    fn metro_and_delay_are_signal_domain_with_edge_detection() {
        let src = "#N canvas 0 50 450 300 12;\r\n\
                    #X obj 20 20 r gate;\r\n\
                    #X obj 20 60 metro 5;\r\n\
                    #X obj 20 100 delay 5;\r\n\
                    #X obj 20 140 dac~;\r\n\
                    #X connect 0 0 1 0;\r\n\
                    #X connect 1 0 2 0;\r\n\
                    #X connect 2 0 3 0;\r\n\
                    #X connect 2 0 3 1;\r\n";
        let (c, warn) = generate_c(src);
        assert!(warn.is_empty(), "unexpected warnings: {warn:?}");
        // metro's compute must be in pd_signal_step (between the function
        // header and pd_process), not in pd_control_recompute.
        let sig_start = c.find("pd_signal_step").unwrap();
        let sig_body = &c[sig_start..c.find("void pd_process").unwrap()];
        assert!(
            sig_body.contains("_pg"),
            "metro/delay edge-detection missing from signal step: {c}"
        );
        assert!(
            c.contains("n1_armed") || c.contains("n2_armed"),
            "delay's arm/countdown state missing: {c}"
        );
    }

    #[test]
    fn unknown_object_warns_and_emits_zero_stub_instead_of_failing() {
        let src = "#N canvas 0 50 450 300 12;\r\n\
                    #X obj 20 20 totally_made_up_object~;\r\n\
                    #X obj 20 60 dac~;\r\n\
                    #X connect 0 0 1 0;\r\n";
        let (c, warn) = generate_c(src);
        assert!(
            !warn.is_empty(),
            "expected a warning for an unsupported object"
        );
        assert!(
            c.contains("pd_process"),
            "generator must still produce valid output: {c}"
        );
    }

    // Array-in-graph save format is fiddly to hand-write as .pd text, so
    // this builds the AST directly (pdast's own parser tests already cover
    // the textual format) to exercise tabread~/array codegen reliably.
    #[test]
    fn tabread_reads_seeded_array_data() {
        use pdast::types::{Canvas as C, Node as N};

        let array_canvas = C {
            x: 0,
            y: 0,
            width: 450,
            height: 300,
            font_size: None,
            name: None,
            open_on_load: false,
            coords: None,
            nodes: vec![N {
                id: 0,
                x: 0,
                y: 0,
                kind: NodeKind::Array {
                    name: "wave".into(),
                    size: 4,
                    data_type: "float".into(),
                    flags: 1,
                    data: vec![0.0, 0.5, 1.0, 0.5],
                },
            }],
            connections: vec![],
        };

        let root = C {
            x: 0,
            y: 0,
            width: 450,
            height: 300,
            font_size: Some(12),
            name: None,
            open_on_load: false,
            coords: None,
            nodes: vec![
                N {
                    id: 0,
                    x: 0,
                    y: 0,
                    kind: NodeKind::Graph {
                        content: Box::new(array_canvas),
                    },
                },
                N {
                    id: 1,
                    x: 0,
                    y: 0,
                    kind: NodeKind::Obj {
                        name: "tabread~".into(),
                        args: vec![Token::Symbol("wave".into())],
                    },
                },
                N {
                    id: 2,
                    x: 0,
                    y: 0,
                    kind: NodeKind::Obj {
                        name: "dac~".into(),
                        args: vec![],
                    },
                },
            ],
            connections: vec![
                Connection {
                    src_node: 1,
                    src_outlet: 0,
                    dst_node: 2,
                    dst_inlet: 0,
                },
                Connection {
                    src_node: 1,
                    src_outlet: 0,
                    dst_node: 2,
                    dst_inlet: 1,
                },
            ],
        };

        let mut g = WclapGenerator::new();
        let c = g.generate(&root);
        assert!(c.contains("double arr_wave[4];"), "{c}");
        assert!(c.contains("0, 0.5, 1, 0.5"), "seeded data missing: {c}");
        assert!(
            c.contains("st->arr_wave[_i]"),
            "tabread~ must read arr_wave: {c}"
        );
    }

    // Regression test for a real bug: resolve_boundaries() rewires
    // connections that touched a subpatch placeholder to instead touch its
    // inlet/outlet nodes directly, but those nodes still need their own
    // passthrough codegen to actually carry the value — without it, every
    // value crossing a sub-patch/abstraction boundary silently became 0
    // (reported as "no wclap codegen for 'inlet'" etc.) despite the wiring
    // itself being correct.
    #[test]
    fn subpatch_boundary_nodes_pass_values_through_not_zero_stubs() {
        // Inline subpatch: inlet -> +1 -> outlet~ (control in, signal out,
        // to exercise both `inlet`/`outlet` and `inlet~`/`outlet~`).
        let src = "#N canvas 0 50 450 300 12;\r\n\
                    #X obj 20 20 r x;\r\n\
                    #N canvas 0 0 450 300 12;\r\n\
                    #X obj 10 10 inlet;\r\n\
                    #X obj 10 40 + 1;\r\n\
                    #X obj 10 70 sig~;\r\n\
                    #X obj 10 100 outlet~;\r\n\
                    #X connect 0 0 1 0;\r\n\
                    #X connect 1 0 2 0;\r\n\
                    #X connect 2 0 3 0;\r\n\
                    #X restore 100 20 pd voice;\r\n\
                    #X obj 100 60 dac~;\r\n\
                    #X connect 0 0 1 0;\r\n\
                    #X connect 1 0 2 0;\r\n";
        let (c, warn) = generate_c(src);
        assert!(warn.is_empty(), "unexpected warnings: {warn:?}");
        assert!(
            !c.contains("inlet") && !c.contains("outlet"),
            "boundary object names shouldn't leak into codegen at all: {c}"
        );
        // r x (n0) -> [subpatch placeholder consumes id 1] -> inlet node
        // (n2) -> +1 (n3) -> sig~ (n4) -> outlet~ (n5) -> dac~.
        // The control-side boundary forwards the message verbatim...
        assert!(c.contains("pd_n2_out0(st, m);"), "inlet must forward m: {c}");
        // `+ 1`'s creation arg now seeds its cold-inlet latch (init to 1),
        // so the sum reads through both latches rather than a baked literal.
        assert!(
            c.contains("(st->n3_i0) + (st->n3_i1)"),
            "+ must read its own inlet latches, not a zero stub: {c}"
        );
        assert!(c.contains("st->n3_i1 = 1;"), "cold inlet must seed from the creation arg: {c}");
        // ...and the signal-side boundary stays a plain per-sample mirror.
        assert!(
            c.contains("st->n5 = st->n4;"),
            "outlet~ must mirror the internal signal: {c}"
        );
        assert!(c.contains("st->n5"), "dac~ must read outlet~'s field: {c}");
    }

    // Regression test for a real bug: wiring N signal sources into the same
    // dac~ inlet (the standard, overwhelmingly common way to mix multiple
    // voices in real PD — PD sums them automatically) only carried the
    // first-declared source through; the other N-1 were computed correctly
    // but silently dropped on the floor, since input_expr only ever
    // resolved a single incoming connection per inlet. Caught via a
    // 4-voice polyphonic synth patch where voices 2-4 were verified (by
    // compiling and running the generated C) to have correct gate/pitch/
    // envelope but contributed ~0 energy to the actual output.
    #[test]
    fn multiple_signal_sources_into_one_dac_inlet_are_summed_not_dropped() {
        let src = "#N canvas 0 50 450 300 12;\r\n\
                    #X obj 20 20 osc~ 220;\r\n\
                    #X obj 20 60 osc~ 440;\r\n\
                    #X obj 20 100 osc~ 660;\r\n\
                    #X obj 20 140 dac~;\r\n\
                    #X connect 0 0 3 0;\r\n\
                    #X connect 1 0 3 0;\r\n\
                    #X connect 2 0 3 0;\r\n\
                    #X connect 0 0 3 1;\r\n";
        let (c, warn) = generate_c(src);
        assert!(warn.is_empty(), "unexpected warnings: {warn:?}");
        assert!(
            c.contains("(st->n0 + st->n1 + st->n2)"),
            "dac~ left channel must sum all three oscillators, not just the first: {c}"
        );
        // Right channel has only one source — no summing needed, same as before.
        assert!(
            !c.contains("(st->n0 + st->n0"),
            "single-connection inlet shouldn't grow a spurious sum: {c}"
        );
    }

    // `poly` follows PD's real contract: three outlets (voice number,
    // pitch, velocity) fired right to left, dispatched downstream with
    // `[route 1 2 ...]`. This is only correct because the control graph is
    // message-driven — a `route` outlet that doesn't fire leaves that voice's
    // latched pitch/velocity alone, instead of re-evaluating it against the
    // new note and cutting it off.
    #[test]
    fn poly_has_pd_three_outlet_contract_and_route_dispatches_the_remainder() {
        let src = "#N canvas 0 50 450 300 12;\r\n\
                    #X obj 20 20 notein;\r\n\
                    #X obj 20 60 pack f f;\r\n\
                    #X obj 20 100 poly 2 1;\r\n\
                    #X obj 20 140 pack f f f;\r\n\
                    #X obj 20 180 route 1 2;\r\n\
                    #X obj 20 220 unpack 0 0;\r\n\
                    #X obj 120 220 unpack 0 0;\r\n\
                    #X connect 0 0 1 0;\r\n\
                    #X connect 0 1 1 1;\r\n\
                    #X connect 1 0 2 0;\r\n\
                    #X connect 2 0 3 0;\r\n\
                    #X connect 2 1 3 1;\r\n\
                    #X connect 2 2 3 2;\r\n\
                    #X connect 3 0 4 0;\r\n\
                    #X connect 4 0 5 0;\r\n\
                    #X connect 4 1 6 0;\r\n";
        let (c, warn) = generate_c(src);
        assert!(warn.is_empty(), "unexpected warnings: {warn:?}");

        // Three outlets, fired right to left (velocity, pitch, voice#).
        let vel = c.find("pd_n2_out2(st, pd_msg_f(_vel));").expect("no velocity send");
        let pitch = c.find("pd_n2_out1(st, pd_msg_f(_pitch));").expect("no pitch send");
        let voice = c
            .find("pd_n2_out0(st, pd_msg_f((double)(_slot + 1)));")
            .expect("no voice-number send");
        assert!(vel < pitch && pitch < voice, "poly must fire right to left: {c}");

        // route emits the REMAINDER of the message on the matching outlet
        // only, and returns — no other branch is touched.
        assert!(c.contains("_rest.len = m.len > 0 ? m.len - 1 : 0;"), "{c}");
        assert!(c.contains("if (_sel == (1)) { pd_n4_out0(st, _rest); return; }"), "{c}");
        assert!(c.contains("if (_sel == (2)) { pd_n4_out1(st, _rest); return; }"), "{c}");

        // A list into a hot inlet spreads across the cold inlets to its right,
        // which is what lets `[pack f f]` fill poly's velocity inlet with no
        // second patch cord.
        assert!(c.contains("case 1: st->n2_i1 = m.a[_i]; break;"), "{c}");
    }

}
