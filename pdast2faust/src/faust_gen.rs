//! Faust DSP code generator from pdast AST.
//!
//! # Architecture
//!
//! Each PD object maps to a named Faust function. The generator:
//!
//! 1. Loads "object templates" from one or more lib directories.
//!    The built-in lib lives in `faust-lib/` (embedded at compile time).
//!    User-supplied lib dirs are searched in order after the built-in.
//!
//! 2. Traverses the canvas graph (nodes + connections), building a Faust
//!    block-diagram composition from the signal flow.
//!
//! 3. Emits a single `.dsp` file containing:
//!    - `import("stdfaust.lib");`
//!    - Inline helper functions for each unique object type used
//!    - The `process` declaration wiring everything together
//!
//! # Faust model
//!
//! PD is a dataflow graph. Faust is also block-diagram dataflow. The mapping
//! is: each PD node becomes a named Faust function; connections become Faust
//! parallel/sequential composition.
//!
//! For simple linear chains: `A → B → C` becomes `A : B : C`.
//! For fan-out (one outlet → multiple inlets): `A <: (B , C)`.
//! For fan-in (multiple outlets → one inlet): `(A , B) :> C`.
//!
//! The generator does a topological sort of the signal graph and emits
//! `let` bindings for each node, then wires them together.
//!
//! # Limitations
//!
//! - Only audio-rate (tilde) objects are mapped. Control-rate objects are
//!   emitted as Faust UI elements where possible, or skipped with a warning.
//! - Complex feedback loops require careful manual review of the generated code.
//! - Objects with no built-in or user-supplied template are emitted as
//!   `/* UNKNOWN: objname */` passthrough stubs.

use std::collections::{HashMap, HashSet, VecDeque};

use pdast::types::{Canvas, Connection, NodeKind, SubPatchContent, Token};

// ── Built-in library (embedded at compile time) ───────────────────────────────

/// Returns a map from PD object name → Faust snippet content (built-in lib).
fn builtin_lib() -> HashMap<&'static str, &'static str> {
    let mut m: HashMap<&'static str, &'static str> = HashMap::new();

    macro_rules! embed {
        ($name:literal, $file:literal) => {
            m.insert($name, include_str!(concat!("../faust-lib/", $file)));
        };
    }

    embed!("osc~", "osc~.dsp");
    embed!("phasor~", "phasor~.dsp");
    embed!("noise~", "noise~.dsp");
    embed!("dac~", "dac~.dsp");
    embed!("adc~", "adc~.dsp");
    embed!("*~", "*~.dsp");
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
    embed!("outlet~", "outlet~.dsp");
    embed!("inlet~", "inlet~.dsp");
    embed!("inlet", "inlet.dsp");
    embed!("outlet", "outlet.dsp");

    m
}

// ── Template loading ──────────────────────────────────────────────────────────

/// Loaded template for a single PD object type.
#[derive(Debug, Clone)]
pub struct ObjectTemplate {
    /// The Faust expression (after `pdobj = ` or the whole snippet).
    pub faust_expr: String,
    /// Whether this object requires the stdfaust.lib import.
    pub needs_stdfaust: bool,
}

/// Resolve object templates from built-in lib + user lib dirs.
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

    /// Look up the template for a PD object name.
    /// User dirs take precedence over built-in.
    pub fn resolve(&mut self, name: &str) -> Option<&ObjectTemplate> {
        if !self.cache.contains_key(name) {
            let tmpl = self.load_template(name);
            self.cache.insert(name.to_string(), tmpl);
        }
        self.cache[name].as_ref()
    }

    fn load_template(&self, name: &str) -> Option<ObjectTemplate> {
        // Try user dirs first
        for dir in &self.user_dirs {
            let path = dir.join(format!("{}.dsp", name));
            if path.exists()
                && let Ok(content) = std::fs::read_to_string(&path)
            {
                return Some(parse_template(&content));
            }
        }
        // Built-in
        if let Some(&content) = self.builtin.get(name) {
            return Some(parse_template(content));
        }
        None
    }
}

/// Parse a `.dsp` snippet into a template.
/// Extracts the `pdobj = ...;` expression.
fn parse_template(content: &str) -> ObjectTemplate {
    let needs_stdfaust = content.contains("stdfaust.lib");

    // Extract the pdobj definition (everything after `pdobj = ` up to `;`)
    let faust_expr = if let Some(pos) = content.find("pdobj") {
        let after = &content[pos..];
        // find the `=`
        if let Some(eq) = after.find('=') {
            let expr_start = &after[eq + 1..];
            // find the terminating `;` (but not inside nested parens)
            let expr = find_statement_end(expr_start.trim());
            expr.trim().to_string()
        } else {
            content.trim().to_string()
        }
    } else {
        // No `pdobj` declaration — use the whole content as the expression
        content.trim().to_string()
    };

    ObjectTemplate {
        faust_expr,
        needs_stdfaust,
    }
}

