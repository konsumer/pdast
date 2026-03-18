//! PureData `.pd` file parser.
//!
//! Entry point: [`parse_patch`] and [`parse_patch_no_loader`].

// Several handler functions take `&mut Vec<CanvasFrame>` rather than
// `&mut [CanvasFrame]` because some callers (handle_restore, handle_obj)
// need Vec-specific methods (pop, and push via CanvasFrame::push_node on
// the last element). Keeping all handler signatures uniform avoids churn.
#![allow(clippy::ptr_arg)]

pub mod gui;
pub mod message;

use crate::error::ParseError;
use crate::types::{
    Canvas, Connection, Coords, Node, NodeKind, ParseResult, Patch, SubPatchContent, Token, Warning,
};
use gui::try_parse_gui;
use message::{parse_atom, parse_message_content};

// ── Record splitting ──────────────────────────────────────────────────────────

/// A raw parsed record from a `.pd` file.
#[derive(Debug)]
struct Record {
    /// `N`, `X`, or `A`
    chunk: char,
    /// Everything after `#X ` / `#N ` / `#A ` up to (but not including) the
    /// terminating `;`.
    body: String,
}

/// Split a `.pd` file into records, handling escaped `;` and `,`.
fn split_records(input: &str) -> Vec<Record> {
    let mut records = Vec::new();
    let mut current_chunk: Option<char> = None;
    let mut current_body = String::new();
    let mut chars = input.chars().peekable();
    // Track whether we are at a position where '#' can start a new record.
    // In PD files, '#N', '#X', '#A' always appear at the start of a line.
    // Inside a record body (e.g. plugdata hex colors like #191919) '#' must
    // NOT be treated as a record starter.
    let mut at_line_start = true;

    while let Some(c) = chars.next() {
        match c {
            '#' if at_line_start || current_chunk.is_none() => {
                // Start of a new record — only valid at line start or before
                // any record has been opened.
                if let Some(chunk_char) = chars.next() {
                    let chunk = chunk_char.to_ascii_uppercase();
                    // Skip whitespace after chunk identifier
                    while chars.peek() == Some(&' ') || chars.peek() == Some(&'\t') {
                        chars.next();
                    }
                    current_chunk = Some(chunk);
                    current_body.clear();
                    at_line_start = false;
                }
            }
            '#' => {
                // Mid-record '#' — treat as a literal character (e.g. hex color)
                if current_chunk.is_some() {
                    current_body.push('#');
                }
                at_line_start = false;
            }
            ';' => {
                // Record terminator (unescaped)
                if let Some(chunk) = current_chunk.take() {
                    let body = current_body.trim_end().to_string();
                    if !body.is_empty() || chunk == 'A' {
                        records.push(Record { chunk, body });
                    }
                    current_body.clear();
                }
                at_line_start = false; // ';' is followed by \r\n which sets it
            }
            '\\' => {
                // Escape sequence — pass through verbatim so atom parser sees it
                current_body.push('\\');
                if let Some(next) = chars.next() {
                    current_body.push(next);
                }
                at_line_start = false;
            }
            '\r' | '\n' => {
                // Line endings: next non-whitespace may be a record starter
                at_line_start = true;
                // Line endings within a record body become spaces
                if current_chunk.is_some() {
                    // Collapse multiple whitespace into one space
                    let last = current_body.chars().last();
                    if last != Some(' ') && last.is_some() {
                        current_body.push(' ');
                    }
                }
            }
            _ => {
                if current_chunk.is_some() {
                    current_body.push(c);
                }
                if !c.is_whitespace() {
                    at_line_start = false;
                }
            }
        }
    }

    records
}

// ── Canvas stack ──────────────────────────────────────────────────────────────

/// In-progress canvas during parsing.
struct CanvasFrame {
    canvas: Canvas,
    /// Next object id to assign.
    next_id: u32,
    /// If this frame is for an array graph, hold the most recent array node id.
    pending_array_id: Option<u32>,
}

impl CanvasFrame {
    fn new(canvas: Canvas) -> Self {
        CanvasFrame {
            canvas,
            next_id: 0,
            pending_array_id: None,
        }
    }

    /// Push a node and return its assigned id.
    fn push_node(&mut self, x: i32, y: i32, kind: NodeKind) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.canvas.nodes.push(Node { id, x, y, kind });
        id
    }
}

