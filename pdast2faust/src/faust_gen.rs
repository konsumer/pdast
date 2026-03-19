//! Faust DSP code generator from pdast AST.
//!
//! # Architecture
//!
//! Each PD node becomes a named Faust `let`-binding inside a `with { }`
//! block, so every node is computed exactly once even when its output fans
//! out to multiple destinations.  The final `process` expression assembles
//! the sink nodes.
//!
//! ## Node classification
//!
//! Every node falls into one of:
//! - **Signal** (audio-rate, tilde objects) — always active
//! - **Control** — math, routing, timing, GUI, send/receive, etc.
//! - **Unsupported** — symbol routing, `trigger`, typed `pack`; emitted as
//!   passthrough stubs with a warning comment
//!
//! Control nodes are NOT ignored — they are included in the unified graph
//! and translated to always-running Faust expressions (sample-rate
//! equivalents). See DEVELOPMENT.md for the semantic caveats.
//!
//! ## send / receive bus
//!
//! `[send X]` / `[receive X]` / `[value X]` sharing the same name are
//! resolved globally: a single `nentry("X", ...)` expression is emitted
//! and every receive/value node references it.  Send nodes become signal
//! sinks (dropped with `!`) unless a matching receive exists in the same
//! canvas, in which case the expression is wired directly.
//!
//! ## Fan-out
//!
//! One outlet connecting to N inlets is represented in Faust with `<:` (split).
//! Named bindings mean the expression is not duplicated in the source even if
//! Faust's compiler has to trace the value to multiple destinations.
//!
//! ## Template files
//!
//! Built-in templates live in `faust-lib/` (embedded at compile time via
//! `include_str!`).  User `--lib` directories are searched first.  Each
//! file contains:
//!
//! ```faust
//! pdobj[(arg1, arg2, ...)] = <faust-expression>;
//! ```
//!
//! The generator extracts the expression and emits a helper function
//! `pd_<sanitized_name>`.  Constant creation arguments from the PD patch
//! are applied as partial application.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use pdast::types::{Canvas, Connection, NodeKind, SubPatchContent, Token};

// ── Object classification ─────────────────────────────────────────────────────

/// How a PD object name maps into the Faust world.
#[derive(Debug, Clone, PartialEq)]
enum ObjClass {
    /// Audio-rate object — ends with `~`.
    Signal,
    /// Control-rate object with a known Faust equivalent.
    Control,
    /// Object that has no useful Faust equivalent (symbol routing, etc.).
    /// Emitted as a passthrough with a warning comment.
    Unsupported,
}

fn classify(name: &str) -> ObjClass {
    if name.ends_with('~') {
        return ObjClass::Signal;
    }
    match name {
        // Control math
        "+" | "-" | "*" | "/" | "mod" | "div" | "pow"
        | "max" | "min" | "abs" | "sqrt" | "log" | "exp"
        | "sin" | "cos" | "atan" | "atan2"
        | "wrap" | "clip" | "int" | "float"
        // Comparisons & logic
        | ">" | "<" | ">=" | "<=" | "==" | "!=" | "&&" | "||" | "!"
        // Routing & control flow
        | "sel" | "select" | "moses" | "change"
        // Timing
        | "metro" | "timer" | "delay" | "pipe" | "line"
        // Storage / UI
        | "bang" | "toggle" | "trigger" | "t"
        // Send / receive / value
        | "send" | "s" | "receive" | "r" | "value"
        // Conversion
        | "mtof" | "ftom" | "dbtorms" | "rmstodb" | "dbtopow" | "powtodb"
        // MIDI input
        | "notein" | "ctlin" | "bendin" | "touchin" | "pgmin"
        // Pack / unpack (numeric approximation)
        | "pack" | "unpack"
        // expr (stub)
        | "expr" => ObjClass::Control,

        // Genuinely unsupported
        "route" | "symbol" | "list" => ObjClass::Unsupported,

        _ => ObjClass::Control, // unknown → try template, warn if missing
    }
}

