//! Emit a `Patch` AST back to PureData `.pd` text format.

use crate::parse::gui::emit_gui_args;
use crate::parse::message::emit_tokens;
use crate::types::*;

const CRLF: &str = "\r\n";

/// Serialize a `Patch` back to a `.pd` file string.
pub fn emit_patch(patch: &Patch) -> String {
    let mut out = String::new();
    emit_canvas(&patch.root, true, &mut out);
    out
}

fn emit_canvas(canvas: &Canvas, is_root: bool, out: &mut String) {
    // #N canvas record
    if is_root {
        out.push_str(&format!(
            "#N canvas {} {} {} {} {};{CRLF}",
            canvas.x,
            canvas.y,
            canvas.width,
            canvas.height,
            canvas.font_size.unwrap_or(12),
        ));
    } else {
        out.push_str(&format!(
            "#N canvas {} {} {} {} {} {};{CRLF}",
            canvas.x,
            canvas.y,
            canvas.width,
            canvas.height,
            canvas.name.as_deref().unwrap_or(""),
            canvas.open_on_load as u8,
        ));
    }

    // Nodes
    for node in &canvas.nodes {
        emit_node(node, out);
    }

    // Coords (if present) — emitted before connections
    if let Some(coords) = &canvas.coords {
        emit_coords(coords, out);
    }

    // Connections
    for conn in &canvas.connections {
        out.push_str(&format!(
            "#X connect {} {} {} {};{CRLF}",
            conn.src_node, conn.src_outlet, conn.dst_node, conn.dst_inlet
        ));
    }
}

fn emit_node(node: &Node, out: &mut String) {
    match &node.kind {
        NodeKind::Obj { name, args } => {
            let args_str = if args.is_empty() {
                String::new()
            } else {
                format!(" {}", emit_tokens(args))
            };
            out.push_str(&format!(
                "#X obj {} {} {}{};{CRLF}",
                node.x, node.y, name, args_str
            ));
        }

        NodeKind::Msg { messages } => {
            // Join messages with " \; "
            let content = messages
                .iter()
                .map(|m| emit_tokens(m))
                .collect::<Vec<_>>()
                .join(" \\; ");
            out.push_str(&format!("#X msg {} {} {};{CRLF}", node.x, node.y, content));
        }

        NodeKind::FloatAtom {
            width,
            min,
            max,
            label_pos,
            label,
            receive,
            send,
        } => {
            let lbl = label.as_deref().unwrap_or("-");
            let rcv = receive.as_deref().unwrap_or("-");
            let snd = send.as_deref().unwrap_or("-");
            out.push_str(&format!(
                "#X floatatom {} {} {} {} {} {} {} {} {};{CRLF}",
                node.x, node.y, width, min, max, label_pos, lbl, rcv, snd
            ));
        }

        NodeKind::SymbolAtom {
            width,
            label_pos,
            label,
            receive,
            send,
        } => {
            let lbl = label.as_deref().unwrap_or("-");
            let rcv = receive.as_deref().unwrap_or("-");
            let snd = send.as_deref().unwrap_or("-");
            out.push_str(&format!(
                "#X symbolatom {} {} {} 0 0 {} {} {} {};{CRLF}",
                node.x, node.y, width, label_pos, lbl, rcv, snd
            ));
        }

        NodeKind::Text { content } => {
            out.push_str(&format!("#X text {} {} {};{CRLF}", node.x, node.y, content));
        }

        NodeKind::SubPatch {
            name,
            args,
            content,
        } => {
            match content {
                SubPatchContent::Inline(inner_canvas) => {
                    // Emit the inner canvas first (it opens a new #N context)
                    emit_canvas(inner_canvas, false, out);
                    // Then restore it into the parent
                    let args_str = if args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", emit_tokens(args))
                    };
                    out.push_str(&format!(
                        "#X restore {} {} pd {}{};{CRLF}",
                        node.x, node.y, name, args_str
                    ));
                }
                SubPatchContent::Unresolved => {
                    // Emit as a plain object — the abstraction body is not available
                    let args_str = if args.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", emit_tokens(args))
                    };
                    out.push_str(&format!(
                        "#X obj {} {} {}{};{CRLF}",
                        node.x, node.y, name, args_str
                    ));
                }
            }
        }

        NodeKind::Graph { content } => {
            emit_canvas(content, false, out);
            out.push_str(&format!("#X restore {} {} graph;{CRLF}", node.x, node.y));
        }

        NodeKind::Gui(gui) => {
            let gui_name = match gui.kind {
                GuiKind::Bang => "bng",
                GuiKind::Toggle => "tgl",
                GuiKind::NumberBox => "nbx",
                GuiKind::HSlider => "hsl",
                GuiKind::VSlider => "vsl",
                GuiKind::HRadio => "hradio",
                GuiKind::VRadio => "vradio",
                GuiKind::Vu => "vu",
                GuiKind::Canvas => "cnv",
            };
            out.push_str(&format!(
                "#X obj {} {} {} {};{CRLF}",
                node.x,
                node.y,
                gui_name,
                emit_gui_args(gui)
            ));
        }

        NodeKind::Array {
            name,
            size,
            data_type,
            flags,
            data,
        } => {
            out.push_str(&format!(
                "#X array {} {} {} {};{CRLF}",
                name, size, data_type, flags
            ));
            // Emit saved data in chunks of 1000
            if flags & 1 != 0 && !data.is_empty() {
                let chunk_size = 1000;
                for (chunk_idx, chunk) in data.chunks(chunk_size).enumerate() {
                    let start = chunk_idx * chunk_size;
                    let values = chunk
                        .iter()
                        .map(|v| format!("{v}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    out.push_str(&format!("#A {} {};{CRLF}", start, values));
                }
            }
        }

        NodeKind::Unknown { name, args } => {
            let name_str = name.as_deref().unwrap_or("");
            let args_str = if args.is_empty() {
                String::new()
            } else {
                format!(" {}", emit_tokens(args))
            };
            out.push_str(&format!(
                "#X obj {} {} {}{};{CRLF}",
                node.x, node.y, name_str, args_str
            ));
        }
    }
}