// ── Main parser ───────────────────────────────────────────────────────────────

/// Parse a PureData patch from a string.
///
/// `loader` is called with an abstraction name (e.g. `"my-filter"`) and should
/// return the `.pd` file content, or `None` if unavailable. Pass
/// `|_| None` to disable abstraction resolution.
pub fn parse_patch<F>(content: &str, loader: F) -> Result<ParseResult, ParseError>
where
    F: Fn(&str) -> Option<String>,
{
    parse_patch_dyn(content, &loader)
}

fn parse_patch_dyn(
    content: &str,
    loader: &dyn Fn(&str) -> Option<String>,
) -> Result<ParseResult, ParseError> {
    let records = split_records(content);
    let mut warnings: Vec<Warning> = Vec::new();
    let mut stack: Vec<CanvasFrame> = Vec::new();

    // We need to track whether we have seen the first record (root canvas).
    let mut is_root = true;

    for record in &records {
        match record.chunk {
            'N' => {
                // #N canvas x y width height [name open_on_load]
                let parts: Vec<&str> = record.body.splitn(2, "canvas").collect();
                if parts.len() < 2 {
                    warnings.push(Warning {
                        node_id: None,
                        message: format!("Unknown #N record: {}", record.body),
                    });
                    continue;
                }
                let args: Vec<&str> = parts[1].split_whitespace().collect();
                let canvas = parse_canvas_header(&args, is_root, &mut warnings);
                is_root = false;
                stack.push(CanvasFrame::new(canvas));
            }

            'X' => {
                let body = record.body.trim();
                // Split into element type + rest
                let (elem, rest) = split_first_word(body);

                match elem {
                    "obj" => handle_obj(rest, &mut stack, &loader, &mut warnings),
                    "msg" => handle_msg(rest, &mut stack, &mut warnings),
                    "floatatom" => handle_floatatom(rest, &mut stack, &mut warnings),
                    "symbolatom" => handle_symbolatom(rest, &mut stack, &mut warnings),
                    "text" => handle_text(rest, &mut stack, &mut warnings),
                    "connect" => handle_connect(rest, &mut stack, &mut warnings),
                    "restore" => handle_restore(rest, &mut stack, &mut warnings),
                    "coords" => handle_coords(rest, &mut stack, &mut warnings),
                    "array" => handle_array(rest, &mut stack, &mut warnings),
                    other => {
                        warnings.push(Warning {
                            node_id: None,
                            message: format!("Unknown #X element type: {other}"),
                        });
                    }
                }
            }

            'A' => {
                // Array data: #A <start_index> <v1> <v2> ...
                //          or #A resize <size>
                let body = record.body.trim();
                if let Some(frame) = stack.last_mut()
                    && let Some(aid) = frame.pending_array_id
                {
                    let parts: Vec<&str> = body.split_whitespace().collect();
                    if parts.first() != Some(&"resize") {
                        // Find the array node and append data
                        let start: usize = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                        let values: Vec<f64> =
                            parts[1..].iter().filter_map(|s| s.parse().ok()).collect();
                        if let Some(node) = frame.canvas.nodes.iter_mut().find(|n| n.id == aid)
                            && let NodeKind::Array { data, .. } = &mut node.kind
                        {
                            // Ensure capacity
                            if data.len() < start + values.len() {
                                data.resize(start + values.len(), 0.0);
                            }
                            for (i, v) in values.into_iter().enumerate() {
                                data[start + i] = v;
                            }
                        }
                    }
                }
            }

            _ => {}
        }
    }

    // The root canvas should be the only frame left
    if stack.len() != 1 {
        if stack.is_empty() {
            return Err(ParseError::NoRootCanvas);
        }
        warnings.push(Warning {
            node_id: None,
            message: format!(
                "Unexpected canvas stack depth {} at end of file (expected 1)",
                stack.len()
            ),
        });
    }

    let root_frame = stack.remove(0);
    Ok(ParseResult {
        patch: Patch {
            root: root_frame.canvas,
        },
        warnings,
    })
}

/// Parse without any abstraction loader.
pub fn parse_patch_no_loader(content: &str) -> Result<ParseResult, ParseError> {
    parse_patch(content, |_| None)
}

// ── Canvas header ─────────────────────────────────────────────────────────────