/// True for objects that consume a signal without producing one (sinks).
fn is_sink(name: &str) -> bool {
    matches!(name, "dac~" | "outlet~" | "outlet" | "send" | "s" | "print")
}

// ── Built-in library (embedded at compile time) ───────────────────────────────

fn builtin_lib() -> HashMap<&'static str, &'static str> {
    let mut m: HashMap<&'static str, &'static str> = HashMap::new();
    macro_rules! embed {
        ($name:literal, $file:literal) => {
            m.insert($name, include_str!(concat!("../faust-lib/", $file)));
        };
    }
    // Audio-rate
    embed!("osc~", "osc~.dsp");
    embed!("phasor~", "phasor~.dsp");
    embed!("noise~", "noise~.dsp");
    embed!("dac~", "dac~.dsp");
    embed!("adc~", "adc~.dsp");
    embed!("*~", "mul~.dsp");
    embed!("+~", "+~.dsp");
    embed!("-~", "-~.dsp");
    embed!("/~", "div~.dsp");
    embed!("lop~", "lop~.dsp");
    embed!("hip~", "hip~.dsp");
    embed!("bp~", "bp~.dsp");
    embed!("vcf~", "vcf~.dsp");
    embed!("delwrite~", "delwrite~.dsp");
    embed!("delread~", "delread~.dsp");
    embed!("vd~", "vd~.dsp");
    embed!("line~", "line~.dsp");
    embed!("sig~", "sig~.dsp");
    embed!("abs~", "abs~.dsp");
    embed!("sqrt~", "sqrt~.dsp");
    embed!("wrap~", "wrap~.dsp");
    embed!("clip~", "clip~.dsp");
    embed!("tabread4~", "tabread4~.dsp");
    embed!("tabosc4~", "tabosc4~.dsp");
    embed!("outlet~", "outlet~.dsp");
    embed!("inlet~", "inlet~.dsp");
    embed!("snapshot~", "snapshot~.dsp");
    embed!("samphold~", "samphold~.dsp");
    embed!("env~", "env~.dsp");
    embed!("threshold~", "threshold~.dsp");
    embed!("rzero~", "rzero~.dsp");
    embed!("rpole~", "rpole~.dsp");
    embed!("biquad~", "biquad~.dsp");
    embed!("cpole~", "cpole~.dsp");
    embed!("expr~", "expr~.dsp");
    // Control-rate
    embed!("+", "+.dsp");
    embed!("-", "-.dsp");
    embed!("*", "mul.dsp");
    embed!("/", "div.dsp");
    embed!("mod", "mod.dsp");
    embed!("pow", "pow.dsp");
    embed!("max", "max.dsp");
    embed!("min", "min.dsp");
    embed!("abs", "abs.dsp");
    embed!("sqrt", "sqrt.dsp");
    embed!("log", "log.dsp");
    embed!("exp", "exp.dsp");
    embed!("sin", "sin.dsp");
    embed!("cos", "cos.dsp");
    embed!("atan", "atan.dsp");
    embed!("atan2", "atan2.dsp");
    embed!("wrap", "wrap.dsp");
    embed!("clip", "clip.dsp");
    embed!("int", "int.dsp");
    embed!("float", "float.dsp");
    embed!("change", "change.dsp");
    embed!("moses", "moses.dsp");
    embed!(">", "gt.dsp");
    embed!("<", "lt.dsp");
    embed!(">=", "gte.dsp");
    embed!("<=", "lte.dsp");
    embed!("==", "eq.dsp");
    embed!("!=", "neq.dsp");
    embed!("&&", "and.dsp");
    embed!("||", "or.dsp");
    embed!("!", "not.dsp");
    embed!("sel", "sel.dsp");
    embed!("select", "sel.dsp");
    embed!("metro", "metro.dsp");
    embed!("timer", "timer.dsp");
    embed!("delay", "delay.dsp");
    embed!("pipe", "delay.dsp");
    embed!("line", "line.dsp");
    embed!("bang", "bang.dsp");
    embed!("trigger", "trigger.dsp");
    embed!("t", "trigger.dsp");
    embed!("pack", "pack.dsp");
    embed!("unpack", "unpack.dsp");
    embed!("send", "send.dsp");
    embed!("s", "send.dsp");
    embed!("receive", "receive.dsp");
    embed!("r", "receive.dsp");
    embed!("value", "value.dsp");
    embed!("mtof", "mtof.dsp");
    embed!("ftom", "ftom.dsp");
    embed!("dbtorms", "dbtorms.dsp");
    embed!("rmstodb", "rmstodb.dsp");
    embed!("dbtopow", "dbtopow.dsp");
    embed!("powtodb", "powtodb.dsp");
    embed!("notein", "notein.dsp");
    embed!("ctlin", "ctlin.dsp");
    embed!("bendin", "bendin.dsp");
    embed!("expr", "expr.dsp");
    embed!("inlet", "inlet.dsp");
    embed!("outlet", "outlet.dsp");
    m
}

