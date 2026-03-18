//! Integration tests using fixture .pd files.

use pdast::types::{NodeKind, SubPatchContent};
use pdast::{emit_patch, from_json, parse_patch, parse_patch_no_loader, to_json};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Cannot read fixture {name}: {e}"))
}

fn fixture_loader(_base: &str) -> impl Fn(&str) -> Option<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/fixtures");
    let dir = dir.to_path_buf();
    move |name: &str| {
        let path = dir.join(format!("{name}.pd"));
        std::fs::read_to_string(&path).ok()
    }
}

// ── Fixture parsing tests ─────────────────────────────────────────────────────

#[test]
fn test_sine_patch() {
    let src = fixture("sine.pd");
    let result = parse_patch_no_loader(&src).unwrap();
    let root = &result.patch.root;

    // 3 signal nodes + 1 text comment = 4 nodes
    assert_eq!(root.nodes.len(), 4);

    // Node 0 is osc~ 440
    match &root.nodes[0].kind {
        NodeKind::Obj { name, args } => {
            assert_eq!(name, "osc~");
            assert_eq!(args.len(), 1);
        }
        _ => panic!("Expected Obj at node 0"),
    }

    // Node 1 is *~ 0.5
    match &root.nodes[1].kind {
        NodeKind::Obj { name, args } => {
            assert_eq!(name, "*~");
            assert_eq!(args.len(), 1);
        }
        _ => panic!("Expected Obj at node 1"),
    }

    // 3 connections
    assert_eq!(root.connections.len(), 3);
    assert!(result.warnings.is_empty());
}

#[test]
fn test_subpatch_inline() {
    let src = fixture("subpatch.pd");
    let result = parse_patch_no_loader(&src).unwrap();
    let root = &result.patch.root;

    // Find the subpatch node
    let subpatch_node = root
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::SubPatch { .. }))
        .expect("Expected a SubPatch node");

    match &subpatch_node.kind {
        NodeKind::SubPatch { name, content, .. } => {
            assert_eq!(name, "gain");
            match content {
                SubPatchContent::Inline(canvas) => {
                    // inlet~, *~, outlet~
                    assert_eq!(canvas.nodes.len(), 3);
                    assert_eq!(canvas.connections.len(), 2);
                }
                _ => panic!("Expected Inline content"),
            }
        }
        _ => panic!("Expected SubPatch node"),
    }
}

#[test]
fn test_abstraction_loader() {
    let src = fixture("abstraction.pd");
    let result = parse_patch(&src, fixture_loader("abstraction")).unwrap();
    let root = &result.patch.root;

    // mygain should be resolved as SubPatch::Inline
    let mygain_node = root
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::SubPatch { name, .. } if name == "mygain"))
        .expect("Expected mygain SubPatch node");

    match &mygain_node.kind {
        NodeKind::SubPatch { content, .. } => {
            assert!(
                matches!(content, SubPatchContent::Inline(_)),
                "mygain should be resolved Inline"
            );
        }
        _ => panic!("Expected SubPatch"),
    }
}

#[test]
fn test_gui_objects() {
    let src = fixture("gui.pd");
    let result = parse_patch_no_loader(&src).unwrap();
    let root = &result.patch.root;

    // Should have: osc~, bng, tgl, hsl, nbx, dac~
    assert_eq!(root.nodes.len(), 6);

    let gui_nodes: Vec<_> = root
        .nodes
        .iter()
        .filter(|n| matches!(&n.kind, NodeKind::Gui(_)))
        .collect();
    assert_eq!(
        gui_nodes.len(),
        4,
        "Expected 4 GUI nodes (bng, tgl, hsl, nbx)"
    );
    assert!(result.warnings.is_empty());
}