fn parse_canvas_header(args: &[&str], is_root: bool, _warnings: &mut Vec<Warning>) -> Canvas {
    let x = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let y = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let width = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400);
    let height = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(300);

    if is_root {
        let font_size = args.get(4).and_then(|s| s.parse().ok());
        Canvas {
            x,
            y,
            width,
            height,
            font_size,
            name: None,
            open_on_load: false,
            coords: None,
            nodes: Vec::new(),
            connections: Vec::new(),
        }
    } else {
        let name = args.get(4).map(|s| s.to_string());
        let open_on_load = args
            .get(5)
            .and_then(|s| s.parse::<u32>().ok())
            .map(|v| v != 0)
            .unwrap_or(false);
        Canvas {
            x,
            y,
            width,
            height,
            font_size: None,
            name,
            open_on_load,
            coords: None,
            nodes: Vec::new(),
            connections: Vec::new(),
        }
    }
}

// ── Element handlers ──────────────────────────────────────────────────────────

fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    if let Some(pos) = s.find(|c: char| c.is_ascii_whitespace()) {
        (&s[..pos], s[pos..].trim_start())
    } else {
        (s, "")
    }
}

/// Parse `x y` from the front of a string, returning the remainder.
fn parse_xy<'a>(s: &'a str, warnings: &mut Vec<Warning>) -> (i32, i32, &'a str) {
    let (xstr, rest) = split_first_word(s);
    let (ystr, rest) = split_first_word(rest);
    let x = xstr.parse().unwrap_or_else(|_| {
        warnings.push(Warning {
            node_id: None,
            message: format!("Bad x coord: {xstr}"),
        });
        0
    });
    let y = ystr.parse().unwrap_or_else(|_| {
        warnings.push(Warning {
            node_id: None,
            message: format!("Bad y coord: {ystr}"),
        });
        0
    });
    (x, y, rest)
}

fn handle_obj(
    rest: &str,
    stack: &mut Vec<CanvasFrame>,
    loader: &dyn Fn(&str) -> Option<String>,
    warnings: &mut Vec<Warning>,
) {
    let (x, y, rest) = parse_xy(rest, warnings);
    let frame = match stack.last_mut() {
        Some(f) => f,
        None => {
            warnings.push(Warning {
                node_id: None,
                message: "obj outside canvas".into(),
            });
            return;
        }
    };

    // rest is now: name args...  (or empty for a broken box)
    if rest.is_empty() {
        frame.push_node(
            x,
            y,
            NodeKind::Unknown {
                name: None,
                args: vec![],
            },
        );
        return;
    }

    let (name, args_str) = split_first_word(rest);
    let raw_args: Vec<&str> = args_str.split_whitespace().collect();

    // Check IEM GUI objects first
    if let Some(gui) = try_parse_gui(name, &raw_args) {
        frame.push_node(x, y, NodeKind::Gui(gui));
        return;
    }

    // Check if this is a potential abstraction by trying the loader
    let args: Vec<Token> = raw_args.iter().map(|s| parse_atom(s)).collect();

    if let Some(content) = loader(name) {
        // Recursively parse the abstraction
        match parse_patch_dyn(&content, loader) {
            Ok(result) => {
                for w in result.warnings {
                    warnings.push(Warning {
                        node_id: None,
                        message: format!("[{}]: {}", name, w.message),
                    });
                }
                let frame = stack.last_mut().unwrap();
                frame.push_node(
                    x,
                    y,
                    NodeKind::SubPatch {
                        name: name.to_string(),
                        args,
                        content: SubPatchContent::Inline(Box::new(result.patch.root)),
                    },
                );
            }
            Err(e) => {
                warnings.push(Warning {
                    node_id: None,
                    message: format!("Failed to parse abstraction '{}': {}", name, e),
                });
                let frame = stack.last_mut().unwrap();
                frame.push_node(
                    x,
                    y,
                    NodeKind::Unknown {
                        name: Some(name.to_string()),
                        args,
                    },
                );
            }
        }
        return;
    }

    // Unknown / external / vanilla with no loader match
    frame.push_node(
        x,
        y,
        NodeKind::Obj {
            name: name.to_string(),
            args,
        },
    );
}