fn emit_coords(c: &Coords, out: &mut String) {
    match (c.x_margin, c.y_margin) {
        (Some(xm), Some(ym)) => {
            out.push_str(&format!(
                "#X coords {} {} {} {} {} {} {} {} {};{CRLF}",
                c.x_from, c.y_top, c.x_to, c.y_bottom, c.width_px, c.height_px, c.gop as u8, xm, ym
            ));
        }
        _ => {
            out.push_str(&format!(
                "#X coords {} {} {} {} {} {} {};{CRLF}",
                c.x_from, c.y_top, c.x_to, c.y_bottom, c.width_px, c.height_px, c.gop as u8
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_patch_no_loader;

    fn roundtrip(input: &str) -> String {
        let result = parse_patch_no_loader(input).expect("parse failed");
        emit_patch(&result.patch)
    }

    #[test]
    fn test_simple_roundtrip() {
        let input = "#N canvas 0 50 450 300 12;\r\n\
                     #X obj 30 27 osc~ 440;\r\n\
                     #X obj 30 60 dac~;\r\n\
                     #X connect 0 0 1 0;\r\n\
                     #X connect 0 0 1 1;\r\n";
        let output = roundtrip(input);
        // Re-parse the output and check structure
        let result2 = parse_patch_no_loader(&output).expect("re-parse failed");
        let r1 = parse_patch_no_loader(input).unwrap();
        assert_eq!(r1.patch, result2.patch);
    }

    #[test]
    fn test_msg_roundtrip() {
        let input = "#N canvas 0 0 450 300 12;\r\n\
                     #X msg 50 80 440;\r\n";
        let r1 = parse_patch_no_loader(input).unwrap();
        let out = emit_patch(&r1.patch);
        let r2 = parse_patch_no_loader(&out).unwrap();
        assert_eq!(r1.patch, r2.patch);
    }

    #[test]
    fn test_subpatch_roundtrip() {
        let input = "#N canvas 0 0 450 300 12;\r\n\
                     #N canvas 0 0 200 150 inner 0;\r\n\
                     #X obj 10 10 inlet;\r\n\
                     #X obj 10 40 outlet;\r\n\
                     #X connect 0 0 1 0;\r\n\
                     #X restore 100 50 pd inner;\r\n";
        let r1 = parse_patch_no_loader(input).unwrap();
        let out = emit_patch(&r1.patch);
        let r2 = parse_patch_no_loader(&out).unwrap();
        assert_eq!(r1.patch, r2.patch);
    }
}
