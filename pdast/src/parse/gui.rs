//! Parse IEM GUI objects from their `#X obj` argument lists.

use crate::types::{Color, GuiExtra, GuiKind, GuiObject};

/// Parse an optional "empty"-or-name field into `Option<String>`.
fn opt_str(s: &str) -> Option<String> {
    if s == "empty" || s == "-" {
        None
    } else {
        Some(s.to_string())
    }
}

fn parse_u32(s: &str) -> u32 {
    s.parse().unwrap_or(0)
}
fn parse_i32(s: &str) -> i32 {
    s.parse().unwrap_or(0)
}
fn parse_f64(s: &str) -> f64 {
    s.parse().unwrap_or(0.0)
}
fn parse_u8(s: &str) -> u8 {
    s.parse().unwrap_or(0)
}
fn parse_bool(s: &str) -> bool {
    s.parse::<u32>().unwrap_or(0) != 0
}
fn parse_color(s: &str) -> Color {
    Color::from_pd_int(s.parse::<i64>().unwrap_or(0))
}

/// Try to parse args (everything after `x y obj_name`) as an IEM GUI object.
/// Returns `None` if `name` is not a known GUI type.
pub fn try_parse_gui(name: &str, args: &[&str]) -> Option<GuiObject> {
    match name {
        "bng" => parse_bng(args),
        "tgl" => parse_tgl(args),
        "nbx" => parse_nbx(args),
        "hsl" => parse_hsl(args),
        "vsl" => parse_vsl(args),
        "hradio" => parse_hradio(args),
        "vradio" => parse_vradio(args),
        "vu" => parse_vu(args),
        "cnv" => parse_cnv(args),
        _ => None,
    }
}

/// `bng <size> <hold_ms> <interrupt_ms> <init> <send> <receive> <label>
///      <label_x_off> <label_y_off> <font> <fontsize>
///      <bg_color> <fg_color> <label_color>`
fn parse_bng(a: &[&str]) -> Option<GuiObject> {
    if a.len() < 14 {
        return None;
    }
    let size = parse_u32(a[0]);
    let hold_ms = parse_u32(a[1]);
    let interrupt_ms = parse_u32(a[2]);
    let init = parse_bool(a[3]);
    let send = opt_str(a[4]);
    let receive = opt_str(a[5]);
    let label = opt_str(a[6]);
    let label_x_off = parse_i32(a[7]);
    let label_y_off = parse_i32(a[8]);
    let font = parse_u8(a[9]);
    let font_size = parse_u32(a[10]);
    let bg_color = parse_color(a[11]);
    let fg_color = parse_color(a[12]);
    let label_color = parse_color(a[13]);
    Some(GuiObject {
        kind: GuiKind::Bang,
        width: size,
        height: size,
        min: 0.0,
        max: 1.0,
        init,
        send,
        receive,
        label,
        label_x_off,
        label_y_off,
        font,
        font_size,
        bg_color,
        fg_color,
        label_color,
        default_value: 0.0,
        extra: GuiExtra::Bang {
            hold_ms,
            interrupt_ms,
        },
    })
}

/// `tgl <size> <init> <send> <receive> <label>
///      <label_x_off> <label_y_off> <font> <fontsize>
///      <bg_color> <fg_color> <label_color> <init_value> <default_value>`
fn parse_tgl(a: &[&str]) -> Option<GuiObject> {
    if a.len() < 14 {
        return None;
    }
    let size = parse_u32(a[0]);
    let init = parse_bool(a[1]);
    let send = opt_str(a[2]);
    let receive = opt_str(a[3]);
    let label = opt_str(a[4]);
    let label_x_off = parse_i32(a[5]);
    let label_y_off = parse_i32(a[6]);
    let font = parse_u8(a[7]);
    let font_size = parse_u32(a[8]);
    let bg_color = parse_color(a[9]);
    let fg_color = parse_color(a[10]);
    let label_color = parse_color(a[11]);
    let init_value = parse_f64(a[12]);
    let default_value = parse_f64(a[13]);
    Some(GuiObject {
        kind: GuiKind::Toggle,
        width: size,
        height: size,
        min: 0.0,
        max: 1.0,
        init,
        send,
        receive,
        label,
        label_x_off,
        label_y_off,
        font,
        font_size,
        bg_color,
        fg_color,
        label_color,
        default_value,
        extra: GuiExtra::Toggle {
            init_value,
            default_value,
        },
    })
}