fn handle_msg(rest: &str, stack: &mut Vec<CanvasFrame>, warnings: &mut Vec<Warning>) {
    let (x, y, content) = parse_xy(rest, warnings);
    let messages = parse_message_content(content);
    if let Some(frame) = stack.last_mut() {
        frame.push_node(x, y, NodeKind::Msg { messages });
    }
}

fn get_arg<'a>(a: &[&'a str], i: usize, default: &'a str) -> &'a str {
    a.get(i).copied().unwrap_or(default)
}

fn opt_str_pd(s: &str) -> Option<String> {
    if s == "-" || s == "empty" {
        None
    } else {
        Some(s.to_string())
    }
}

fn handle_floatatom(rest: &str, stack: &mut Vec<CanvasFrame>, warnings: &mut Vec<Warning>) {
    let (x, y, rest) = parse_xy(rest, warnings);
    let a: Vec<&str> = rest.split_whitespace().collect();

    let kind = NodeKind::FloatAtom {
        width: get_arg(&a, 0, "5").parse().unwrap_or(5),
        min: get_arg(&a, 1, "0").parse().unwrap_or(0.0),
        max: get_arg(&a, 2, "0").parse().unwrap_or(0.0),
        label_pos: get_arg(&a, 3, "0").parse().unwrap_or(0),
        label: opt_str_pd(get_arg(&a, 4, "-")),
        receive: opt_str_pd(get_arg(&a, 5, "-")),
        send: opt_str_pd(get_arg(&a, 6, "-")),
    };
    if let Some(frame) = stack.last_mut() {
        frame.push_node(x, y, kind);
    }
}

fn handle_symbolatom(rest: &str, stack: &mut Vec<CanvasFrame>, warnings: &mut Vec<Warning>) {
    let (x, y, rest) = parse_xy(rest, warnings);
    let a: Vec<&str> = rest.split_whitespace().collect();

    let kind = NodeKind::SymbolAtom {
        width: get_arg(&a, 0, "10").parse().unwrap_or(10),
        label_pos: get_arg(&a, 3, "0").parse().unwrap_or(0),
        label: opt_str_pd(get_arg(&a, 4, "-")),
        receive: opt_str_pd(get_arg(&a, 5, "-")),
        send: opt_str_pd(get_arg(&a, 6, "-")),
    };
    if let Some(frame) = stack.last_mut() {
        frame.push_node(x, y, kind);
    }
}

fn handle_text(rest: &str, stack: &mut Vec<CanvasFrame>, warnings: &mut Vec<Warning>) {
    let (x, y, content) = parse_xy(rest, warnings);
    if let Some(frame) = stack.last_mut() {
        frame.push_node(
            x,
            y,
            NodeKind::Text {
                content: content.to_string(),
            },
        );
    }
}

fn handle_connect(rest: &str, stack: &mut Vec<CanvasFrame>, warnings: &mut Vec<Warning>) {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 4 {
        warnings.push(Warning {
            node_id: None,
            message: format!("Bad connect record: {rest}"),
        });
        return;
    }
    let conn = Connection {
        src_node: parts[0].parse().unwrap_or(0),
        src_outlet: parts[1].parse().unwrap_or(0),
        dst_node: parts[2].parse().unwrap_or(0),
        dst_inlet: parts[3].parse().unwrap_or(0),
    };
    if let Some(frame) = stack.last_mut() {
        frame.canvas.connections.push(conn);
    }
}

fn handle_restore(rest: &str, stack: &mut Vec<CanvasFrame>, warnings: &mut Vec<Warning>) {
    // #X restore <x> <y> <type> [name]
    let (x, y, rest) = parse_xy(rest, warnings);
    let (restore_type, name_str) = split_first_word(rest);

    if stack.len() < 2 {
        warnings.push(Warning {
            node_id: None,
            message: format!(
                "restore without matching canvas: {} {}",
                restore_type, name_str
            ),
        });
        return;
    }

    let finished = stack.pop().unwrap();
    let parent = stack.last_mut().unwrap();

    match restore_type {
        "pd" => {
            // The inline subpatch name comes from the finished canvas's name field.
            // In PD, the flow is:
            //   #N canvas ... name open_on_load  ← opens sub-canvas (name stored there)
            //   ... nodes ...
            //   #X restore x y pd [name]         ← closes it, places in parent
            //
            // There is NO separate `#X obj pd name` record for inline subpatches.
            // Just push the finished canvas as a SubPatch node in the parent.
            let name = finished.canvas.name.clone().unwrap_or_default();
            parent.push_node(
                x,
                y,
                NodeKind::SubPatch {
                    name,
                    args: vec![],
                    content: SubPatchContent::Inline(Box::new(finished.canvas)),
                },
            );
        }
        "graph" => {
            parent.pending_array_id = None;
            parent.push_node(
                x,
                y,
                NodeKind::Graph {
                    content: Box::new(finished.canvas),
                },
            );
        }
        other => {
            warnings.push(Warning {
                node_id: None,
                message: format!("Unknown restore type: {other}"),
            });
        }
    }
}