/// Find the end of a Faust statement, respecting nesting.
fn find_statement_end(s: &str) -> &str {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut last_semi = s.len();
    for (i, c) in s.char_indices() {
        if in_string {
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ';' if depth <= 0 => {
                last_semi = i;
                break;
            }
            _ => {}
        }
    }
    &s[..last_semi]
}

// ── Graph analysis ────────────────────────────────────────────────────────────

/// Determine if an object name is an audio-rate (tilde) object.
fn is_signal(name: &str) -> bool {
    name.ends_with('~') || matches!(name, "inlet~" | "outlet~" | "adc~" | "dac~")
}

/// Topological sort of nodes using Kahn's algorithm.
/// Returns node IDs in evaluation order (sources first).
fn topo_sort(nodes: &[u32], connections: &[Connection]) -> Vec<u32> {
    // Build adjacency: src_node -> dst_node
    let mut in_degree: HashMap<u32, usize> = nodes.iter().map(|&id| (id, 0)).collect();
    let mut adj: HashMap<u32, Vec<u32>> = nodes.iter().map(|&id| (id, vec![])).collect();

    for conn in connections {
        if !in_degree.contains_key(&conn.src_node) || !in_degree.contains_key(&conn.dst_node) {
            continue;
        }
        adj.entry(conn.src_node).or_default().push(conn.dst_node);
        *in_degree.entry(conn.dst_node).or_insert(0) += 1;
    }

    let mut queue: VecDeque<u32> = in_degree
        .iter()
        .filter(|&(_, &d)| d == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut sorted = Vec::new();
    while let Some(node) = queue.pop_front() {
        sorted.push(node);
        if let Some(neighbors) = adj.get(&node) {
            for &next in neighbors {
                let deg = in_degree.entry(next).or_insert(1);
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(next);
                }
            }
        }
    }

    // Append any remaining (cycle nodes)
    for &id in nodes {
        if !sorted.contains(&id) {
            sorted.push(id);
        }
    }
    sorted
}

// ── Code generation ───────────────────────────────────────────────────────────

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

    /// Generate a complete Faust `.dsp` file from a canvas.
    pub fn generate(&mut self, canvas: &Canvas) -> String {
        let mut out = String::new();
        let mut needs_stdfaust = false;
        let mut helper_defs: Vec<String> = Vec::new();
        let mut used_names: HashSet<String> = HashSet::new();

        // Collect signal nodes (tilde objects + special ones)
        let signal_node_ids: Vec<u32> = canvas
            .nodes
            .iter()
            .filter(|n| node_is_signal(n))
            .map(|n| n.id)
            .collect();

        // Only connections between signal nodes
        let signal_conns: Vec<&Connection> = canvas
            .connections
            .iter()
            .filter(|c| {
                signal_node_ids.contains(&c.src_node) && signal_node_ids.contains(&c.dst_node)
            })
            .collect();

        // Topological order
        let order = topo_sort(&signal_node_ids, &canvas.connections);

        // Build helper functions
        for &node_id in &order {
            let node = match canvas.nodes.iter().find(|n| n.id == node_id) {
                Some(n) => n,
                None => continue,
            };

            let obj_name = match &node.kind {
                NodeKind::Obj { name, .. } => name.as_str(),
                NodeKind::SubPatch { .. } => continue, // handled separately
                _ => continue,
            };

            if used_names.contains(obj_name) {
                continue;
            }
            used_names.insert(obj_name.to_string());

            if let Some(tmpl) = self.resolver.resolve(obj_name) {
                if tmpl.needs_stdfaust {
                    needs_stdfaust = true;
                }
                // Emit a helper function: pd_<sanitized_name> = <expr>;
                let fn_name = sanitize_name(obj_name);
                let def = format!("// {} \npd_{} = {};\n", obj_name, fn_name, tmpl.faust_expr);
                helper_defs.push(def);
            } else {
                self.warnings.push(format!(
                    "No Faust template for '{}' (node {}), using passthrough",
                    obj_name, node_id
                ));
                let fn_name = sanitize_name(obj_name);
                helper_defs.push(format!(
                    "// UNKNOWN: {} \npd_{} = _; // passthrough stub\n",
                    obj_name, fn_name
                ));
                used_names.insert(obj_name.to_string());
            }
        }

        // Header
        out.push_str("// Generated by pdast2faust\n");
        if needs_stdfaust {
            out.push_str("import(\"stdfaust.lib\");\n");
        }
        out.push('\n');

        // Helpers
        for h in &helper_defs {
            out.push_str(h);
        }
        out.push('\n');

        // Build the process expression
        let process_expr = self.build_process(canvas, &order, &signal_conns);
        out.push_str(&format!("process = {};\n", process_expr));

        out
    }

    /// Build the Faust `process` expression from the signal graph.
    fn build_process(
        &mut self,
        canvas: &Canvas,
        order: &[u32],
        signal_conns: &[&Connection],
    ) -> String {
        // Find sink nodes (dac~, outlet~, or nodes with no outgoing signal connections)
        let has_outgoing: HashSet<u32> = signal_conns.iter().map(|c| c.src_node).collect();
        let sink_ids: Vec<u32> = order
            .iter()
            .filter(|&&id| !has_outgoing.contains(&id))
            .cloned()
            .collect();

        // Find source nodes (adc~, noise~, osc~ with no incoming, etc.)
        let has_incoming: HashSet<u32> = signal_conns.iter().map(|c| c.dst_node).collect();
        let _source_ids: Vec<u32> = order
            .iter()
            .filter(|&&id| !has_incoming.contains(&id))
            .cloned()
            .collect();

        if order.is_empty() {
            return "0 // empty patch".to_string();
        }

        // Build a per-node Faust expression using let-style binding
        // For a linear graph, we chain with `:`.
        // This is a simplified approach that handles common linear patches well.
        let mut node_exprs: HashMap<u32, String> = HashMap::new();

        for &node_id in order {
            let node = match canvas.nodes.iter().find(|n| n.id == node_id) {
                Some(n) => n,
                None => continue,
            };

            let fn_name = match &node.kind {
                NodeKind::Obj { name, args } => {
                    let sname = sanitize_name(name);
                    // If object has constant args, apply them
                    let const_args = args_as_faust_args(args);
                    if const_args.is_empty() {
                        format!("pd_{sname}")
                    } else {
                        format!("pd_{sname}({const_args})")
                    }
                }
                NodeKind::Gui(_) => {
                    // GUI objects become Faust UI primitives
                    gui_to_faust(node)
                }
                _ => "_".to_string(),
            };

            // Find incoming connections for this node
            let incoming: Vec<&Connection> = signal_conns
                .iter()
                .filter(|c| c.dst_node == node_id)
                .cloned()
                .collect();

            let expr = if incoming.is_empty() {
                fn_name
            } else if incoming.len() == 1 {
                let src_expr = node_exprs
                    .get(&incoming[0].src_node)
                    .cloned()
                    .unwrap_or_else(|| "_".to_string());
                format!("{} : {}", src_expr, fn_name)
            } else {
                // Multiple inputs — parallel merge
                let inputs: Vec<String> = incoming
                    .iter()
                    .map(|c| {
                        node_exprs
                            .get(&c.src_node)
                            .cloned()
                            .unwrap_or_else(|| "_".to_string())
                    })
                    .collect();
                format!("({}) :> {}", inputs.join(","), fn_name)
            };

            node_exprs.insert(node_id, expr);
        }

        // Collect sink expressions
        let sink_exprs: Vec<String> = sink_ids
            .iter()
            .filter_map(|id| node_exprs.get(id))
            .cloned()
            .collect();

        if sink_exprs.is_empty() {
            "0 // no sinks".to_string()
        } else if sink_exprs.len() == 1 {
            sink_exprs[0].clone()
        } else {
            sink_exprs.join(",\n    ")
        }
    }
}