/// `nbx <num_digits> <height_px> <min> <max> <log> <init> <send> <receive> <label>
///      <label_x_off> <label_y_off> <font> <fontsize>
///      <bg_color> <fg_color> <label_color> <log_height>`
fn parse_nbx(a: &[&str]) -> Option<GuiObject> {
    if a.len() < 17 {
        return None;
    }
    let num_digits = parse_u32(a[0]);
    let height_px = parse_u32(a[1]);
    let min = parse_f64(a[2]);
    let max = parse_f64(a[3]);
    let log_scale = parse_bool(a[4]);
    let init = parse_bool(a[5]);
    let send = opt_str(a[6]);
    let receive = opt_str(a[7]);
    let label = opt_str(a[8]);
    let label_x_off = parse_i32(a[9]);
    let label_y_off = parse_i32(a[10]);
    let font = parse_u8(a[11]);
    let font_size = parse_u32(a[12]);
    let bg_color = parse_color(a[13]);
    let fg_color = parse_color(a[14]);
    let label_color = parse_color(a[15]);
    let log_height = parse_u32(a[16]);
    // default_value is not stored in nbx — use 0
    Some(GuiObject {
        kind: GuiKind::NumberBox,
        width: num_digits * 7 + 4, // approximate pixel width
        height: height_px,
        min,
        max,
        init,
        send,
        receive,
        label,
        label_x_off,
        label_y_off,
        font,
        font_size,
        bg_color,
        fg_color,
        label_color,
        default_value: 0.0,
        extra: GuiExtra::NumberBox {
            num_digits,
            log_scale,
            log_height,
        },
    })
}

/// `hsl <width> <height> <min> <max> <log> <init> <send> <receive> <label>
///      <label_x_off> <label_y_off> <font> <fontsize>
///      <bg_color> <fg_color> <label_color> <default_value> <steady_on_click>`
fn parse_hsl(a: &[&str]) -> Option<GuiObject> {
    if a.len() < 18 {
        return None;
    }
    let width = parse_u32(a[0]);
    let height = parse_u32(a[1]);
    let min = parse_f64(a[2]);
    let max = parse_f64(a[3]);
    let _log = parse_bool(a[4]);
    let init = parse_bool(a[5]);
    let send = opt_str(a[6]);
    let receive = opt_str(a[7]);
    let label = opt_str(a[8]);
    let label_x_off = parse_i32(a[9]);
    let label_y_off = parse_i32(a[10]);
    let font = parse_u8(a[11]);
    let font_size = parse_u32(a[12]);
    let bg_color = parse_color(a[13]);
    let fg_color = parse_color(a[14]);
    let label_color = parse_color(a[15]);
    let default_value = parse_f64(a[16]);
    let steady_on_click = parse_bool(a[17]);
    Some(GuiObject {
        kind: GuiKind::HSlider,
        width,
        height,
        min,
        max,
        init,
        send,
        receive,
        label,
        label_x_off,
        label_y_off,
        font,
        font_size,
        bg_color,
        fg_color,
        label_color,
        default_value,
        extra: GuiExtra::HSlider { steady_on_click },
    })
}

/// `vsl` — same layout as `hsl`
fn parse_vsl(a: &[&str]) -> Option<GuiObject> {
    let mut obj = parse_hsl(a)?;
    obj.kind = GuiKind::VSlider;
    obj.extra = match obj.extra {
        GuiExtra::HSlider { steady_on_click } => GuiExtra::VSlider { steady_on_click },
        _ => return None,
    };
    Some(obj)
}