// ── Template loading ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ObjectTemplate {
    /// The full RHS of the `pdobj` definition, including any parameter list.
    /// e.g. for `pdobj(ms) = ba.pulse(...)` this is `(ms) = ba.pulse(...)`.
    /// Emitted as `pd_name<params_and_rhs>`.
    pub params_and_rhs: String,
}

pub struct TemplateResolver {
    builtin: HashMap<&'static str, &'static str>,
    user_dirs: Vec<std::path::PathBuf>,
    cache: HashMap<String, Option<ObjectTemplate>>,
}

impl TemplateResolver {
    pub fn new(user_dirs: Vec<std::path::PathBuf>) -> Self {
        TemplateResolver {
            builtin: builtin_lib(),
            user_dirs,
            cache: HashMap::new(),
        }
    }

    pub fn resolve(&mut self, name: &str) -> Option<&ObjectTemplate> {
        if !self.cache.contains_key(name) {
            let tmpl = self.load_template(name);
            self.cache.insert(name.to_string(), tmpl);
        }
        self.cache[name].as_ref()
    }

    fn load_template(&self, name: &str) -> Option<ObjectTemplate> {
        for dir in &self.user_dirs {
            let path = dir.join(format!("{}.dsp", name));
            if path.exists()
                && let Ok(content) = std::fs::read_to_string(&path)
            {
                return Some(parse_template(&content));
            }
        }
        if let Some(&content) = self.builtin.get(name) {
            return Some(parse_template(content));
        }
        None
    }
}

fn parse_template(content: &str) -> ObjectTemplate {
    // Extract everything after `pdobj` up to the terminating `;`.
    // This preserves `(params) = expr` so that the helper emits
    // `pd_name(params) = expr` — parameters are not stripped.
    let params_and_rhs = if let Some(pos) = content.find("pdobj") {
        let after = &content[pos + "pdobj".len()..];
        let body = find_statement_end(after.trim_start());
        body.trim().to_string()
    } else {
        // No pdobj declaration — treat whole file as the expression body
        format!(" = {}", content.trim().trim_end_matches(';'))
    };
    ObjectTemplate { params_and_rhs }
}