/// Check if a canvas node should be treated as a signal (audio-rate) node.
fn node_is_signal(node: &pdast::types::Node) -> bool {
    match &node.kind {
        NodeKind::Obj { name, .. } => is_signal(name),
        NodeKind::Gui(_) => true, // GUI objects can produce control signals
        NodeKind::SubPatch {
            content: SubPatchContent::Inline(canvas),
            ..
        } => {
            // A subpatch is signal if it contains signal outlets
            canvas
                .nodes
                .iter()
                .any(|n| matches!(&n.kind, NodeKind::Obj { name, .. } if name == "outlet~"))
        }
        _ => false,
    }
}

/// Convert a node id / args list of Tokens to Faust argument string.
fn args_as_faust_args(args: &[Token]) -> String {
    args.iter()
        .filter_map(|t| match t {
            Token::Float(f) => Some(format!("{f}")),
            Token::Symbol(_s) => None, // skip symbol args (names etc.)
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Sanitize a PD object name to a valid Faust identifier.
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '~' => 's', // signal
            '+' => 'p',
            '-' => 'm',
            '*' => 'x',
            '/' => 'd',
            '.' => '_',
            c if c.is_alphanumeric() || c == '_' => c,
            _ => '_',
        })
        .collect()
}

/// Convert a GUI node to a Faust UI primitive.
fn gui_to_faust(node: &pdast::types::Node) -> String {
    use pdast::types::GuiKind;
    match &node.kind {
        NodeKind::Gui(g) => {
            let label = g.label.as_deref().unwrap_or("param");
            match g.kind {
                GuiKind::HSlider | GuiKind::VSlider => format!(
                    "hslider(\"{}\", {}, {}, {}, 0.001)",
                    label, g.default_value, g.min, g.max
                ),
                GuiKind::Toggle => format!("checkbox(\"{}\")", label),
                GuiKind::NumberBox => format!(
                    "nentry(\"{}\", {}, {}, {}, 0.001)",
                    label, g.default_value, g.min, g.max
                ),
                GuiKind::Bang => format!("button(\"{}\")", label),
                GuiKind::HRadio | GuiKind::VRadio => format!(
                    "nentry(\"{}\", {}, {}, {}, 1)",
                    label, g.default_value, g.min, g.max
                ),
                _ => format!("// GUI:{}", label),
            }
        }
        _ => "_".to_string(),
    }
}