fn handle_coords(rest: &str, stack: &mut Vec<CanvasFrame>, warnings: &mut Vec<Warning>) {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 7 {
        warnings.push(Warning {
            node_id: None,
            message: format!("Bad coords record: {rest}"),
        });
        return;
    }
    let coords = Coords {
        x_from: parts[0].parse().unwrap_or(0.0),
        y_top: parts[1].parse().unwrap_or(1.0),
        x_to: parts[2].parse().unwrap_or(100.0),
        y_bottom: parts[3].parse().unwrap_or(-1.0),
        width_px: parts[4].parse().unwrap_or(200),
        height_px: parts[5].parse().unwrap_or(140),
        gop: parts[6].parse::<u32>().unwrap_or(0) != 0,
        x_margin: parts.get(7).and_then(|s| s.parse().ok()),
        y_margin: parts.get(8).and_then(|s| s.parse().ok()),
    };
    if let Some(frame) = stack.last_mut() {
        frame.canvas.coords = Some(coords);
    }
}

fn handle_array(rest: &str, stack: &mut Vec<CanvasFrame>, warnings: &mut Vec<Warning>) {
    // #X array <name> <size> float <flags>
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 4 {
        warnings.push(Warning {
            node_id: None,
            message: format!("Bad array record: {rest}"),
        });
        return;
    }
    let name = parts[0].to_string();
    let size: u32 = parts[1].parse().unwrap_or(0);
    let data_type = parts[2].to_string();
    let flags: u32 = parts[3].parse().unwrap_or(0);

    // Pre-allocate data only if save-data flag is set
    let data = if flags & 1 != 0 {
        vec![0.0f64; size as usize]
    } else {
        vec![]
    };

    if let Some(frame) = stack.last_mut() {
        let id = frame.push_node(
            0,
            0,
            NodeKind::Array {
                name,
                size,
                data_type,
                flags,
                data,
            },
        );
        frame.pending_array_id = Some(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_PATCH: &str = "#N canvas 0 50 450 300 12;\r\n\
                                 #X obj 30 27 osc~ 440;\r\n\
                                 #X obj 30 60 dac~;\r\n\
                                 #X connect 0 0 1 0;\r\n\
                                 #X connect 0 0 1 1;\r\n";

    #[test]
    fn test_simple_parse() {
        let r = parse_patch_no_loader(SIMPLE_PATCH).unwrap();
        assert_eq!(r.patch.root.nodes.len(), 2);
        assert_eq!(r.patch.root.connections.len(), 2);
        assert!(r.warnings.is_empty());
        assert_eq!(r.patch.root.font_size, Some(12));
    }

    #[test]
    fn test_node_ids() {
        let r = parse_patch_no_loader(SIMPLE_PATCH).unwrap();
        assert_eq!(r.patch.root.nodes[0].id, 0);
        assert_eq!(r.patch.root.nodes[1].id, 1);
    }

    #[test]
    fn test_connections() {
        let r = parse_patch_no_loader(SIMPLE_PATCH).unwrap();
        assert_eq!(
            r.patch.root.connections[0],
            Connection {
                src_node: 0,
                src_outlet: 0,
                dst_node: 1,
                dst_inlet: 0
            }
        );
        assert_eq!(
            r.patch.root.connections[1],
            Connection {
                src_node: 0,
                src_outlet: 0,
                dst_node: 1,
                dst_inlet: 1
            }
        );
    }

    #[test]
    fn test_text_comment() {
        let patch = "#N canvas 0 0 450 300 12;\r\n\
                     #X text 10 10 hello world;\r\n";
        let r = parse_patch_no_loader(patch).unwrap();
        assert_eq!(r.patch.root.nodes.len(), 1);
        match &r.patch.root.nodes[0].kind {
            NodeKind::Text { content } => assert_eq!(content, "hello world"),
            _ => panic!("Expected Text node"),
        }
    }

    #[test]
    fn test_msg_box() {
        let patch = "#N canvas 0 0 450 300 12;\r\n\
                     #X msg 50 80 440;\r\n";
        let r = parse_patch_no_loader(patch).unwrap();
        assert_eq!(r.patch.root.nodes.len(), 1);
        match &r.patch.root.nodes[0].kind {
            NodeKind::Msg { messages } => {
                assert_eq!(messages.len(), 1);
                assert_eq!(messages[0], vec![Token::Float(440.0)]);
            }
            _ => panic!("Expected Msg node"),
        }
    }

    #[test]
    fn test_subpatch() {
        let patch = "#N canvas 0 0 450 300 12;\r\n\
                     #N canvas 0 0 450 300 inner 0;\r\n\
                     #X obj 10 10 inlet;\r\n\
                     #X obj 10 40 outlet;\r\n\
                     #X connect 0 0 1 0;\r\n\
                     #X restore 100 50 pd inner;\r\n";
        let r = parse_patch_no_loader(patch).unwrap();
        assert_eq!(r.patch.root.nodes.len(), 1);
        match &r.patch.root.nodes[0].kind {
            NodeKind::SubPatch { name, content, .. } => {
                assert_eq!(name, "inner");
                match content {
                    SubPatchContent::Inline(canvas) => {
                        assert_eq!(canvas.nodes.len(), 2);
                        assert_eq!(canvas.connections.len(), 1);
                    }
                    _ => panic!("Expected Inline content"),
                }
            }
            _ => panic!("Expected SubPatch node"),
        }
    }

    #[test]
    fn test_loader() {
        let abstraction = "#N canvas 0 0 450 300 12;\r\n\
                           #X obj 10 10 inlet;\r\n\
                           #X obj 10 40 *~ 2;\r\n\
                           #X obj 10 70 outlet;\r\n\
                           #X connect 0 0 1 0;\r\n\
                           #X connect 1 0 2 0;\r\n";
        let patch = "#N canvas 0 0 450 300 12;\r\n\
                     #X obj 50 50 my-double;\r\n";
        let r = parse_patch(patch, |name| {
            if name == "my-double" {
                Some(abstraction.to_string())
            } else {
                None
            }
        })
        .unwrap();
        assert_eq!(r.patch.root.nodes.len(), 1);
        match &r.patch.root.nodes[0].kind {
            NodeKind::SubPatch { name, content, .. } => {
                assert_eq!(name, "my-double");
                assert!(matches!(content, SubPatchContent::Inline(_)));
            }
            _ => panic!("Expected SubPatch"),
        }
    }

    #[test]
    fn test_floatatom() {
        let patch = "#N canvas 0 0 450 300 12;\r\n\
                     #X floatatom 32 26 5 0 0 0 - - -;\r\n";
        let r = parse_patch_no_loader(patch).unwrap();
        assert_eq!(r.patch.root.nodes.len(), 1);
        assert!(matches!(
            r.patch.root.nodes[0].kind,
            NodeKind::FloatAtom { width: 5, .. }
        ));
    }

    #[test]
    fn test_array() {
        let patch = "#N canvas 0 0 450 300 12;\r\n\
                     #N canvas 0 0 200 140 graph1 0;\r\n\
                     #X array myarray 4 float 3;\r\n\
                     #A 0 0.1 0.5 0.9 1.0;\r\n\
                     #X coords 0 1 3 -1 200 140 0;\r\n\
                     #X restore 50 100 graph;\r\n";
        let r = parse_patch_no_loader(patch).unwrap();
        assert_eq!(r.patch.root.nodes.len(), 1);
        match &r.patch.root.nodes[0].kind {
            NodeKind::Graph { content } => {
                assert_eq!(content.nodes.len(), 1);
                match &content.nodes[0].kind {
                    NodeKind::Array {
                        name, size, data, ..
                    } => {
                        assert_eq!(name, "myarray");
                        assert_eq!(*size, 4);
                        assert_eq!(data.len(), 4);
                    }
                    _ => panic!("Expected Array node inside graph"),
                }
            }
            _ => panic!("Expected Graph node"),
        }
    }
}
