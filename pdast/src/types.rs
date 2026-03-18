//! AST types for PureData patches.
//!
//! These types form the JSON-serializable intermediate representation of a `.pd` file.

use serde::{Deserialize, Serialize};

// ── Atoms / tokens ──────────────────────────────────────────────────────────

/// A single atom in a PureData message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Token {
    /// A floating-point number literal.
    Float(f64),
    /// A symbol (unquoted word, possibly with escape sequences resolved).
    Symbol(String),
    /// A positional argument reference: `$1`, `$2`, etc.
    Dollar(u32),
    /// The patch-instance ID variable `$0`.
    DollarZero,
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Token::Float(v) => {
                // Emit without trailing ".0" when the value is a whole number,
                // but always use a dot when the value has a fractional part.
                if v.fract() == 0.0 && v.is_finite() {
                    write!(f, "{}", *v as i64)
                } else {
                    write!(f, "{v}")
                }
            }
            Token::Symbol(s) => write!(f, "{s}"),
            Token::Dollar(n) => write!(f, "${n}"),
            Token::DollarZero => write!(f, "$0"),
        }
    }
}

// ── Colors ───────────────────────────────────────────────────────────────────

/// RGB color stored as individual channels.
///
/// PD encodes colors as a negative integer: `color = -(R*65536 + G*256 + B)`.
/// We decode on parse and re-encode on emit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub fn black() -> Self {
        Color { r: 0, g: 0, b: 0 }
    }
    pub fn white() -> Self {
        Color {
            r: 255,
            g: 255,
            b: 255,
        }
    }

    /// Decode from the PD legacy signed-integer color format.
    pub fn from_pd_int(n: i64) -> Self {
        // Legacy: color = -(R*65536 + G*256 + B)
        // Newer PD (≥0.52) uses plain positive decimal, but it's formatted
        // the same way in the file — we handle both here.
        let raw: u32 = if n < 0 { (-n) as u32 } else { n as u32 };
        Color {
            r: ((raw >> 16) & 0xFF) as u8,
            g: ((raw >> 8) & 0xFF) as u8,
            b: (raw & 0xFF) as u8,
        }
    }

    /// Encode to the PD legacy signed-integer color format.
    pub fn to_pd_int(&self) -> i64 {
        let raw = (self.r as i64) * 65536 + (self.g as i64) * 256 + self.b as i64;
        -raw
    }
}

// ── Graph coordinate system ──────────────────────────────────────────────────

/// The `#X coords` record — defines data-to-pixel mapping for a graph sub-canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Coords {
    pub x_from: f64,
    pub y_top: f64,
    pub x_to: f64,
    pub y_bottom: f64,
    pub width_px: u32,
    pub height_px: u32,
    /// If true, the sub-patch renders its contents in the parent (GOP mode).
    pub gop: bool,
    pub x_margin: Option<i32>,
    pub y_margin: Option<i32>,
}

// ── GUI objects ───────────────────────────────────────────────────────────────

/// Which kind of IEM GUI object this is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuiKind {
    Bang,
    Toggle,
    NumberBox,
    HSlider,
    VSlider,
    HRadio,
    VRadio,
    Vu,
    Canvas,
}

/// Extra fields that differ per GUI kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "extra_kind", rename_all = "snake_case")]
pub enum GuiExtra {
    Bang {
        hold_ms: u32,
        interrupt_ms: u32,
    },
    Toggle {
        /// Initial value when `init` is true.
        init_value: f64,
        default_value: f64,
    },
    NumberBox {
        num_digits: u32,
        log_scale: bool,
        log_height: u32,
    },
    HSlider {
        steady_on_click: bool,
    },
    VSlider {
        steady_on_click: bool,
    },
    HRadio {
        num_cells: u32,
    },
    VRadio {
        num_cells: u32,
    },
    Vu {
        scale: bool,
    },
    Canvas {
        sel_size: u32,
    },
}

/// An IEM GUI object (bng, tgl, nbx, hsl, vsl, hradio, vradio, vu, cnv).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuiObject {
    #[serde(rename = "gui_kind")]
    pub kind: GuiKind,
    pub width: u32,
    pub height: u32,
    pub min: f64,
    pub max: f64,
    pub init: bool,
    pub send: Option<String>,
    pub receive: Option<String>,
    pub label: Option<String>,
    pub label_x_off: i32,
    pub label_y_off: i32,
    /// 0=Courier, 1=Helvetica, 2=Times
    pub font: u8,
    pub font_size: u32,
    pub bg_color: Color,
    pub fg_color: Color,
    pub label_color: Color,
    pub default_value: f64,
    pub extra: GuiExtra,
}