/// `hradio <cell_size> <new_old> <init> <num_cells> <send> <receive> <label>
///         <label_x_off> <label_y_off> <font> <fontsize>
///         <bg_color> <fg_color> <label_color> <default_value>`
fn parse_hradio(a: &[&str]) -> Option<GuiObject> {
    if a.len() < 15 {
        return None;
    }
    let cell_size = parse_u32(a[0]);
    let _new_old = parse_u32(a[1]);
    let init = parse_bool(a[2]);
    let num_cells = parse_u32(a[3]);
    let send = opt_str(a[4]);
    let receive = opt_str(a[5]);
    let label = opt_str(a[6]);
    let label_x_off = parse_i32(a[7]);
    let label_y_off = parse_i32(a[8]);
    let font = parse_u8(a[9]);
    let font_size = parse_u32(a[10]);
    let bg_color = parse_color(a[11]);
    let fg_color = parse_color(a[12]);
    let label_color = parse_color(a[13]);
    let default_value = parse_f64(a[14]);
    Some(GuiObject {
        kind: GuiKind::HRadio,
        width: cell_size * num_cells,
        height: cell_size,
        min: 0.0,
        max: (num_cells.saturating_sub(1)) as f64,
        init,
        send,
        receive,
        label,
        label_x_off,
        label_y_off,
        font,
        font_size,
        bg_color,
        fg_color,
        label_color,
        default_value,
        extra: GuiExtra::HRadio { num_cells },
    })
}

/// `vradio` — same layout as `hradio`
fn parse_vradio(a: &[&str]) -> Option<GuiObject> {
    let mut obj = parse_hradio(a)?;
    obj.kind = GuiKind::VRadio;
    // swap width/height for vertical
    std::mem::swap(&mut obj.width, &mut obj.height);
    obj.extra = match obj.extra {
        GuiExtra::HRadio { num_cells } => GuiExtra::VRadio { num_cells },
        _ => return None,
    };
    Some(obj)
}

/// `vu <width> <height> <receive> <label>
///     <label_x_off> <label_y_off> <font> <fontsize>
///     <bg_color> <label_color> <scale> <flag>`
fn parse_vu(a: &[&str]) -> Option<GuiObject> {
    if a.len() < 12 {
        return None;
    }
    let width = parse_u32(a[0]);
    let height = parse_u32(a[1]);
    let receive = opt_str(a[2]);
    let label = opt_str(a[3]);
    let label_x_off = parse_i32(a[4]);
    let label_y_off = parse_i32(a[5]);
    let font = parse_u8(a[6]);
    let font_size = parse_u32(a[7]);
    let bg_color = parse_color(a[8]);
    let label_color = parse_color(a[9]);
    let scale = parse_bool(a[10]);
    // a[11] is an undocumented flag; ignore
    Some(GuiObject {
        kind: GuiKind::Vu,
        width,
        height,
        min: -99.0,
        max: 12.0,
        init: false,
        send: None, // VU has no send
        receive,
        label,
        label_x_off,
        label_y_off,
        font,
        font_size,
        bg_color,
        fg_color: Color::black(),
        label_color,
        default_value: 0.0,
        extra: GuiExtra::Vu { scale },
    })
}

/// `cnv <sel_size> <width> <height> <send> <receive> <label>
///      <label_x_off> <label_y_off> <font> <fontsize>
///      <bg_color> <label_color> <flag>`
fn parse_cnv(a: &[&str]) -> Option<GuiObject> {
    if a.len() < 13 {
        return None;
    }
    let sel_size = parse_u32(a[0]);
    let width = parse_u32(a[1]);
    let height = parse_u32(a[2]);
    let send = opt_str(a[3]);
    let receive = opt_str(a[4]);
    let label = opt_str(a[5]);
    let label_x_off = parse_i32(a[6]);
    let label_y_off = parse_i32(a[7]);
    let font = parse_u8(a[8]);
    let font_size = parse_u32(a[9]);
    let bg_color = parse_color(a[10]);
    let label_color = parse_color(a[11]);
    // a[12] is an undocumented flag
    Some(GuiObject {
        kind: GuiKind::Canvas,
        width,
        height,
        min: 0.0,
        max: 0.0,
        init: false,
        send,
        receive,
        label,
        label_x_off,
        label_y_off,
        font,
        font_size,
        bg_color,
        fg_color: Color::black(),
        label_color,
        default_value: 0.0,
        extra: GuiExtra::Canvas { sel_size },
    })
}