fn find_statement_end(s: &str) -> &str {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut end = s.len();
    for (i, c) in s.char_indices() {
        if in_str {
            if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ';' if depth <= 0 => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    &s[..end]
}

// ── Graph helpers ─────────────────────────────────────────────────────────────

/// Kahn topological sort over a set of node ids and their connections.
fn topo_sort(node_ids: &[u32], connections: &[&Connection]) -> Vec<u32> {
    let id_set: HashSet<u32> = node_ids.iter().copied().collect();
    let mut in_deg: HashMap<u32, usize> = node_ids.iter().map(|&id| (id, 0)).collect();
    let mut adj: HashMap<u32, Vec<u32>> = node_ids.iter().map(|&id| (id, vec![])).collect();

    for c in connections {
        if id_set.contains(&c.src_node) && id_set.contains(&c.dst_node) {
            adj.entry(c.src_node).or_default().push(c.dst_node);
            *in_deg.entry(c.dst_node).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<u32> = in_deg
        .iter()
        .filter(|&(_, &d)| d == 0)
        .map(|(&id, _)| id)
        .collect();
    // deterministic ordering for nodes with the same in-degree
    let mut q_vec: Vec<u32> = queue.drain(..).collect();
    q_vec.sort_unstable();
    queue.extend(q_vec);

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

    // Append remaining (cycles) in stable order
    for &id in node_ids {
        if !sorted.contains(&id) {
            sorted.push(id);
        }
    }
    sorted
}

/// Sanitize a PD object name to a valid Faust identifier segment.
pub fn sanitize_name(name: &str) -> String {
    // Special-case operators that would otherwise produce confusing names
    match name {
        "*" => return "mul".into(),
        "/" => return "div".into(),
        ">" => return "gt".into(),
        "<" => return "lt".into(),
        ">=" => return "gte".into(),
        "<=" => return "lte".into(),
        "==" => return "eq".into(),
        "!=" => return "neq".into(),
        "&&" => return "and".into(),
        "||" => return "or".into(),
        "!" => return "not".into(),
        _ => {}
    }
    name.chars()
        .map(|c| match c {
            '~' => 's',
            '+' => 'p',
            '-' => 'm',
            '.' => '_',
            c if c.is_alphanumeric() || c == '_' => c,
            _ => '_',
        })
        .collect()
}

/// Format a Token as a Faust literal.
fn token_to_faust(t: &Token) -> Option<String> {
    match t {
        Token::Float(f) => Some(format!("{f}")),
        Token::Dollar(_) | Token::DollarZero => Some("0".into()), // unresolved $arg → 0
        Token::Symbol(_) => None,                                 // symbols can't be Faust values
    }
}

/// Convert a GUI node to a Faust UI primitive expression.
fn gui_to_faust(node: &pdast::types::Node) -> String {
    use pdast::types::GuiKind;
    let NodeKind::Gui(g) = &node.kind else {
        return "_".into();
    };
    let lbl = g.label.as_deref().unwrap_or("param");
    match g.kind {
        GuiKind::HSlider | GuiKind::VSlider => {
            format!(
                "hslider(\"{lbl}\", {}, {}, {}, 0.001)",
                g.default_value, g.min, g.max
            )
        }
        GuiKind::Toggle => format!("checkbox(\"{lbl}\")"),
        GuiKind::NumberBox => {
            format!(
                "nentry(\"{lbl}\", {}, {}, {}, 0.001)",
                g.default_value, g.min, g.max
            )
        }
        GuiKind::Bang => format!("button(\"{lbl}\")"),
        GuiKind::HRadio | GuiKind::VRadio => {
            let cells = match &g.extra {
                pdast::types::GuiExtra::HRadio { num_cells }
                | pdast::types::GuiExtra::VRadio { num_cells } => *num_cells as f64 - 1.0,
                _ => g.max,
            };
            format!(
                "nentry(\"{lbl}\", {}, {}, {cells}, 1)",
                g.default_value, g.min
            )
        }
        GuiKind::Vu | GuiKind::Canvas => format!("// GUI:{lbl}"),
    }
}

/// Extract the send/receive/value bus name from a node's creation args.
fn bus_name(args: &[Token]) -> Option<String> {
    args.iter().find_map(|t| {
        if let Token::Symbol(s) = t {
            Some(s.clone())
        } else {
            None
        }
    })
}

// ── Main generator ────────────────────────────────────────────────────────────

pub struct FaustGenerator {
    pub resolver: TemplateResolver,
    pub warnings: Vec<String>,
}

impl FaustGenerator {
    pub fn new(user_dirs: Vec<std::path::PathBuf>) -> Self {
        FaustGenerator {
            resolver: TemplateResolver::new(user_dirs),
            warnings: Vec::new(),
        }
    }

    pub fn generate(&mut self, canvas: &Canvas) -> String {
        let mut out = String::new();
        out.push_str("// Generated by pdast2faust\n");
        out.push_str("import(\"stdfaust.lib\");\n\n");

        // Collect helper defs needed (one per unique object type)
        let helper_block = self.build_helpers(canvas);
        if !helper_block.is_empty() {
            out.push_str(&helper_block);
            out.push('\n');
        }

        let process = self.build_process(canvas);
        out.push_str(&process);
        out
    }

    // ── Helper function definitions ──────────────────────────────────────────

    fn build_helpers(&mut self, canvas: &Canvas) -> String {
        let mut seen: HashSet<String> = HashSet::new();
        let mut defs = String::new();

        for node in &canvas.nodes {
            let name = match &node.kind {
                NodeKind::Obj { name, .. } => name.clone(),
                // Recurse into sub-patches
                NodeKind::SubPatch {
                    content: SubPatchContent::Inline(inner),
                    ..
                } => {
                    let inner_helpers = self.build_helpers(inner);
                    defs.push_str(&inner_helpers);
                    continue;
                }
                _ => continue,
            };

            if seen.contains(&name) {
                continue;
            }
            seen.insert(name.clone());

            if let Some(tmpl) = self.resolver.resolve(&name) {
                let fn_name = sanitize_name(&name);
                // Emit: pd_name<params_and_rhs>;
                // e.g. pd_metro(ms) = ba.pulse(...);
                //  or  pd_oscs = os.osc;
                // Ensure there's a space before `=` when no params:
                // `pdobj = expr` → `(ms) = expr` or ` = expr`
                let sep = if tmpl.params_and_rhs.starts_with('(') {
                    ""
                } else {
                    " "
                };
                defs.push_str(&format!(
                    "// {name}\npd_{fn_name}{sep}{};\n",
                    tmpl.params_and_rhs
                ));
            } else if classify(&name) != ObjClass::Unsupported {
                // Unknown object — emit passthrough stub
                let fn_name = sanitize_name(&name);
                defs.push_str(&format!(
                    "// UNKNOWN: {name} (no template — passthrough stub)\npd_{fn_name} = _;\n"
                ));
                self.warnings.push(format!(
                    "No Faust template for '{name}' — emitted as passthrough stub"
                ));
            }
        }
        defs
    }

    // ── process block ────────────────────────────────────────────────────────

    fn build_process(&mut self, canvas: &Canvas) -> String {
        // 1. Collect all node ids that participate (everything except Text/Array)
        let active_ids: Vec<u32> = canvas
            .nodes
            .iter()
            .filter(|n| !matches!(n.kind, NodeKind::Text { .. } | NodeKind::Array { .. }))
            .map(|n| n.id)
            .collect();

        if active_ids.is_empty() {
            return "process = 0; // empty patch\n".to_string();
        }

        // 2. Build send/receive global bus map
        //    name → (list of sender node ids, list of receiver node ids)
        let bus_map = self.collect_bus_map(canvas);

        // 3. Topological sort
        let all_conns: Vec<&Connection> = canvas.connections.iter().collect();
        let order = topo_sort(&active_ids, &all_conns);

        // 4. Assign a Faust binding name to every active node
        //    n<id> — e.g. n0, n3, n7
        let binding_name = |id: u32| format!("n{id}");

        // 5. For each node in order, build its Faust expression
        //    Each node's expression receives its incoming signals.
        let mut bindings: Vec<(String, String)> = Vec::new(); // (name, expr)
        let mut node_outlets: HashMap<u32, usize> = HashMap::new(); // id → #outlets

        for &node_id in &order {
            let node = match canvas.nodes.iter().find(|n| n.id == node_id) {
                Some(n) => n,
                None => continue,
            };

            // Compute how many outlets this node has
            let num_outlets = self.outlet_count(node);
            node_outlets.insert(node_id, num_outlets);

            // Gather incoming connections, sorted by inlet index
            let mut incoming: Vec<&Connection> = canvas
                .connections
                .iter()
                .filter(|c| c.dst_node == node_id)
                .collect();
            incoming.sort_by_key(|c| (c.dst_inlet, c.src_node));

            // Build the right-hand side of the binding
            let rhs = self.node_rhs(node, &incoming, &binding_name, &node_outlets, &bus_map);

            bindings.push((binding_name(node_id), rhs));
        }

        // 6. Identify sink nodes (no outgoing connections, or named sinks).
        //    Exclude send nodes that have paired receivers — those are
        //    internal bus nodes, not output sinks.
        let has_outgoing: HashSet<u32> = canvas.connections.iter().map(|c| c.src_node).collect();
        let sink_ids: Vec<u32> = order
            .iter()
            .filter(|&&id| {
                let node = canvas.nodes.iter().find(|n| n.id == id);
                let is_paired_send = node.is_some_and(|n| {
                    if let NodeKind::Obj { name, args } = &n.kind {
                        (name == "send" || name == "s")
                            && bus_name(args)
                                .and_then(|b| bus_map.get(&b))
                                .is_some_and(|e| !e.receivers.is_empty())
                    } else {
                        false
                    }
                });
                if is_paired_send { return false; }
                !has_outgoing.contains(&id)
                    || node.is_some_and(|n| {
                        matches!(&n.kind, NodeKind::Obj { name, .. } if is_sink(name))
                            || matches!(&n.kind, NodeKind::Gui(g)
                                if matches!(g.kind, pdast::types::GuiKind::Vu | pdast::types::GuiKind::Canvas))
                    })
            })
            .copied()
            .collect();

        if bindings.is_empty() || sink_ids.is_empty() {
            return "process = 0; // no computable nodes\n".to_string();
        }

        // 7. Assemble the process block
        //    Use Faust's `with { }` to define all bindings, then reference sinks.
        let mut s = String::new();

        let sink_exprs: Vec<String> = sink_ids.iter().map(|id| binding_name(*id)).collect();

        let process_rhs = if sink_exprs.len() == 1 {
            sink_exprs[0].clone()
        } else {
            sink_exprs.join(",\n    ")
        };

        s.push_str(&format!("process = {process_rhs}\n"));
        s.push_str("with {\n");
        for (name, rhs) in &bindings {
            s.push_str(&format!("  {name} = {rhs};\n"));
        }
        s.push_str("};\n");

        s
    }

    /// Build the RHS expression for a single node.
    fn node_rhs(
        &mut self,
        node: &pdast::types::Node,
        incoming: &[&Connection],
        binding_name: &impl Fn(u32) -> String,
        node_outlets: &HashMap<u32, usize>,
        bus_map: &BTreeMap<String, BusEntry>,
    ) -> String {
        match &node.kind {
            NodeKind::Gui(_) => gui_to_faust(node),

            NodeKind::Text { .. } | NodeKind::Array { .. } => "0".into(),

            NodeKind::FloatAtom { .. } | NodeKind::SymbolAtom { .. } => {
                // Number/symbol boxes — treated as UI sliders
                "nentry(\"atom\", 0, -1e9, 1e9, 0.001)".into()
            }

            NodeKind::Msg { messages } => {
                // Message box: emit the first numeric atom as a constant signal
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
                format!("{val}")
            }

            NodeKind::SubPatch {
                content: SubPatchContent::Inline(inner),
                args,
                ..
            } => {
                // Inline sub-patch: generate as a nested process expression.
                // We do a simplified version — if it has inlet~/outlet~, wire them.
                let _ = args; // creation args ($1, $2) not substituted yet
                let inner_process = self.build_process(inner);
                // Wrap the inner process as an expression by extracting `process = X;`
                extract_process_expr(&inner_process).unwrap_or_else(|| "_".into())
            }

            NodeKind::SubPatch {
                content: SubPatchContent::Unresolved,
                name,
                args,
            } => {
                self.warnings.push(format!(
                    "Unresolved abstraction '{name}' — emitting passthrough stub"
                ));
                let fn_name = sanitize_name(name);
                let fargs = args
                    .iter()
                    .filter_map(token_to_faust)
                    .collect::<Vec<_>>()
                    .join(", ");
                if fargs.is_empty() {
                    format!("pd_{fn_name} // unresolved abstraction")
                } else {
                    format!("pd_{fn_name}({fargs}) // unresolved abstraction")
                }
            }

            NodeKind::Graph { .. } | NodeKind::Unknown { .. } => "_".into(),

            NodeKind::Obj { name, args } => {
                // --- send/receive/value special handling ---
                match name.as_str() {
                    "send" | "s" => {
                        // send node: if a matching receive exists, forward the
                        // signal as a passthrough so receive can reference this
                        // binding directly. Otherwise drop it.
                        let has_receiver = bus_name(args)
                            .and_then(|b| bus_map.get(&b))
                            .is_some_and(|e| !e.receivers.is_empty());
                        let override_fn = if has_receiver { "_" } else { "!" };
                        return self.wire_inputs(incoming, binding_name, node_outlets, override_fn);
                    }
                    "receive" | "r" | "value" => {
                        if let Some(bname) = bus_name(args) {
                            if let Some(entry) = bus_map.get(&bname)
                                && !entry.senders.is_empty()
                            {
                                // Reference the send node's binding directly.
                                // The send node passes through, so its binding
                                // holds the value.
                                let sender_id = entry.senders[0];
                                return binding_name(sender_id);
                            } else {
                                // No matching sender — emit a UI element named after the bus
                                // No matching sender — emit a UI element named after the bus
                                return format!("nentry(\"{bname}\", 0, -1e9, 1e9, 0.001)");
                            }
                        }
                        return "nentry(\"receive\", 0, -1e9, 1e9, 0.001)".into();
                    }
                    _ => {}
                }

                // --- unsupported objects ---
                if classify(name) == ObjClass::Unsupported {
                    self.warnings.push(format!(
                        "'{name}' has no Faust equivalent (symbol/type routing) — passthrough stub"
                    ));
                    return self.wire_inputs(incoming, binding_name, node_outlets, "_");
                }

                // --- expr: emit comment with original expression ---
                if name == "expr" || name == "expr~" {
                    let expr_text = args
                        .iter()
                        .map(|t| format!("{t}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    self.warnings.push(format!(
                        "expr '{expr_text}' not translated — manual conversion needed"
                    ));
                    return format!("_ /* expr: {expr_text} */");
                }

                self.apply_fn(name, args, incoming, binding_name, node_outlets, "")
            }
        }
    }

    /// Build a call to the helper function `pd_<name>`, wiring inputs correctly.
    fn apply_fn(
        &mut self,
        name: &str,
        args: &[Token],
        incoming: &[&Connection],
        binding_name: &impl Fn(u32) -> String,
        node_outlets: &HashMap<u32, usize>,
        override_fn: &str, // if non-empty, use this expression instead of pd_<name>
    ) -> String {
        let fn_name = if override_fn.is_empty() {
            let sname = sanitize_name(name);
            // Build const args from creation arguments (float args only)
            let const_args: Vec<String> = args.iter().filter_map(token_to_faust).collect();
            if const_args.is_empty() {
                format!("pd_{sname}")
            } else {
                format!("pd_{sname}({})", const_args.join(", "))
            }
        } else {
            override_fn.to_string()
        };

        self.wire_inputs(incoming, binding_name, node_outlets, &fn_name)
    }

    /// Wire incoming connections into a function call.
    /// - 0 incoming: return fn_expr as-is (source node)
    /// - 1 incoming: `<src> : <fn>`
    /// - N incoming (same outlet): `<src> <: (<fn>, ...)` — fan-out
    /// - N incoming (different outlets): `(<src1>, <src2>) : <fn>` — merge
    fn wire_inputs(
        &self,
        incoming: &[&Connection],
        binding_name: &impl Fn(u32) -> String,
        node_outlets: &HashMap<u32, usize>,
        fn_expr: &str,
    ) -> String {
        if incoming.is_empty() {
            return fn_expr.to_string();
        }

        // Build the input signal expression for each inlet slot.
        // Group by src_node to detect fan-out from the same node.
        let mut by_inlet: BTreeMap<u32, String> = BTreeMap::new();
        for c in incoming {
            let src_name = binding_name(c.src_node);
            let n_outlets = node_outlets.get(&c.src_node).copied().unwrap_or(1);
            let src_expr = if n_outlets > 1 {
                // Select the right outlet using Faust's ba.selector or split
                format!(
                    "({}  <: si.bus({n_outlets})) : ba.selector({}, {n_outlets})",
                    src_name, c.src_outlet,
                )
            } else {
                src_name
            };
            by_inlet.insert(c.dst_inlet, src_expr);
        }

        let inputs: Vec<String> = by_inlet.into_values().collect();

        if inputs.len() == 1 {
            format!("{} : {fn_expr}", inputs[0])
        } else {
            format!("({}) : {fn_expr}", inputs.join(", "))
        }
    }

    /// Estimate how many outlets a node produces.
    fn outlet_count(&self, node: &pdast::types::Node) -> usize {
        match &node.kind {
            NodeKind::Obj { name, .. } => match name.as_str() {
                "dac~" | "outlet~" | "outlet" | "send" | "s" | "print" => 0,
                "moses" | "moses~" | "snapshot~" | "threshold~" | "cpole~" => 2,
                "notein" => 3,
                "vcf~" => 2,
                _ => 1,
            },
            NodeKind::Gui(_) => 1,
            NodeKind::FloatAtom { .. } | NodeKind::SymbolAtom { .. } | NodeKind::Msg { .. } => 1,
            NodeKind::SubPatch {
                content: SubPatchContent::Inline(inner),
                ..
            } => inner
                .nodes
                .iter()
                .filter(|n| {
                    matches!(&n.kind, NodeKind::Obj { name, .. }
                        if name == "outlet" || name == "outlet~")
                })
                .count()
                .max(1),
            _ => 1,
        }
    }

    /// Collect all send/receive/value bus entries from the canvas.
    fn collect_bus_map(&self, canvas: &Canvas) -> BTreeMap<String, BusEntry> {
        let mut map: BTreeMap<String, BusEntry> = BTreeMap::new();
        for node in &canvas.nodes {
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
}

/// A send/receive bus entry: who sends, who receives.
#[derive(Default)]
struct BusEntry {
    senders: Vec<u32>,
    receivers: Vec<u32>,
}

/// Extract the expression from `process = <expr>\nwith {...};` or `process = <expr>;`.
/// Used to inline a sub-patch's process as an expression.
fn extract_process_expr(process_block: &str) -> Option<String> {
    // Find `process =`
    let start = process_block.find("process =")? + "process =".len();
    let rest = process_block[start..].trim();
    // Find the end: either `\nwith {` or the final `;`
    let expr = if let Some(with_pos) = rest.find("\nwith {") {
        rest[..with_pos].trim()
    } else if let Some(semi) = rest.rfind(';') {
        rest[..semi].trim()
    } else {
        rest
    };
    if expr.is_empty() {
        None
    } else {
        Some(expr.to_string())
    }
}