// ── Node kinds ────────────────────────────────────────────────────────────────

/// The semantic content of a node in a canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKind {
    /// A vanilla or external object box: `#X obj x y name args...`
    Obj { name: String, args: Vec<Token> },
    /// A message box: `#X msg x y content`
    ///
    /// Content is split on `;` into separate messages, each a list of atoms
    /// separated by `,` (which produces a list) or spaces.
    Msg {
        /// Outer vec: semicolon-separated messages. Inner vec: comma/space atoms.
        messages: Vec<Vec<Token>>,
    },
    /// A vanilla number box: `#X floatatom`
    FloatAtom {
        width: u32,
        min: f64,
        max: f64,
        label_pos: u8,
        label: Option<String>,
        receive: Option<String>,
        send: Option<String>,
    },
    /// A vanilla symbol box: `#X symbolatom`
    SymbolAtom {
        width: u32,
        label_pos: u8,
        label: Option<String>,
        receive: Option<String>,
        send: Option<String>,
    },
    /// A comment: `#X text`
    Text { content: String },
    /// An inline sub-patch (`pd name`) or unresolved abstraction reference.
    SubPatch {
        name: String,
        args: Vec<Token>,
        content: SubPatchContent,
    },
    /// A graph sub-canvas (`#X restore x y graph`).
    Graph { content: Box<Canvas> },
    /// An IEM GUI object.
    Gui(GuiObject),
    /// An array declared inside a graph sub-canvas.
    Array {
        name: String,
        size: u32,
        /// Currently always `"float"`.
        data_type: String,
        /// Bitmask: bit0=save data, bits1-2=plot style (0=line,1=points,2=bezier), bit3=hide name.
        flags: u32,
        /// Saved sample data (only present when bit0 of flags is set).
        data: Vec<f64>,
    },
    /// An object whose type could not be resolved (external, broken box, etc.).
    Unknown {
        name: Option<String>,
        args: Vec<Token>,
    },
}

/// Either the full inline canvas content, or a marker that the abstraction
/// could not be resolved (loader returned None).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SubPatchContent {
    /// Full canvas tree embedded here (inline `pd` sub-patch, or resolved abstraction).
    Inline(Box<Canvas>),
    /// The loader did not return content for this name.
    Unresolved,
}

// ── Connections ───────────────────────────────────────────────────────────────

/// A patch cord connecting two objects within the same canvas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub src_node: u32,
    pub src_outlet: u32,
    pub dst_node: u32,
    pub dst_inlet: u32,
}

// ── Node ─────────────────────────────────────────────────────────────────────

/// A single node (object box) within a canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Sequential index within the canvas (0-based), matching PD's own numbering.
    pub id: u32,
    pub x: i32,
    pub y: i32,
    pub kind: NodeKind,
}

// ── Canvas ────────────────────────────────────────────────────────────────────

/// A canvas — either the root patch window or a sub-patch context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Canvas {
    /// Window / canvas position on screen.
    pub x: i32,
    pub y: i32,
    /// Canvas width in pixels.
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
    /// Font size — `Some` only on the root canvas.
    pub font_size: Option<u32>,
    /// Sub-patch name — `Some` only on named sub-patch canvases.
    pub name: Option<String>,
    /// Whether this sub-patch window opens automatically when the parent is loaded.
    pub open_on_load: bool,
    /// Graph coordinate mapping (`#X coords`), if present.
    pub coords: Option<Coords>,
    /// All nodes, in file order. Their `id` fields match PD's implicit numbering.
    pub nodes: Vec<Node>,
    /// All patch cords within this canvas.
    pub connections: Vec<Connection>,
}

// ── Top-level Patch ───────────────────────────────────────────────────────────

/// The top-level patch, containing the root canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    pub root: Canvas,
}

// ── Parse result ──────────────────────────────────────────────────────────────

/// A non-fatal issue noticed during parsing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Warning {
    /// Object index within its canvas, if the warning is about a specific node.
    pub node_id: Option<u32>,
    pub message: String,
}

/// The result of parsing a `.pd` file: an AST plus any non-fatal warnings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParseResult {
    pub patch: Patch,
    pub warnings: Vec<Warning>,
}