// ── Emission ─────────────────────────────────────────────────────────────────

/// Emit an optional string back to "empty" when None.
pub fn emit_opt(o: &Option<String>) -> &str {
    o.as_deref().unwrap_or("empty")
}

/// Emit a GUI object's arguments (everything after `#X obj x y name`).
pub fn emit_gui_args(g: &GuiObject) -> String {
    let send = emit_opt(&g.send);
    let recv = emit_opt(&g.receive);
    let lbl = emit_opt(&g.label);
    let bg = g.bg_color.to_pd_int();
    let fg = g.fg_color.to_pd_int();
    let lc = g.label_color.to_pd_int();

    match &g.extra {
        GuiExtra::Bang {
            hold_ms,
            interrupt_ms,
        } => format!(
            "{} {} {} {} {} {} {} {} {} {} {} {} {} {}",
            g.width,
            hold_ms,
            interrupt_ms,
            g.init as u8,
            send,
            recv,
            lbl,
            g.label_x_off,
            g.label_y_off,
            g.font,
            g.font_size,
            bg,
            fg,
            lc
        ),
        GuiExtra::Toggle {
            init_value,
            default_value,
        } => format!(
            "{} {} {} {} {} {} {} {} {} {} {} {} {} {}",
            g.width,
            g.init as u8,
            send,
            recv,
            lbl,
            g.label_x_off,
            g.label_y_off,
            g.font,
            g.font_size,
            bg,
            fg,
            lc,
            init_value,
            default_value
        ),
        GuiExtra::NumberBox {
            num_digits,
            log_scale,
            log_height,
        } => format!(
            "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {}",
            num_digits,
            g.height,
            g.min,
            g.max,
            *log_scale as u8,
            g.init as u8,
            send,
            recv,
            lbl,
            g.label_x_off,
            g.label_y_off,
            g.font,
            g.font_size,
            bg,
            fg,
            lc,
            log_height
        ),
        GuiExtra::HSlider { steady_on_click } | GuiExtra::VSlider { steady_on_click } => format!(
            "{} {} {} {} 0 {} {} {} {} {} {} {} {} {} {} {} {} {}",
            g.width,
            g.height,
            g.min,
            g.max,
            g.init as u8,
            send,
            recv,
            lbl,
            g.label_x_off,
            g.label_y_off,
            g.font,
            g.font_size,
            bg,
            fg,
            lc,
            g.default_value,
            *steady_on_click as u8
        ),
        GuiExtra::HRadio { num_cells } | GuiExtra::VRadio { num_cells } => {
            let cell = match &g.extra {
                GuiExtra::HRadio { .. } => {
                    if *num_cells > 0 {
                        g.width / num_cells
                    } else {
                        15
                    }
                }
                GuiExtra::VRadio { .. } => {
                    if *num_cells > 0 {
                        g.height / num_cells
                    } else {
                        15
                    }
                }
                _ => 15,
            };
            format!(
                "{} 1 {} {} {} {} {} {} {} {} {} {} {} {} {}",
                cell,
                g.init as u8,
                num_cells,
                send,
                recv,
                lbl,
                g.label_x_off,
                g.label_y_off,
                g.font,
                g.font_size,
                bg,
                fg,
                lc,
                g.default_value
            )
        }
        GuiExtra::Vu { scale } => format!(
            "{} {} {} {} {} {} {} {} {} {} {} 0",
            g.width,
            g.height,
            recv,
            lbl,
            g.label_x_off,
            g.label_y_off,
            g.font,
            g.font_size,
            bg,
            lc,
            *scale as u8
        ),
        GuiExtra::Canvas { sel_size } => format!(
            "{} {} {} {} {} {} {} {} {} {} {} {} 0",
            sel_size,
            g.width,
            g.height,
            send,
            recv,
            lbl,
            g.label_x_off,
            g.label_y_off,
            g.font,
            g.font_size,
            bg,
            lc
        ),
    }
}
