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
//! - The **signal domain** (tilde objects) is recomputed every audio sample
//!   inside `pd_signal_step()`. The **control domain** (everything else) is
//!   recomputed only at event boundaries (note on/off, param change) inside
//!   `pd_control_recompute()` — NOT every sample like pdast2faust's Faust
//!   output, which is what causes its documented `change`/`metro`/ordering
//!   caveats. A control value feeding a signal inlet is naturally
//!   sample-and-held between control updates, matching real PD semantics.
//! - Sub-patches/abstractions are fully flattened (graph-spliced with id
//!   remapping and real `$1`/`$2` substitution) before codegen, rather than
//!   pdast2faust's expression-string-splicing approach (which is confirmed
//!   to drop the inner canvas's bindings and produce dangling references).

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
            "dac~" | "send" | "s" | "print" => 0,
            "notein" => 3,
            "vcf~" | "moses" => 2,
            "sel" | "select" => {
                let n = args
                    .iter()
                    .filter(|t| matches!(t, Token::Float(_)))
                    .count();
                (n.max(1)) + 1
            }
            "pack" | "unpack" => args.len().max(2),
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

// ── Main generator ────────────────────────────────────────────────────────────

pub struct WclapGenerator {
    pub warnings: Vec<String>,
}

impl WclapGenerator {
    pub fn new() -> Self {
        WclapGenerator {
            warnings: Vec::new(),
        }
    }

    pub fn generate(&mut self, canvas: &Canvas) -> String {
        let (nodes, connections) = flatten_patch(canvas);

        let active: Vec<&Node> = nodes
            .iter()
            .filter(|n| !matches!(n.kind, NodeKind::Text { .. } | NodeKind::Array { .. }))
            .collect();
        let active_ids: Vec<u32> = active.iter().map(|n| n.id).collect();
        let node_by_id: HashMap<u32, &Node> = active.iter().map(|n| (n.id, *n)).collect();

        let bus_map = collect_bus_map(&nodes);
        let params = collect_params(&nodes);

        let order = topo_order(&active_ids, &connections);

        let mut node_outlets: HashMap<u32, usize> = HashMap::new();
        for n in &active {
            node_outlets.insert(n.id, outlet_count(n));
        }

        let has_audio_in = active
            .iter()
            .any(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name=="adc~"));
        let notein_id: Option<u32> = active
            .iter()
            .find(|n| matches!(&n.kind, NodeKind::Obj{name,..} if name=="notein"))
            .map(|n| n.id);
        let has_note_in = notein_id.is_some();