#[test]
fn test_array_data() {
    let src = fixture("array.pd");
    let result = parse_patch_no_loader(&src).unwrap();
    let root = &result.patch.root;

    // Find the graph node
    let graph_node = root
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Graph { .. }))
        .expect("Expected Graph node");

    match &graph_node.kind {
        NodeKind::Graph { content } => {
            assert_eq!(content.nodes.len(), 1);
            match &content.nodes[0].kind {
                NodeKind::Array {
                    name,
                    size,
                    data,
                    flags,
                    ..
                } => {
                    assert_eq!(name, "mywave");
                    assert_eq!(*size, 8);
                    assert_eq!(data.len(), 8);
                    assert_eq!(data[0], 0.0);
                    // flags = 3 (save data + polygon)
                    assert!(flags & 1 != 0, "save-data flag should be set");
                }
                _ => panic!("Expected Array node in graph"),
            }
        }
        _ => panic!("Expected Graph node"),
    }
}

// ── JSON roundtrip ────────────────────────────────────────────────────────────

#[test]
fn test_json_roundtrip_sine() {
    let src = fixture("sine.pd");
    let result = parse_patch_no_loader(&src).unwrap();
    let json = to_json(&result.patch).unwrap();
    let patch2 = from_json(&json).unwrap();
    assert_eq!(result.patch, patch2);
}

#[test]
fn test_json_roundtrip_gui() {
    let src = fixture("gui.pd");
    let result = parse_patch_no_loader(&src).unwrap();
    let json = to_json(&result.patch).unwrap();
    let patch2 = from_json(&json).unwrap();
    assert_eq!(result.patch, patch2);
}

// ── PD emit roundtrip ─────────────────────────────────────────────────────────

#[test]
fn test_pd_roundtrip_sine() {
    let src = fixture("sine.pd");
    let r1 = parse_patch_no_loader(&src).unwrap();
    let emitted = emit_patch(&r1.patch);
    let r2 = parse_patch_no_loader(&emitted).unwrap();
    assert_eq!(r1.patch, r2.patch, "PD roundtrip failed for sine.pd");
}

#[test]
fn test_pd_roundtrip_subpatch() {
    let src = fixture("subpatch.pd");
    let r1 = parse_patch_no_loader(&src).unwrap();
    let emitted = emit_patch(&r1.patch);
    let r2 = parse_patch_no_loader(&emitted).unwrap();
    assert_eq!(r1.patch, r2.patch, "PD roundtrip failed for subpatch.pd");
}

#[test]
fn test_pd_roundtrip_array() {
    let src = fixture("array.pd");
    let r1 = parse_patch_no_loader(&src).unwrap();
    let emitted = emit_patch(&r1.patch);
    let r2 = parse_patch_no_loader(&emitted).unwrap();
    assert_eq!(r1.patch, r2.patch, "PD roundtrip failed for array.pd");
}

// ── Full pd2ast | ast2pd roundtrip (via JSON) ─────────────────────────────────

/// Parse .pd → JSON → Patch → .pd → Patch and assert the two Patch values match.
/// This exercises exactly what the pd2ast | ast2pd pipeline does.
fn json_roundtrip(fixture_name: &str) {
    let src = fixture(fixture_name);
    let r1 = parse_patch_no_loader(&src).unwrap();

    // pd2ast step: Patch → JSON
    let json = to_json(&r1.patch).unwrap();

    // ast2pd step: JSON → Patch → .pd text
    let patch2 = from_json(&json).unwrap();
    let pd_text = emit_patch(&patch2);

    // Re-parse the emitted .pd and compare ASTs
    let r3 = parse_patch_no_loader(&pd_text).unwrap();
    assert_eq!(
        r1.patch, r3.patch,
        "JSON roundtrip (pd→json→pd→parse) failed for {fixture_name}"
    );
}

#[test]
fn test_json_pd_roundtrip_sine() {
    json_roundtrip("sine.pd");
}

#[test]
fn test_json_pd_roundtrip_subpatch() {
    json_roundtrip("subpatch.pd");
}

#[test]
fn test_json_pd_roundtrip_array() {
    json_roundtrip("array.pd");
}

#[test]
fn test_json_pd_roundtrip_gui() {
    json_roundtrip("gui.pd");
}