        let mut state_fields = String::new();
        let mut init_stmts = String::new();
        let mut signal_stmts = String::new();
        let mut control_stmts = String::new();
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
                &bus_map,
                &mut dac_l,
                &mut dac_r,
            );

            state_fields.push_str(&emitted.state_fields);
            init_stmts.push_str(&emitted.init);
            match emitted.domain {
                Domain::Signal => signal_stmts.push_str(&emitted.compute),
                Domain::Control => control_stmts.push_str(&emitted.compute),
            }
        }

        render_output(RenderInput {
            state_fields,
            init_stmts,
            signal_stmts,
            control_stmts,
            dac_l,
            dac_r,
            params: &params,
            has_audio_in,
            has_note_in,
            notein_id,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_node(
        &mut self,
        node: &Node,
        incoming: &[&Connection],
        node_outlets: &HashMap<u32, usize>,
        bus_map: &BTreeMap<String, BusEntry>,
        dac_l: &mut Vec<String>,
        dac_r: &mut Vec<String>,
    ) -> EmittedNode {
        let id = node.id;

        // Resolve the C expression feeding a given inlet: connected source's
        // state field, else a creation-arg constant, else a caller default.
        let input_expr = |inlet: u32, default_lit: &str| -> String {
            if let Some(c) = incoming.iter().find(|c| c.dst_inlet == inlet) {
                let n_out = node_outlets.get(&c.src_node).copied().unwrap_or(1) as u32;
                format!(
                    "st->{}",
                    field_o(c.src_node, if n_out > 1 { c.src_outlet } else { 0 })
                )
            } else {
                default_lit.to_string()
            }
        };

        match &node.kind {
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

            NodeKind::Msg { messages } => {
                let val = messages
                    .first()
                    .and_then(|m| m.first())
                    .and_then(|t| {
                        if let Token::Float(f) = t {
                            Some(*f)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0.0);
                let f = field(id);
                EmittedNode {
                    domain: Domain::Control,
                    state_fields: format!("  double {f};\n"),
                    init: format!("  st->{f} = {val};\n"),
                    compute: String::new(),
                }
            }

            NodeKind::Obj { name, args } => self.emit_obj(
                name,
                args,
                id,
                node_outlets,
                bus_map,
                &input_expr,
                dac_l,
                dac_r,
            ),

            _ => EmittedNode {
                domain: Domain::Control,
                state_fields: String::new(),
                init: String::new(),
                compute: String::new(),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_obj(
        &mut self,
        name: &str,
        args: &[Token],
        id: u32,
        _node_outlets: &HashMap<u32, usize>,
        bus_map: &BTreeMap<String, BusEntry>,
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

        match name {
            // ── send / receive / value ────────────────────────────────────
            "send" | "s" => {
                let has_receiver = bus_name(args)
                    .and_then(|b| bus_map.get(&b))
                    .is_some_and(|e| !e.receivers.is_empty());
                let in0 = input_expr(0, "0.0");
                let compute = if has_receiver {
                    format!("  st->{f} = {in0};\n")
                } else {
                    String::new() // dropped: no receiver, true sink
                };
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
                if let Some(entry) = bus_map.get(&bname) {
                    if let Some(&sender) = entry.senders.first() {
                        // Alias: this node's field just mirrors the sender's.
                        return EmittedNode {
                            domain: Domain::Control,
                            state_fields: format!("  double {f};\n"),
                            init: format!("  st->{f} = 0.0;\n"),
                            compute: format!("  st->{f} = st->{};\n", field(sender)),
                        };
                    }
                }
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
                let lo = input_expr(1, &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }));
                let hi = input_expr(2, &format!("{}", if args.len() < 2 { 1.0 } else { farg(1) }));
                simple_control(
                    id,
                    &format!("(({a}) < ({lo}) ? ({lo}) : (({a}) > ({hi}) ? ({hi}) : ({a})))"),
                )
            }
            "int" | "i" => {
                let a = input_expr(0, "0.0");
                simple_control(id, &format!("(double)(int64_t)({a})"))
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
                simple_control(id, &format!("(({a}) > 0.0 ? pow(10.0, (({a}) - 100.0) / 20.0) : 0.0)"))
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
                simple_control(id, &format!("(({a}) > 0.0 ? pow(10.0, (({a}) - 100.0) / 10.0) : 0.0)"))
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
                    compute: format!(
                        "  {{ double _v = {a}, _t = {thresh}; st->{o0} = (_v < _t) ? _v : 0.0; st->{o1} = (_v >= _t) ? _v : 0.0; }}\n"
                    ),
                }
            }
            "spigot" => {
                let a = input_expr(0, "0.0");
                let gate = input_expr(
                    1,
                    &format!("{}", if args.is_empty() { 0.0 } else { farg(0) }),
                );
                simple_control(id, &format!("(({gate}) != 0.0 ? ({a}) : 0.0)"))
            }
            "sel" | "select" => {
                let a = input_expr(0, "0.0");
                let targets: Vec<f64> = if args.is_empty() {
                    vec![0.0]
                } else {
                    args.iter()
                        .filter_map(|t| if let Token::Float(v) = t { Some(*v) } else { None })
                        .collect()
                };
                let mut compute = String::new();
                let mut state_fields = String::new();
                let mut init = String::new();
                for (i, t) in targets.iter().enumerate() {
                    let fld = field_o(id, i as u32);
                    state_fields.push_str(&format!("  double {fld};\n"));
                    init.push_str(&format!("  st->{fld} = 0.0;\n"));
                    compute.push_str(&format!(
                        "  st->{fld} = (({a}) == ({t}) ? 1.0 : 0.0);\n"
                    ));
                }
                // Last outlet: pass the input through when nothing matched.
                let pass_fld = field_o(id, targets.len() as u32);
                state_fields.push_str(&format!("  double {pass_fld};\n"));
                init.push_str(&format!("  st->{pass_fld} = 0.0;\n"));
                let matches_any = targets
                    .iter()
                    .map(|t| format!("(({a}) == ({t}))"))
                    .collect::<Vec<_>>()
                    .join(" || ");
                compute.push_str(&format!(
                    "  st->{pass_fld} = (!({matches_any})) ? ({a}) : 0.0;\n"
                ));
                EmittedNode { domain: Domain::Control, state_fields, init, compute }
            }
            "change" => {
                // Continuous approximation: mirrors the input every recompute
                // (real PD only outputs on an actual change/bang) — see README.
                let a = input_expr(0, "0.0");
                simple_control(id, &a)
            }
            "pack" => {
                let n = args.len().max(2);
                let mut state_fields = String::new();
                let mut init = String::new();
                let mut compute = String::new();
                for i in 0..n {
                    let fld = field_o(id, i as u32);
                    let default = format!("{}", args.get(i).and_then(|t| if let Token::Float(v)=t {Some(*v)} else {None}).unwrap_or(0.0));
                    let v = input_expr(i as u32, &default);
                    state_fields.push_str(&format!("  double {fld};\n"));
                    init.push_str(&format!("  st->{fld} = 0.0;\n"));
                    compute.push_str(&format!("  st->{fld} = {v};\n"));
                }
                EmittedNode { domain: Domain::Control, state_fields, init, compute }
            }
            "unpack" => {
                // Single numeric input feeds every outlet identically (no
                // real list-splitting since we carry scalars, not lists).
                let n = args.len().max(2);
                let a = input_expr(0, "0.0");
                let mut state_fields = String::new();
                let mut init = String::new();
                let mut compute = String::new();
                for i in 0..n {
                    let fld = field_o(id, i as u32);
                    state_fields.push_str(&format!("  double {fld};\n"));
                    init.push_str(&format!("  st->{fld} = 0.0;\n"));
                    compute.push_str(&format!("  st->{fld} = {a};\n"));
                }
                EmittedNode { domain: Domain::Control, state_fields, init, compute }
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
                compute: String::new(), // written directly by pd_note_on/off
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

fn simple_control(id: u32, expr: &str) -> EmittedNode {
    let f = field(id);
    EmittedNode {
        domain: Domain::Control,
        state_fields: format!("  double {f};\n"),
        init: format!("  st->{f} = 0.0;\n"),
        compute: format!("  st->{f} = {expr};\n"),
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
    signal_stmts: String,
    control_stmts: String,
    dac_l: Vec<String>,
    dac_r: Vec<String>,
    params: &'a [ParamInfo],
    has_audio_in: bool,
    has_note_in: bool,
    notein_id: Option<u32>,
}

fn render_output(inp: RenderInput) -> String {
    let mut out = String::new();
    out.push_str("// Generated by pdast2wclap — do not edit\n");
    out.push_str(
        "#include <math.h>\n#include <stdlib.h>\n#include <string.h>\n#include \"pd_wclap.h\"\n\n",
    );

    out.push_str("struct PdState {\n  double sample_rate;\n");
    out.push_str(&inp.state_fields);
    out.push_str("};\n\n");

    out.push_str("PdState* pd_create(double sample_rate) {\n");
    out.push_str("  PdState* st = (PdState*)calloc(1, sizeof(PdState));\n");
    out.push_str("  st->sample_rate = sample_rate > 0 ? sample_rate : 48000.0;\n");
    out.push_str(&inp.init_stmts);
    out.push_str("  return st;\n}\n\n");

    out.push_str("void pd_destroy(PdState* st) { free(st); }\n\n");

    out.push_str("void pd_control_recompute(PdState* st) {\n");
    out.push_str(&inp.control_stmts);
    out.push_str("}\n\n");

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

    // Control graph is recomputed on every note/param event (below) AND
    // once per process() call, so time-driven objects (line~, delay lines,
    // envelopes) and any pure-control chain downstream of them still see
    // fresh values even with no event in a given block — matching how
    // audio plugins commonly quantize control-rate updates to block size.
    out.push_str(
        "void pd_process(PdState* st, const float* in_l, const float* in_r, float* out_l, float* out_r, uint32_t nframes) {\n  pd_control_recompute(st);\n  for (uint32_t i = 0; i < nframes; i++) {\n    float il = in_l ? in_l[i] : 0.0f;\n    float ir = in_r ? in_r[i] : 0.0f;\n    pd_signal_step(st, il, ir, &out_l[i], &out_r[i]);\n  }\n}\n\n",
    );

    // Note on/off: monophonic (matches poketrack's single-active-note host
    // model). Writes the `notein` node's (pitch, velocity, channel) fields —
    // PD's real notein outlet order — then lets the control graph propagate.
    out.push_str("void pd_note_on(PdState* st, int16_t key, double velocity01) {\n");
    match inp.notein_id {
        Some(nid) => {
            out.push_str(&format!(
                "  st->{pitch} = (double)key;\n  st->{vel} = velocity01 * 127.0;\n  st->{chan} = 1.0;\n",
                pitch = field(nid), vel = field_o(nid, 1), chan = field_o(nid, 2)
            ));
        }
        None => out.push_str("  (void)key; (void)velocity01;\n"),
    }
    out.push_str("  pd_control_recompute(st);\n}\n\n");

    out.push_str("void pd_note_off(PdState* st, int16_t key, double velocity01) {\n");
    match inp.notein_id {
        Some(nid) => {
            out.push_str(&format!(
                "  st->{pitch} = (double)key;\n  st->{vel} = 0.0;\n  (void)velocity01;\n",
                pitch = field(nid),
                vel = field_o(nid, 1)
            ));
        }
        None => out.push_str("  (void)key; (void)velocity01;\n"),
    }
    out.push_str("  pd_control_recompute(st);\n}\n\n");

    out.push_str("void pd_set_param(PdState* st, int32_t index, double value) {\n");
    out.push_str("  switch (index) {\n");
    for (i, p) in inp.params.iter().enumerate() {
        for &tid in &p.target_ids {
            out.push_str(&format!(
                "    case {i}: st->{} = value; break;\n",
                field(tid)
            ));
        }
        if p.target_ids.is_empty() {
            out.push_str(&format!("    case {i}: break;\n"));
        }
    }
    out.push_str("    default: break;\n  }\n");
    out.push_str("  pd_control_recompute(st);\n}\n\n");

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
        // pd_note_on must write into notein's own field (n0), not some
        // downstream node's.
        assert!(c.contains("st->n0 = (double)key;"), "{c}");
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
}
