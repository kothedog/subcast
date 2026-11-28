//! Headless PNG rendering for clip overlays, subtitle cue bands, video covers, and quote posters.
use crate::highlights::{ClipFit, ClipGeometry};
use crate::subexport::Cue;
use crate::videoclip::{subject_x, CueOverlay};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use cosmic_text::{
    Align, Attrs, Buffer, Color as TextColor, Family, FontSystem, Metrics, Shaping, SwashCache,
    Weight, Wrap,
};
use image::{imageops::FilterType, DynamicImage, GenericImageView, RgbaImage};
use once_cell::sync::Lazy;
use qrcode::QrCode;
use std::path::Path;
use parking_lot::Mutex;
use tiny_skia::{
    FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point,
    PremultipliedColorU8, Rect, SpreadMode, Transform,
};

const LOGO_PNG: &[u8] = include_bytes!("../../../src/assets/logo.png");

static FONT_SYS: Lazy<Mutex<FontState>> = Lazy::new(|| {
    Mutex::new(FontState {
        fonts: FontSystem::new(),
        swash: SwashCache::new(),
    })
});

struct FontState {
    fonts: FontSystem,
    swash: SwashCache,
}

#[derive(Clone, Copy, Debug)]
struct Rgba(u8, u8, u8, u8);

impl Rgba {
    fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.0, self.1, self.2, self.3)
    }

    fn to_text(self) -> TextColor {
        TextColor::rgba(self.0, self.1, self.2, self.3)
    }
}

fn parse_hex(s: &str, fallback: Rgba) -> Rgba {
    let hex = s.trim().trim_start_matches('#');
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok();
            let g = u8::from_str_radix(&hex[2..4], 16).ok();
            let b = u8::from_str_radix(&hex[4..6], 16).ok();
            match (r, g, b) {
                (Some(r), Some(g), Some(b)) => Rgba(r, g, b, 255),
                _ => fallback,
            }
        }
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok();
            let g = u8::from_str_radix(&hex[2..4], 16).ok();
            let b = u8::from_str_radix(&hex[4..6], 16).ok();
            let a = u8::from_str_radix(&hex[6..8], 16).ok();
            match (r, g, b, a) {
                (Some(r), Some(g), Some(b), Some(a)) => Rgba(r, g, b, a),
                _ => fallback,
            }
        }
        _ => fallback,
    }
}

fn is_light_color(c: Rgba) -> bool {
    let luma = 0.299 * c.0 as f32 + 0.587 * c.1 as f32 + 0.114 * c.2 as f32;
    luma > 140.0
}

fn ink_on_bg(bg: Rgba) -> Rgba {
    if is_light_color(bg) {
        Rgba(20, 20, 22, 255)
    } else {
        Rgba(255, 255, 255, 255)
    }
}

fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let r = r.min(w * 0.5).min(h * 0.5).max(0.0);
    if r <= 0.5 {
        let mut pb = PathBuilder::new();
        pb.push_rect(Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    let k = 0.55228475 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn fill_rect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
    if let Some(rect) = Rect::from_xywh(x, y, w, h) {
        let mut paint = Paint::default();
        paint.set_color(color.to_skia());
        paint.anti_alias = true;
        pm.fill_rect(rect, &paint, Transform::identity(), None);
    }
}

fn fill_round_rect(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, color: Rgba) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        let mut paint = Paint::default();
        paint.set_color(color.to_skia());
        paint.anti_alias = true;
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn stroke_round_rect(
    pm: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    stroke_w: f32,
    color: Rgba,
) {
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        let mut paint = Paint::default();
        paint.set_color(color.to_skia());
        paint.anti_alias = true;
        let stroke = tiny_skia::Stroke {
            width: stroke_w,
            ..Default::default()
        };
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

fn fill_v_gradient(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, top: Rgba, bottom: Rgba) {
    if let Some(rect) = Rect::from_xywh(x, y, w, h) {
        let stops = vec![
            GradientStop::new(0.0, top.to_skia()),
            GradientStop::new(1.0, bottom.to_skia()),
        ];
        if let Some(shader) = LinearGradient::new(
            Point::from_xy(x, y),
            Point::from_xy(x, y + h),
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        ) {
            let mut paint = Paint::default();
            paint.shader = shader;
            pm.fill_rect(rect, &paint, Transform::identity(), None);
        }
    }
}

fn blend_rgba_over(dst: PremultipliedColorU8, sr: u8, sg: u8, sb: u8, sa: u8) -> PremultipliedColorU8 {
    if sa == 0 {
        return dst;
    }
    if sa == 255 {
        return PremultipliedColorU8::from_rgba(sr, sg, sb, 255).unwrap_or(dst);
    }
    let af = sa as f32 / 255.0;
    let inv = 1.0 - af;
    let nr_pm = (sr as f32 * af + dst.red() as f32 * inv).round().clamp(0.0, 255.0) as u8;
    let ng_pm = (sg as f32 * af + dst.green() as f32 * inv).round().clamp(0.0, 255.0) as u8;
    let nb_pm = (sb as f32 * af + dst.blue() as f32 * inv).round().clamp(0.0, 255.0) as u8;
    let na = (sa as f32 + dst.alpha() as f32 * inv).round().clamp(0.0, 255.0) as u8;
    PremultipliedColorU8::from_rgba(nr_pm.min(na), ng_pm.min(na), nb_pm.min(na), na).unwrap_or(dst)
}

fn blit_rgba_image(pm: &mut Pixmap, img: &RgbaImage, dx: i32, dy: i32, circle_clip: bool) {
    let pw = pm.width() as i32;
    let ph = pm.height() as i32;
    let iw = img.width() as i32;
    let ih = img.height() as i32;
    if iw <= 0 || ih <= 0 {
        return;
    }
    let cx = iw as f32 * 0.5;
    let cy = ih as f32 * 0.5;
    let rad = (iw.min(ih) as f32) * 0.5;
    let pixels = pm.pixels_mut();
    for sy in 0..ih {
        let ty = dy + sy;
        if ty < 0 || ty >= ph {
            continue;
        }
        for sx in 0..iw {
            let tx = dx + sx;
            if tx < 0 || tx >= pw {
                continue;
            }
            if circle_clip {
                let ddx = sx as f32 + 0.5 - cx;
                let ddy = sy as f32 + 0.5 - cy;
                if ddx * ddx + ddy * ddy > rad * rad {
                    continue;
                }
            }
            let p = img.get_pixel(sx as u32, sy as u32).0;
            let idx = (ty as usize) * (pw as usize) + (tx as usize);
            pixels[idx] = blend_rgba_over(pixels[idx], p[0], p[1], p[2], p[3]);
        }
    }
}

fn draw_cover_image_cropped(
    pm: &mut Pixmap,
    src: &DynamicImage,
    dx: i32,
    dy: i32,
    dw: u32,
    dh: u32,
    focus_y: f64,
    crop_bottom: f64,
    subj_x: f64,
    circle_clip: bool,
) {
    if dw == 0 || dh == 0 {
        return;
    }
    let (sw, raw_sh) = src.dimensions();
    if sw == 0 || raw_sh == 0 {
        return;
    }
    let sh = ((raw_sh as f64) * (1.0 - crop_bottom.clamp(0.0, 0.45))).round().max(1.0) as u32;
    let target_ratio = dw as f64 / dh as f64;
    let src_ratio = sw as f64 / sh as f64;
    let (cw, ch, cx, cy) = if src_ratio > target_ratio {
        let cw = ((sh as f64) * target_ratio).round().max(1.0) as u32;
        let max_x = sw.saturating_sub(cw);
        let ideal_x = ((sw as f64) * subj_x - (cw as f64) * 0.5).round().max(0.0) as u32;
        (cw.min(sw), sh, ideal_x.min(max_x), 0)
    } else {
        let ch = ((sw as f64) / target_ratio).round().max(1.0) as u32;
        let max_y = sh.saturating_sub(ch);
        let ideal_y = ((sh as f64 - ch as f64) * focus_y).round().max(0.0) as u32;
        (sw, ch.min(sh), 0, ideal_y.min(max_y))
    };
    let cropped = src.crop_imm(cx, cy, cw, ch);
    let resized = cropped.resize_exact(dw, dh, FilterType::Triangle).to_rgba8();
    blit_rgba_image(pm, &resized, dx, dy, circle_clip);
}

#[derive(Clone, Debug)]
struct RichSpan {
    text: String,
    highlight: bool,
}

fn parse_rich_spans(raw: &str) -> Vec<RichSpan> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut hi = false;
    for ch in raw.chars() {
        match ch {
            '【' => {
                if !cur.is_empty() {
                    out.push(RichSpan {
                        text: std::mem::take(&mut cur),
                        highlight: hi,
                    });
                }
                hi = true;
            }
            '】' => {
                if !cur.is_empty() {
                    out.push(RichSpan {
                        text: std::mem::take(&mut cur),
                        highlight: hi,
                    });
                }
                hi = false;
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(RichSpan { text: cur, highlight: hi });
    }
    out
}

#[derive(Clone, Copy)]
struct TextDrawStyle {
    size: f32,
    line_height: f32,
    max_width: f32,
    color: Rgba,
    highlight_color: Rgba,
    weight: Weight,
    serif: bool,
    align: Align,
    outline: Option<(Rgba, i32)>,
    marker: Option<(Rgba, Rgba)>,
}

impl Default for TextDrawStyle {
    fn default() -> Self {
        Self {
            size: 32.0,
            line_height: 44.0,
            max_width: 900.0,
            color: Rgba(255, 255, 255, 255),
            highlight_color: Rgba(255, 210, 63, 255),
            weight: Weight::NORMAL,
            serif: false,
            align: Align::Left,
            outline: None,
            marker: None,
        }
    }
}

/// Layouts text (with optional `【highlight】` spans), returns the rendered height, and draws onto `pm`.
fn draw_rich_text_block(
    pm: &mut Pixmap,
    text: &str,
    x: f32,
    y: f32,
    style: TextDrawStyle,
    max_lines: Option<usize>,
) -> f32 {
    if text.trim().is_empty() {
        return 0.0;
    }
    let spans = parse_rich_spans(text);
    let mut guard = FONT_SYS.lock();
    let FontState { fonts, swash } = &mut *guard;

    let family = if style.serif { Family::Serif } else { Family::SansSerif };
    let default_attrs = Attrs::new()
        .family(family)
        .weight(style.weight)
        .color(style.color.to_text());

    let mut buffer = Buffer::new(fonts, Metrics::new(style.size, style.line_height));
    buffer.set_size(fonts, Some(style.max_width.max(10.0)), None);
    buffer.set_wrap(fonts, Wrap::WordOrGlyph);

    let rich: Vec<(&str, Attrs<'_>)> = spans
        .iter()
        .map(|s| {
            let col = if s.highlight {
                if let Some((_, marker_ink)) = style.marker {
                    marker_ink
                } else {
                    style.highlight_color
                }
            } else {
                style.color
            };
            (
                s.text.as_str(),
                Attrs::new()
                    .family(family)
                    .weight(style.weight)
                    .color(col.to_text()),
            )
        })
        .collect();
    buffer.set_rich_text(fonts, rich, default_attrs, Shaping::Advanced);
    for line in buffer.lines.iter_mut() {
        line.set_align(Some(style.align));
    }
    buffer.shape_until_scroll(fonts, false);

    let max_run_y = max_lines
        .map(|n| (n as f32) * style.line_height)
        .unwrap_or(f32::MAX);

    // Optional marker pill behind highlighted spans (before text glyphs)
    if let Some((marker_bg, _)) = style.marker {
        // Compute byte ranges of highlighted spans per flattened string
        let mut offset = 0usize;
        let mut hi_ranges: Vec<(usize, usize)> = Vec::new();
        for s in &spans {
            let end = offset + s.text.len();
            if s.highlight {
                hi_ranges.push((offset, end));
            }
            offset = end;
        }
        let mut cur_line_offset = 0usize;
        let mut prev_line_i = None;
        for run in buffer.layout_runs() {
            if run.line_top + 1.0 >= max_run_y {
                break;
            }
            if prev_line_i != Some(run.line_i) {
                // find start byte offset in flat string
                cur_line_offset = buffer
                    .lines
                    .iter()
                    .take(run.line_i)
                    .map(|l| l.text().len() + 1)
                    .sum();
                prev_line_i = Some(run.line_i);
            }
            let mut gx0: Option<f32> = None;
            let mut gx1: f32 = 0.0;
            for g in run.glyphs.iter() {
                let gs = cur_line_offset + g.start;
                let ge = cur_line_offset + g.end;
                let is_hi = hi_ranges.iter().any(|&(a, b)| gs < b && ge > a);
                if is_hi {
                    if gx0.is_none() {
                        gx0 = Some(g.x);
                    }
                    gx1 = g.x + g.w;
                } else if let Some(x0) = gx0.take() {
                    let pad = style.size * 0.14;
                    fill_round_rect(
                        pm,
                        x + x0 - pad,
                        y + run.line_top + style.size * 0.06,
                        (gx1 - x0) + pad * 2.0,
                        style.size * 1.15,
                        style.size * 0.14,
                        marker_bg,
                    );
                }
            }
            if let Some(x0) = gx0 {
                let pad = style.size * 0.14;
                fill_round_rect(
                    pm,
                    x + x0 - pad,
                    y + run.line_top + style.size * 0.06,
                    (gx1 - x0) + pad * 2.0,
                    style.size * 1.15,
                    style.size * 0.14,
                    marker_bg,
                );
            }
        }
    }

    let pw = pm.width() as i32;
    let ph = pm.height() as i32;
    let xi = x.round() as i32;
    let yi = y.round() as i32;

    if let Some((out_col, rad)) = style.outline {
        let mut offsets = Vec::new();
        let r = rad.clamp(1, 8);
        for dy in -r..=r {
            for dx in -r..=r {
                if dx == 0 && dy == 0 {
                    continue;
                }
                if dx * dx + dy * dy <= r * r + 1 {
                    offsets.push((dx, dy));
                }
            }
        }
        let mut mask_pts: Vec<(i32, i32, u8)> = Vec::new();
        buffer.draw(fonts, swash, out_col.to_text(), |gx, gy, gw, gh, c| {
            if (gy as f32) >= max_run_y || c.a() == 0 {
                return;
            }
            for sy in 0..gh as i32 {
                for sx in 0..gw as i32 {
                    mask_pts.push((gx + sx, gy + sy, c.a()));
                }
            }
        });
        let pixels = pm.pixels_mut();
        for &(ox, oy) in &offsets {
            for &(mx, my, alpha) in &mask_pts {
                let px = xi + mx + ox;
                let py = yi + my + oy;
                if px >= 0 && px < pw && py >= 0 && py < ph {
                    let idx = (py as usize) * (pw as usize) + (px as usize);
                    pixels[idx] = blend_rgba_over(pixels[idx], out_col.0, out_col.1, out_col.2, alpha);
                }
            }
        }
    }

    let mut total_h = 0.0f32;
    for run in buffer.layout_runs() {
        if run.line_top + 1.0 >= max_run_y {
            break;
        }
        total_h = total_h.max(run.line_top + style.line_height);
    }

    let pixels = pm.pixels_mut();
    buffer.draw(fonts, swash, style.color.to_text(), |gx, gy, gw, gh, c| {
        if (gy as f32) >= max_run_y || c.a() == 0 {
            return;
        }
        for sy in 0..gh as i32 {
            let py = yi + gy + sy;
            if py < 0 || py >= ph {
                continue;
            }
            for sx in 0..gw as i32 {
                let px = xi + gx + sx;
                if px < 0 || px >= pw {
                    continue;
                }
                let idx = (py as usize) * (pw as usize) + (px as usize);
                pixels[idx] = blend_rgba_over(pixels[idx], c.r(), c.g(), c.b(), c.a());
            }
        }
    });

    total_h
}

fn measure_rich_text_lines(
    text: &str,
    size: f32,
    line_height: f32,
    max_width: f32,
    weight: Weight,
    serif: bool,
) -> (usize, f32, f32) {
    if text.trim().is_empty() {
        return (0, 0.0, 0.0);
    }
    let clean = text.replace(['【', '】'], "");
    let mut guard = FONT_SYS.lock();
    let fonts = &mut guard.fonts;
    let family = if serif { Family::Serif } else { Family::SansSerif };
    let mut buffer = Buffer::new(fonts, Metrics::new(size, line_height));
    buffer.set_size(fonts, Some(max_width.max(10.0)), None);
    buffer.set_wrap(fonts, Wrap::WordOrGlyph);
    buffer.set_text(
        fonts,
        &clean,
        Attrs::new().family(family).weight(weight),
        Shaping::Advanced,
    );
    buffer.shape_until_scroll(fonts, false);
    let mut count = 0usize;
    let mut max_w = 0.0f32;
    for run in buffer.layout_runs() {
        count += 1;
        max_w = max_w.max(run.line_w);
    }
    (count, max_w, count as f32 * line_height)
}

fn fit_title_size(
    text: &str,
    max_width: f32,
    max_lines: usize,
    max_size: f32,
    min_size: f32,
    line_mult: f32,
    serif: bool,
) -> f32 {
    let mut s = max_size;
    while s >= min_size {
        let (lines, _, _) =
            measure_rich_text_lines(text, s, s * line_mult, max_width, Weight::BOLD, serif);
        if lines <= max_lines {
            return s;
        }
        s -= 4.0;
    }
    min_size
}

fn draw_pill(
    pm: &mut Pixmap,
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    bg: Rgba,
    ink: Rgba,
    align_right: bool,
) -> f32 {
    let (_, tw, _) = measure_rich_text_lines(text, size, size * 1.2, 1200.0, Weight::SEMIBOLD, false);
    let w = tw + size * 1.2;
    let h = size * 1.85;
    let px = if align_right { x - w } else { x };
    fill_round_rect(pm, px, y, w, h, h * 0.5, bg);
    draw_rich_text_block(
        pm,
        text,
        px + size * 0.6,
        y + h * 0.19,
        TextDrawStyle {
            size,
            line_height: size * 1.25,
            max_width: tw + 20.0,
            color: ink,
            weight: Weight::SEMIBOLD,
            ..Default::default()
        },
        Some(1),
    );
    w
}

#[derive(Clone, Debug)]
pub struct StaticOverlayOpts {
    pub geom: ClipGeometry,
    pub show_name: String,
    pub title: String,
    pub card_bg: String,
    pub accent: String,
    pub show_brand: bool,
    pub brand_text: String,
}

pub fn render_static_overlay_png(opts: &StaticOverlayOpts) -> Result<Vec<u8>> {
    let w = opts.geom.w.max(64);
    let h = opts.geom.h.max(64);
    let mut pm = Pixmap::new(w, h).ok_or_else(|| anyhow!("failed to allocate overlay pixmap"))?;
    let u = w.min(h) as f32 / 1080.0;
    let is_light = matches!(opts.card_bg.as_str(), "paper" | "white" | "warm");
    let primary_ink = if is_light {
        Rgba(24, 24, 27, 255)
    } else {
        Rgba(255, 255, 255, 242)
    };
    let muted_ink = if is_light {
        Rgba(87, 83, 78, 220)
    } else {
        Rgba(255, 255, 255, 168)
    };
    let accent = parse_hex(&opts.accent, Rgba(255, 210, 63, 255));
    let pad_x = (64.0 * u).round();

    if opts.geom.fit == ClipFit::Card {
        let vx = opts.geom.video.x as f32;
        let vy = opts.geom.video.y as f32;
        let vw = opts.geom.video.w as f32;
        let vh = opts.geom.video.h as f32;
        let border_col = if is_light {
            Rgba(24, 24, 27, 36)
        } else {
            Rgba(255, 255, 255, 32)
        };
        stroke_round_rect(&mut pm, vx, vy, vw, vh, 0.0, (2.0 * u).max(1.5), border_col);

        let avail_top = vy - (32.0 * u);
        if avail_top > 120.0 * u {
            let max_w = w as f32 - pad_x * 2.0;
            let show = opts.show_name.trim();
            let title = opts.title.trim();
            let title_size =
                fit_title_size(title, max_w, 2, 54.0 * u, 32.0 * u, 1.28, false);
            let (_, _, title_h) = measure_rich_text_lines(
                title,
                title_size,
                title_size * 1.28,
                max_w,
                Weight::BOLD,
                false,
            );
            let kicker_h = if show.is_empty() { 0.0 } else { 44.0 * u };
            let block_h = kicker_h + title_h;
            let mut ty = ((vy - block_h) * 0.56).max(40.0 * u);
            if !show.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    show,
                    pad_x,
                    ty,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: max_w,
                        color: accent,
                        weight: Weight::SEMIBOLD,
                        ..Default::default()
                    },
                    Some(1),
                );
                ty += kicker_h;
            }
            if !title.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    title,
                    pad_x,
                    ty,
                    TextDrawStyle {
                        size: title_size,
                        line_height: title_size * 1.28,
                        max_width: max_w,
                        color: primary_ink,
                        highlight_color: accent,
                        weight: Weight::BOLD,
                        ..Default::default()
                    },
                    Some(2),
                );
            }
        }
    }

    if opts.show_brand {
        let brand = if opts.brand_text.trim().is_empty() {
            "SubCast"
        } else {
            opts.brand_text.trim()
        };
        let badge_sz = (28.0 * u).round().max(18.0) as u32;
        if let Ok(logo_img) = image::load_from_memory(LOGO_PNG) {
            let resized = logo_img.resize_exact(badge_sz, badge_sz, FilterType::Triangle).to_rgba8();
            let ly = h as i32 - (66.0 * u).round() as i32;
            blit_rgba_image(&mut pm, &resized, pad_x.round() as i32, ly, false);
        }
        draw_rich_text_block(
            &mut pm,
            brand,
            pad_x + badge_sz as f32 + 12.0 * u,
            h as f32 - 66.0 * u,
            TextDrawStyle {
                size: 24.0 * u,
                line_height: 30.0 * u,
                max_width: w as f32 - pad_x * 2.0,
                color: muted_ink,
                weight: Weight::SEMIBOLD,
                ..Default::default()
            },
            Some(1),
        );
    }

    Ok(pm.encode_png()?)
}

pub fn render_cue_overlays(
    geom: &ClipGeometry,
    cues: &[Cue],
    subs_below: bool,
    card_bg: &str,
    accent: &str,
) -> Result<Vec<CueOverlay>> {
    let w = geom.w.max(64);
    let h = geom.h.max(64);
    let u = w.min(h) as f32 / 1080.0;
    let pad_x = (56.0 * u).round();
    let max_w = (w as f32 - pad_x * 2.0).max(100.0);
    let band_h = ((260.0 * u).round() as u32).max(120).min(h);
    let below_video = geom.fit == ClipFit::Card && subs_below;
    let is_light_card = below_video && matches!(card_bg, "paper" | "white" | "warm");

    let target_y: i32 = if below_video {
        let space_below = h.saturating_sub(geom.video.y + geom.video.h);
        if space_below >= band_h + 24 {
            (geom.video.y + geom.video.h + ((space_below - band_h) / 3).min(48)) as i32
        } else {
            h.saturating_sub(band_h + 24) as i32
        }
    } else if geom.fit == ClipFit::Card {
        (geom.video.y + geom.video.h).saturating_sub(band_h + 12) as i32
    } else {
        h.saturating_sub(band_h + (120.0 * u).round() as u32) as i32
    };

    let primary_sz = if geom.vertical { 44.0 * u } else { 38.0 * u };
    let secondary_sz = if geom.vertical { 34.0 * u } else { 30.0 * u };
    let primary_lh = primary_sz * 1.32;
    let secondary_lh = secondary_sz * 1.32;
    let _accent = parse_hex(accent, Rgba(255, 210, 63, 255));

    let mut out = Vec::with_capacity(cues.len());
    for cue in cues {
        let orig = cue.orig.trim();
        let zh = cue.zh.as_deref().map(str::trim).unwrap_or("");
        if orig.is_empty() && zh.is_empty() {
            continue;
        }
        let mut pm =
            Pixmap::new(w, band_h).ok_or_else(|| anyhow!("failed to allocate cue pixmap"))?;

        let (lead, sub) = if !orig.is_empty() {
            (orig, zh)
        } else {
            (zh, "")
        };
        let (lead_lines, lead_w, lead_h) = measure_rich_text_lines(
            lead,
            primary_sz,
            primary_lh,
            max_w - 40.0 * u,
            Weight::BOLD,
            false,
        );
        let (sub_lines, sub_w, sub_h) = if !sub.is_empty() {
            measure_rich_text_lines(
                sub,
                secondary_sz,
                secondary_lh,
                max_w - 40.0 * u,
                Weight::MEDIUM,
                false,
            )
        } else {
            (0, 0.0, 0.0)
        };
        let gap = if lead_lines > 0 && sub_lines > 0 {
            10.0 * u
        } else {
            0.0
        };
        let content_h = lead_h + gap + sub_h;
        let top_y = ((band_h as f32 - content_h) * 0.5).max(8.0 * u);

        if !below_video {
            let box_w = (lead_w.max(sub_w) + 56.0 * u).min(max_w);
            let box_h = (content_h + 28.0 * u).min(band_h as f32 - 8.0);
            fill_round_rect(
                &mut pm,
                (w as f32 - box_w) * 0.5,
                (top_y - 14.0 * u).max(4.0),
                box_w,
                box_h,
                16.0 * u,
                Rgba(12, 12, 16, 188),
            );
        }

        let ink_main = if is_light_card {
            Rgba(24, 24, 27, 255)
        } else {
            Rgba(255, 255, 255, 255)
        };
        let ink_sub = if is_light_card {
            Rgba(68, 64, 60, 235)
        } else {
            Rgba(240, 240, 244, 225)
        };
        let outline = if below_video {
            None
        } else {
            Some((Rgba(0, 0, 0, 210), 2))
        };

        let mut cy = top_y;
        draw_rich_text_block(
            &mut pm,
            lead,
            pad_x,
            cy,
            TextDrawStyle {
                size: primary_sz,
                line_height: primary_lh,
                max_width: max_w,
                color: ink_main,
                weight: Weight::BOLD,
                align: Align::Center,
                outline,
                ..Default::default()
            },
            Some(2),
        );
        cy += lead_h + gap;
        if !sub.is_empty() {
            draw_rich_text_block(
                &mut pm,
                sub,
                pad_x,
                cy,
                TextDrawStyle {
                    size: secondary_sz,
                    line_height: secondary_lh,
                    max_width: max_w,
                    color: ink_sub,
                    weight: Weight::MEDIUM,
                    align: Align::Center,
                    outline,
                    ..Default::default()
                },
                Some(2),
            );
        }

        let png = pm.encode_png()?;
        out.push(CueOverlay {
            start: cue.start,
            end: cue.end,
            y: target_y,
            png_base64: B64.encode(png),
        });
    }
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct CoverOpts {
    pub template: String,
    pub width: u32,
    pub height: u32,
    pub hook_title: String,
    pub sub_title: String,
    pub kicker: String,
    pub show_name: String,
    pub brand: String,
    pub duration_sec: f64,
    pub accent: String,
    pub font: String,
    pub backdrop_path: Option<String>,
    pub crop_bottom: f64,
}

impl Default for CoverOpts {
    fn default() -> Self {
        Self {
            template: "impact".into(),
            width: 1080,
            height: 1440,
            hook_title: String::new(),
            sub_title: String::new(),
            kicker: String::new(),
            show_name: String::new(),
            brand: String::new(),
            duration_sec: 0.0,
            accent: String::new(),
            font: String::new(),
            backdrop_path: None,
            crop_bottom: 0.0,
        }
    }
}

fn format_cover_duration(sec: f64) -> String {
    if sec <= 0.0 {
        return String::new();
    }
    let total = sec.round() as u64;
    let m = total / 60;
    let s = total % 60;
    format!("▶ {m:02}:{s:02}")
}

fn draw_fallback_backdrop(pm: &mut Pixmap, x: f32, y: f32, w: f32, h: f32) {
    fill_v_gradient(
        pm,
        x,
        y,
        w,
        h,
        Rgba(26, 28, 36, 255),
        Rgba(12, 13, 18, 255),
    );
}

fn load_image_source(src: &str) -> Option<DynamicImage> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("data:") {
        let (_, b64) = rest.split_once(",")?;
        let bytes = B64.decode(b64.trim()).ok()?;
        return image::load_from_memory(&bytes).ok();
    }
    image::open(Path::new(trimmed)).ok()
}

pub fn render_cover_png(opts: &CoverOpts) -> Result<Vec<u8>> {
    let w = opts.width.max(120);
    let h = opts.height.max(120);
    let mut pm = Pixmap::new(w, h).ok_or_else(|| anyhow!("failed to allocate cover pixmap"))?;

    let img = opts
        .backdrop_path
        .as_deref()
        .and_then(load_image_source);
    let subj_x = img
        .as_ref()
        .and_then(|im| {
            let small = im.resize_exact(96, 54, FilterType::Triangle).to_rgb8();
            subject_x(small.as_raw(), 96, 54)
        })
        .unwrap_or(0.5);

    let u = w.min(h) as f32 / 1080.0;
    let land = w as f32 > h as f32 * 1.2;
    let tall = h as f32 > w as f32 * 1.5;
    let dur = format_cover_duration(opts.duration_sec);
    let kicker = if !opts.kicker.trim().is_empty() {
        opts.kicker.trim().to_string()
    } else {
        opts.show_name.trim().to_string()
    };
    let title = if opts.hook_title.trim().is_empty() {
        " "
    } else {
        opts.hook_title.trim()
    };
    let sub = opts.sub_title.trim();
    let brand_footer = if !opts.brand.trim().is_empty() {
        opts.brand.trim().to_string()
    } else {
        opts.show_name.trim().to_string()
    };
    let pad_x = ((if land { 110.0 } else { 76.0 }) * u).round();
    let max_w = w as f32 - pad_x * 2.0;
    let title_lines = if land { 2 } else if tall { 4 } else { 3 };
    let serif = opts.font.trim().eq_ignore_ascii_case("serif");

    let hot = |fallback: Rgba| -> Rgba {
        if opts.accent.trim().is_empty() {
            fallback
        } else {
            parse_hex(&opts.accent, fallback)
        }
    };

    let draw_photo = |pm: &mut Pixmap, x: i32, y: i32, pw: u32, ph: u32, focus: f64| {
        if let Some(im) = img.as_ref() {
            draw_cover_image_cropped(pm, im, x, y, pw, ph, focus, opts.crop_bottom, subj_x, false);
        } else {
            draw_fallback_backdrop(pm, x as f32, y as f32, pw as f32, ph as f32);
        }
    };

    match opts.template.as_str() {
        "split" => {
            let ph = ((h as f32) * if land { 1.0 } else if tall { 0.5 } else { 0.52 }).round() as u32;
            let bx = ((w as f32) * 0.5).round() as u32;
            if land {
                draw_photo(&mut pm, 0, 0, bx, h, 0.4);
                fill_rect(&mut pm, bx as f32, 0.0, (w - bx) as f32, h as f32, Rgba(18, 18, 20, 255));
            } else {
                draw_photo(&mut pm, 0, 0, w, ph, 0.38);
                fill_rect(&mut pm, 0.0, ph as f32, w as f32, (h - ph) as f32, Rgba(18, 18, 20, 255));
            }
            let left = (if land { bx as f32 } else { 0.0 }) + pad_x;
            let width = (if land { (w - bx) as f32 } else { w as f32 }) - pad_x * 2.0;
            let ts = fit_title_size(title, width, if land { 3 } else { 4 }, (if tall { 112.0 } else { 96.0 }) * u, 54.0 * u, 1.24, serif);
            let (_, _, title_h) = measure_rich_text_lines(title, ts, ts * 1.24, width, Weight::BOLD, serif);
            let (_, _, sub_h) = measure_rich_text_lines(sub, 40.0 * u, 56.0 * u, width, Weight::SEMIBOLD, serif);
            let block_h = (if kicker.is_empty() { 0.0 } else { 80.0 * u }) + title_h + if sub.is_empty() { 0.0 } else { 24.0 * u + sub_h };
            let area_top = if land { 0.0 } else { ph as f32 };
            let area_h = (if land { h as f32 } else { (h - ph) as f32 }) - 80.0 * u;
            let mut y = area_top + ((area_h - block_h) * 0.5).max(46.0 * u);
            let hl = hot(Rgba(255, 210, 63, 255));
            if !kicker.is_empty() {
                draw_pill(&mut pm, &kicker, left, y, 28.0 * u, hl, ink_on_bg(hl), false);
                y += 80.0 * u;
            }
            y += draw_rich_text_block(
                &mut pm,
                title,
                left,
                y,
                TextDrawStyle {
                    size: ts,
                    line_height: ts * 1.24,
                    max_width: width,
                    color: Rgba(255, 255, 255, 255),
                    highlight_color: hl,
                    weight: Weight::BOLD,
                    serif,
                    ..Default::default()
                },
                Some(if land { 3 } else { 4 }),
            );
            if !sub.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    sub,
                    left,
                    y + 20.0 * u,
                    TextDrawStyle {
                        size: 40.0 * u,
                        line_height: 54.0 * u,
                        max_width: width,
                        color: Rgba(255, 255, 255, 220),
                        weight: Weight::SEMIBOLD,
                        serif,
                        ..Default::default()
                    },
                    Some(2),
                );
            }
            if !dur.is_empty() {
                draw_pill(&mut pm, &dur, w as f32 - pad_x, 40.0 * u, 26.0 * u, Rgba(0, 0, 0, 160), Rgba(255, 255, 255, 255), true);
            }
            if !brand_footer.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    &brand_footer,
                    if land { left } else { pad_x },
                    h as f32 - 58.0 * u,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: width,
                        color: Rgba(255, 255, 255, 160),
                        weight: Weight::SEMIBOLD,
                        align: if land { Align::Left } else { Align::Center },
                        serif,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
        }
        "bold" => {
            draw_photo(&mut pm, 0, 0, w, h, 0.38);
            fill_v_gradient(&mut pm, 0.0, 0.0, w as f32, h as f32, Rgba(0, 0, 0, 32), Rgba(0, 0, 0, 96));
            let hl = hot(Rgba(255, 225, 77, 255));
            let ts = fit_title_size(title, max_w, title_lines, 118.0 * u, 60.0 * u, 1.2, serif);
            let (_, _, title_h) = measure_rich_text_lines(title, ts, ts * 1.2, max_w, Weight::BOLD, serif);
            let mut y = (h as f32) * (if land { 0.44 } else if tall { 0.56 } else { 0.52 }) - title_h * 0.5;
            y += draw_rich_text_block(
                &mut pm,
                title,
                pad_x,
                y,
                TextDrawStyle {
                    size: ts,
                    line_height: ts * 1.2,
                    max_width: max_w,
                    color: Rgba(255, 255, 255, 255),
                    highlight_color: hl,
                    weight: Weight::BOLD,
                    serif,
                    align: Align::Center,
                    outline: Some((Rgba(17, 17, 17, 255), ((ts * 0.08).round() as i32).max(2))),
                    ..Default::default()
                },
                Some(title_lines),
            );
            if !sub.is_empty() {
                let (_, sub_w, sub_h) = measure_rich_text_lines(sub, 42.0 * u, 58.0 * u, max_w - 40.0 * u, Weight::SEMIBOLD, serif);
                let bw = (sub_w + 48.0 * u).min(max_w);
                fill_round_rect(&mut pm, (w as f32 - bw) * 0.5, y + 18.0 * u, bw, sub_h + 16.0 * u, 10.0 * u, Rgba(0, 0, 0, 204));
                draw_rich_text_block(
                    &mut pm,
                    sub,
                    pad_x,
                    y + 26.0 * u,
                    TextDrawStyle {
                        size: 42.0 * u,
                        line_height: 58.0 * u,
                        max_width: max_w,
                        color: Rgba(255, 255, 255, 255),
                        weight: Weight::SEMIBOLD,
                        align: Align::Center,
                        ..Default::default()
                    },
                    Some(2),
                );
            }
            if !kicker.is_empty() {
                draw_pill(&mut pm, &kicker, pad_x, 56.0 * u, 28.0 * u, Rgba(0, 0, 0, 168), hl, false);
            }
            if !brand_footer.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    &brand_footer,
                    pad_x,
                    h as f32 - 64.0 * u,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: max_w,
                        color: Rgba(255, 255, 255, 210),
                        weight: Weight::SEMIBOLD,
                        align: Align::Center,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
        }
        "quote" => {
            fill_rect(&mut pm, 0.0, 0.0, w as f32, h as f32, Rgba(244, 239, 230, 255));
            let hl = hot(Rgba(217, 72, 15, 255));
            let r = ((if land { 170.0 } else if tall { 120.0 } else { 96.0 }) * u).round() as i32;
            let left = pad_x;
            let width = if land { w as f32 - pad_x * 3.0 - (r * 2) as f32 } else { max_w };
            let ts = fit_title_size(title, width, title_lines, (if tall { 92.0 } else { 78.0 }) * u, 46.0 * u, 1.38, true);
            let (_, _, title_h) = measure_rich_text_lines(title, ts, ts * 1.38, width, Weight::BOLD, true);
            let (_, _, sub_h) = measure_rich_text_lines(sub, 38.0 * u, 54.0 * u, width, Weight::MEDIUM, false);
            let mark_h = 100.0 * u;
            let block_h = (if land { 0.0 } else { (r * 2) as f32 + 56.0 * u }) + mark_h + title_h + if sub.is_empty() { 0.0 } else { 26.0 * u + sub_h };
            let mut y = ((h as f32 - 140.0 * u - block_h) * 0.5).max(56.0 * u);
            let cx = if land { w as i32 - pad_x as i32 - r } else { (w / 2) as i32 };
            let cy = if land { (h / 2) as i32 } else { y.round() as i32 + r };
            if let Some(im) = img.as_ref() {
                draw_cover_image_cropped(&mut pm, im, cx - r, cy - r, (r * 2) as u32, (r * 2) as u32, 0.25, opts.crop_bottom, subj_x, true);
            }
            if !land {
                y += (r * 2) as f32 + 56.0 * u;
            }
            // i18n-ignore
            draw_rich_text_block(
                &mut pm,
                "“",
                left - 6.0 * u,
                y,
                TextDrawStyle {
                    size: 140.0 * u,
                    line_height: 140.0 * u,
                    max_width: 160.0 * u,
                    color: hl,
                    weight: Weight::BOLD,
                    serif: true,
                    ..Default::default()
                },
                Some(1),
            );
            y += mark_h;
            y += draw_rich_text_block(
                &mut pm,
                title,
                left,
                y,
                TextDrawStyle {
                    size: ts,
                    line_height: ts * 1.38,
                    max_width: width,
                    color: Rgba(24, 24, 27, 255),
                    highlight_color: hl,
                    weight: Weight::BOLD,
                    serif: true,
                    ..Default::default()
                },
                Some(title_lines),
            );
            if !sub.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    sub,
                    left,
                    y + 24.0 * u,
                    TextDrawStyle {
                        size: 38.0 * u,
                        line_height: 54.0 * u,
                        max_width: width,
                        color: Rgba(68, 64, 60, 255),
                        weight: Weight::MEDIUM,
                        ..Default::default()
                    },
                    Some(2),
                );
            }
            fill_rect(&mut pm, left, h as f32 - 118.0 * u, 80.0 * u, (3.0 * u).max(2.0), Rgba(24, 24, 27, 42));
            if !kicker.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    &kicker,
                    left,
                    h as f32 - 96.0 * u,
                    TextDrawStyle {
                        size: 30.0 * u,
                        line_height: 38.0 * u,
                        max_width: width * 0.7,
                        color: Rgba(63, 63, 70, 255),
                        weight: Weight::SEMIBOLD,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
            if !opts.brand.trim().is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    opts.brand.trim(),
                    pad_x,
                    h as f32 - 94.0 * u,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: max_w,
                        color: Rgba(87, 83, 78, 255),
                        weight: Weight::SEMIBOLD,
                        align: Align::Right,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
        }
        "magazine" => {
            let ph = ((h as f32) * if land { 0.6 } else { 0.52 }).round() as u32;
            fill_rect(&mut pm, 0.0, 0.0, w as f32, h as f32, Rgba(250, 247, 242, 255));
            draw_photo(&mut pm, 0, 0, w, ph, 0.36);
            fill_v_gradient(&mut pm, 0.0, ph as f32 - 90.0 * u, w as f32, 90.0 * u, Rgba(250, 247, 242, 0), Rgba(250, 247, 242, 255));
            if !dur.is_empty() {
                draw_pill(&mut pm, &dur, w as f32 - pad_x, 44.0 * u, 26.0 * u, Rgba(18, 18, 22, 184), Rgba(255, 255, 255, 255), true);
            }
            let hl = hot(Rgba(217, 72, 26, 255));
            let mut y = ph as f32 + (if land { 32.0 } else { 52.0 }) * u;
            if !kicker.is_empty() {
                y += draw_rich_text_block(
                    &mut pm,
                    &kicker,
                    pad_x,
                    y,
                    TextDrawStyle {
                        size: 30.0 * u,
                        line_height: 40.0 * u,
                        max_width: max_w,
                        color: hl,
                        weight: Weight::BOLD,
                        serif,
                        ..Default::default()
                    },
                    Some(1),
                ) + 12.0 * u;
            }
            let ts = fit_title_size(title, max_w, if land { 2 } else { 3 }, 82.0 * u, 50.0 * u, 1.24, serif);
            y += draw_rich_text_block(
                &mut pm,
                title,
                pad_x,
                y,
                TextDrawStyle {
                    size: ts,
                    line_height: ts * 1.24,
                    max_width: max_w,
                    color: Rgba(24, 24, 27, 255),
                    highlight_color: hl,
                    weight: Weight::BOLD,
                    serif,
                    ..Default::default()
                },
                Some(if land { 2 } else { 3 }),
            );
            if !sub.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    sub,
                    pad_x,
                    y + 20.0 * u,
                    TextDrawStyle {
                        size: 38.0 * u,
                        line_height: 54.0 * u,
                        max_width: max_w,
                        color: Rgba(68, 64, 60, 255),
                        weight: Weight::MEDIUM,
                        serif,
                        ..Default::default()
                    },
                    Some(2),
                );
            }
            fill_rect(&mut pm, pad_x, h as f32 - 100.0 * u, max_w, (2.0 * u).max(2.0), Rgba(230, 224, 214, 255));
            if !brand_footer.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    &brand_footer,
                    pad_x,
                    h as f32 - 76.0 * u,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: max_w,
                        color: Rgba(87, 83, 78, 255),
                        weight: Weight::SEMIBOLD,
                        serif,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
        }
        "cinema" => {
            fill_rect(&mut pm, 0.0, 0.0, w as f32, h as f32, Rgba(18, 17, 19, 255));
            let fw = (w as f32 - pad_x * 2.0).round() as u32;
            let fh = (if land { (h as f32) * 0.52 } else { (fw as f32) * 9.0 / 16.0 }).round() as u32;
            let top = (if land { 96.0 * u } else { (h as f32) * if tall { 0.16 } else { 0.12 } }).round() as i32;
            if !kicker.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    &kicker,
                    pad_x,
                    top as f32 - 54.0 * u,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: max_w * 0.7,
                        color: Rgba(255, 255, 255, 210),
                        weight: Weight::SEMIBOLD,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
            if !dur.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    &dur,
                    pad_x,
                    top as f32 - 54.0 * u,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: max_w,
                        color: Rgba(245, 166, 35, 255),
                        weight: Weight::SEMIBOLD,
                        align: Align::Right,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
            draw_photo(&mut pm, pad_x.round() as i32, top, fw, fh, 0.42);
            let hl = hot(Rgba(251, 191, 36, 255));
            let ts = fit_title_size(title, fw as f32, title_lines, (if tall { 92.0 } else { 80.0 }) * u, 48.0 * u, 1.32, true);
            let (_, _, title_h) = measure_rich_text_lines(title, ts, ts * 1.32, fw as f32, Weight::BOLD, true);
            let (_, _, sub_h) = measure_rich_text_lines(sub, 38.0 * u, 54.0 * u, fw as f32, Weight::MEDIUM, false);
            let block_h = title_h + if sub.is_empty() { 0.0 } else { 18.0 * u + sub_h };
            let below = h as f32 - (top as f32 + fh as f32) - 100.0 * u;
            let mut y = top as f32 + fh as f32 + ((below - block_h) * 0.5).max(24.0 * u);
            y += draw_rich_text_block(
                &mut pm,
                title,
                pad_x,
                y,
                TextDrawStyle {
                    size: ts,
                    line_height: ts * 1.32,
                    max_width: fw as f32,
                    color: Rgba(245, 242, 235, 255),
                    highlight_color: hl,
                    weight: Weight::BOLD,
                    serif: true,
                    align: Align::Center,
                    ..Default::default()
                },
                Some(title_lines),
            );
            if !sub.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    sub,
                    pad_x,
                    y + 18.0 * u,
                    TextDrawStyle {
                        size: 38.0 * u,
                        line_height: 54.0 * u,
                        max_width: fw as f32,
                        color: Rgba(245, 242, 235, 224),
                        weight: Weight::MEDIUM,
                        align: Align::Center,
                        ..Default::default()
                    },
                    Some(2),
                );
            }
            if !brand_footer.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    &brand_footer,
                    pad_x,
                    h as f32 - 64.0 * u,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: max_w,
                        color: Rgba(255, 255, 255, 155),
                        weight: Weight::SEMIBOLD,
                        align: Align::Center,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
        }
        "minimal" => {
            draw_photo(&mut pm, 0, 0, w, h, 0.4);
            fill_v_gradient(&mut pm, 0.0, h as f32 * 0.4, w as f32, h as f32 * 0.6, Rgba(0, 0, 0, 0), Rgba(0, 0, 0, 224));
            let hl = hot(Rgba(255, 210, 122, 255));
            let ts = fit_title_size(title, max_w, if land { 2 } else { 3 }, 84.0 * u, 48.0 * u, 1.34, true);
            let (_, _, title_h) = measure_rich_text_lines(title, ts, ts * 1.34, max_w, Weight::SEMIBOLD, true);
            let (_, _, sub_h) = measure_rich_text_lines(sub, 36.0 * u, 52.0 * u, max_w, Weight::MEDIUM, false);
            let footer_space = if !brand_footer.is_empty() { 130.0 * u } else { 90.0 * u };
            let mut y = h as f32 - footer_space - title_h - sub_h;
            y += draw_rich_text_block(
                &mut pm,
                title,
                pad_x,
                y,
                TextDrawStyle {
                    size: ts,
                    line_height: ts * 1.34,
                    max_width: max_w,
                    color: Rgba(255, 255, 255, 255),
                    highlight_color: hl,
                    weight: Weight::SEMIBOLD,
                    serif: true,
                    ..Default::default()
                },
                Some(if land { 2 } else { 3 }),
            );
            if !sub.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    sub,
                    pad_x,
                    y + 10.0 * u,
                    TextDrawStyle {
                        size: 36.0 * u,
                        line_height: 52.0 * u,
                        max_width: max_w,
                        color: Rgba(255, 255, 255, 230),
                        weight: Weight::MEDIUM,
                        ..Default::default()
                    },
                    Some(2),
                );
            }
            if !kicker.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    &kicker.to_uppercase(),
                    pad_x,
                    60.0 * u,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: max_w,
                        color: Rgba(255, 255, 255, 224),
                        weight: Weight::SEMIBOLD,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
            if !brand_footer.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    &brand_footer,
                    pad_x,
                    h as f32 - 74.0 * u,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: max_w,
                        color: Rgba(255, 255, 255, 200),
                        weight: Weight::SEMIBOLD,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
        }
        _ => {
            // impact / marker
            let is_marker = opts.template == "marker";
            draw_photo(&mut pm, 0, 0, w, h, 0.38);
            fill_v_gradient(&mut pm, 0.0, 0.0, w as f32, h as f32 * 0.3, Rgba(8, 8, 12, 140), Rgba(8, 8, 12, 0));
            fill_v_gradient(&mut pm, 0.0, h as f32 * 0.38, w as f32, h as f32 * 0.62, Rgba(8, 8, 12, 0), Rgba(8, 8, 12, 235));
            let hl = hot(Rgba(255, 210, 63, 255));
            let top = (56.0 * u).round();
            if !kicker.is_empty() {
                draw_pill(&mut pm, &kicker, pad_x, top, 28.0 * u, Rgba(18, 18, 22, 188), hl, false);
            }
            if !dur.is_empty() {
                draw_pill(&mut pm, &dur, w as f32 - pad_x, top, 28.0 * u, Rgba(18, 18, 22, 188), Rgba(255, 255, 255, 255), true);
            }
            let ts = fit_title_size(title, max_w, title_lines, (if land { 96.0 } else { 100.0 }) * u, 54.0 * u, 1.26, serif);
            let (_, _, title_h) = measure_rich_text_lines(title, ts, ts * 1.26, max_w, Weight::BOLD, serif);
            let (_, _, sub_h) = measure_rich_text_lines(sub, 44.0 * u, 60.0 * u, max_w, Weight::SEMIBOLD, serif);
            let sub_total = if sub.is_empty() { 0.0 } else { sub_h + 24.0 * u };
            let mut y = h as f32 - (if land { 100.0 } else if tall { 220.0 } else { 130.0 }) * u - title_h - sub_total;
            y += draw_rich_text_block(
                &mut pm,
                title,
                pad_x,
                y,
                TextDrawStyle {
                    size: ts,
                    line_height: ts * 1.26,
                    max_width: max_w,
                    color: Rgba(255, 255, 255, 255),
                    highlight_color: hl,
                    weight: Weight::BOLD,
                    serif,
                    align: Align::Center,
                    marker: if is_marker { Some((hl, ink_on_bg(hl))) } else { None },
                    ..Default::default()
                },
                Some(title_lines),
            );
            if !sub.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    sub,
                    pad_x,
                    y + 22.0 * u,
                    TextDrawStyle {
                        size: 44.0 * u,
                        line_height: 60.0 * u,
                        max_width: max_w,
                        color: Rgba(255, 255, 255, 240),
                        weight: Weight::SEMIBOLD,
                        serif,
                        align: Align::Center,
                        ..Default::default()
                    },
                    Some(2),
                );
            }
            if !brand_footer.is_empty() {
                draw_rich_text_block(
                    &mut pm,
                    &brand_footer,
                    pad_x,
                    h as f32 - (if land { 50.0 } else { 80.0 }) * u,
                    TextDrawStyle {
                        size: 28.0 * u,
                        line_height: 36.0 * u,
                        max_width: max_w,
                        color: Rgba(255, 255, 255, 190),
                        weight: Weight::SEMIBOLD,
                        serif,
                        align: Align::Center,
                        ..Default::default()
                    },
                    Some(1),
                );
            }
        }
    }

    Ok(pm.encode_png()?)
}

struct PosterTheme {
    bg: Rgba,
    card: Rgba,
    ink: Rgba,
    sub: Rgba,
    muted: Rgba,
    border: Rgba,
    accent: Rgba,
    qr_dark: Rgba,
    qr_light: Rgba,
}

fn poster_theme(name: &str) -> PosterTheme {
    match name.trim().to_ascii_lowercase().as_str() {
        "warm" => PosterTheme {
            bg: Rgba(245, 239, 228, 255),
            card: Rgba(255, 253, 248, 255),
            ink: Rgba(35, 30, 24, 255),
            sub: Rgba(92, 81, 69, 255),
            muted: Rgba(147, 134, 119, 255),
            border: Rgba(228, 218, 203, 255),
            accent: Rgba(194, 65, 12, 255),
            qr_dark: Rgba(35, 30, 24, 255),
            qr_light: Rgba(255, 253, 248, 255),
        },
        "night" => PosterTheme {
            bg: Rgba(18, 19, 22, 255),
            card: Rgba(28, 29, 34, 255),
            ink: Rgba(242, 242, 243, 255),
            sub: Rgba(182, 184, 192, 255),
            muted: Rgba(125, 128, 140, 255),
            border: Rgba(43, 45, 53, 255),
            accent: Rgba(245, 158, 11, 255),
            qr_dark: Rgba(18, 19, 22, 255),
            qr_light: Rgba(245, 245, 247, 255),
        },
        "bamboo" => PosterTheme {
            bg: Rgba(239, 244, 238, 255),
            card: Rgba(251, 253, 249, 255),
            ink: Rgba(27, 38, 27, 255),
            sub: Rgba(74, 92, 74, 255),
            muted: Rgba(126, 145, 126, 255),
            border: Rgba(215, 226, 213, 255),
            accent: Rgba(45, 106, 79, 255),
            qr_dark: Rgba(27, 38, 27, 255),
            qr_light: Rgba(251, 253, 249, 255),
        },
        "sand" => PosterTheme {
            bg: Rgba(239, 232, 219, 255),
            card: Rgba(249, 245, 237, 255),
            ink: Rgba(38, 33, 26, 255),
            sub: Rgba(94, 84, 70, 255),
            muted: Rgba(142, 130, 114, 255),
            border: Rgba(223, 213, 195, 255),
            accent: Rgba(154, 52, 18, 255),
            qr_dark: Rgba(38, 33, 26, 255),
            qr_light: Rgba(249, 245, 237, 255),
        },
        _ => PosterTheme {
            bg: Rgba(250, 248, 244, 255),
            card: Rgba(255, 255, 255, 255),
            ink: Rgba(26, 26, 26, 255),
            sub: Rgba(85, 85, 85, 255),
            muted: Rgba(138, 133, 124, 255),
            border: Rgba(232, 227, 216, 255),
            accent: Rgba(217, 83, 47, 255),
            qr_dark: Rgba(26, 26, 26, 255),
            qr_light: Rgba(255, 255, 255, 255),
        },
    }
}

#[derive(Clone, Debug, Default)]
pub struct PosterOpts {
    pub theme: String,
    pub show_title: String,
    pub episode_title: String,
    pub time_label: String,
    pub quote_orig: String,
    pub quote_zh: String,
    pub cover_path: Option<String>,
    pub share_url: Option<String>,
    pub brand_title: Option<String>,
    pub brand_tagline: Option<String>,
}

fn draw_qr(pm: &mut Pixmap, data: &str, x: i32, y: i32, size: u32, dark: Rgba, light: Rgba) {
    let Ok(code) = QrCode::new(data.as_bytes()) else { return };
    let n = code.width();
    if n == 0 {
        return;
    }
    let margin = 2usize;
    let total = n + margin * 2;
    fill_round_rect(
        pm,
        x as f32,
        y as f32,
        size as f32,
        size as f32,
        12.0,
        light,
    );
    let cell = size as f32 / total as f32;
    for row in 0..n {
        for col in 0..n {
            if code[(col, row)] == qrcode::Color::Dark {
                let rx = x as f32 + (col + margin) as f32 * cell;
                let ry = y as f32 + (row + margin) as f32 * cell;
                fill_rect(pm, rx, ry, cell.ceil(), cell.ceil(), dark);
            }
        }
    }
}

pub fn render_poster_png(opts: &PosterOpts) -> Result<Vec<u8>> {
    // i18n-ignore-start
    let default_show = "播客";
    let default_brand = "SubCast";
    let default_tagline = "海外好播客 · 中文字幕 · 精读与 AI 问答";
    let qr_hint = "扫码收听原节目这一段 →";
    // i18n-ignore-end

    let th = poster_theme(&opts.theme);
    let w = 1080u32;
    let outer_pad = 64.0f32;
    let card_pad = 56.0f32;
    let inner_w = w as f32 - (outer_pad + card_pad) * 2.0;

    let orig = opts.quote_orig.trim();
    let zh = opts.quote_zh.trim();
    let show = if opts.show_title.trim().is_empty() {
        default_show
    } else {
        opts.show_title.trim()
    };

    let orig_sz = if orig.chars().count() > 260 { 36.0 } else { 42.0 };
    let orig_lh = orig_sz * 1.48;
    let zh_sz = if zh.chars().count() > 180 { 32.0 } else { 36.0 };
    let zh_lh = zh_sz * 1.54;

    let (_, _, ep_h) = measure_rich_text_lines(
        opts.episode_title.trim(),
        28.0,
        38.0,
        inner_w - 110.0,
        Weight::MEDIUM,
        false,
    );
    let (_, _, orig_h) = if orig.is_empty() {
        (0, 0.0, 0.0)
    } else {
        measure_rich_text_lines(orig, orig_sz, orig_lh, inner_w, Weight::SEMIBOLD, true)
    };
    let (_, _, zh_h) = if zh.is_empty() {
        (0, 0.0, 0.0)
    } else {
        measure_rich_text_lines(zh, zh_sz, zh_lh, inner_w, Weight::NORMAL, false)
    };

    let header_h = (92.0f32).max(40.0 + ep_h + 24.0);
    let body_gap = if !orig.is_empty() && !zh.is_empty() { 32.0 } else { 0.0 };
    let footer_h = if opts.share_url.as_deref().map(|s| !s.is_empty()).unwrap_or(false) {
        160.0
    } else {
        92.0
    };
    let card_h = (card_pad * 2.0 + header_h + 54.0 + orig_h + body_gap + zh_h + 64.0 + footer_h).ceil();
    let h = ((card_h + outer_pad * 2.0).round() as u32).clamp(640, 2400);

    let mut pm = Pixmap::new(w, h).ok_or_else(|| anyhow!("failed to allocate poster pixmap"))?;
    fill_rect(&mut pm, 0.0, 0.0, w as f32, h as f32, th.bg);

    let cx = outer_pad;
    let cy = outer_pad;
    let cw = w as f32 - outer_pad * 2.0;
    let ch = h as f32 - outer_pad * 2.0;
    fill_round_rect(&mut pm, cx, cy, cw, ch, 28.0, th.card);
    stroke_round_rect(&mut pm, cx, cy, cw, ch, 28.0, 2.0, th.border);

    let ix = cx + card_pad;
    let mut y = cy + card_pad;

    let thumb_sz = 84u32;
    let thumb_img = opts
        .cover_path
        .as_deref()
        .and_then(load_image_source)
        .or_else(|| image::load_from_memory(LOGO_PNG).ok());
    if let Some(im) = thumb_img {
        draw_cover_image_cropped(
            &mut pm,
            &im,
            ix.round() as i32,
            y.round() as i32,
            thumb_sz,
            thumb_sz,
            0.5,
            0.0,
            0.5,
            false,
        );
    }

    let meta_x = ix + thumb_sz as f32 + 24.0;
    let meta_w = inner_w - thumb_sz as f32 - 24.0;
    let show_line = if opts.time_label.trim().is_empty() {
        show.to_string()
    } else {
        format!("{show} · {}", opts.time_label.trim())
    };
    draw_rich_text_block(
        &mut pm,
        &show_line,
        meta_x,
        y + 4.0,
        TextDrawStyle {
            size: 24.0,
            line_height: 32.0,
            max_width: meta_w,
            color: th.accent,
            weight: Weight::SEMIBOLD,
            ..Default::default()
        },
        Some(1),
    );
    if !opts.episode_title.trim().is_empty() {
        draw_rich_text_block(
            &mut pm,
            opts.episode_title.trim(),
            meta_x,
            y + 42.0,
            TextDrawStyle {
                size: 26.0,
                line_height: 36.0,
                max_width: meta_w,
                color: th.sub,
                weight: Weight::MEDIUM,
                ..Default::default()
            },
            Some(2),
        );
    }

    y += header_h + 24.0;
    fill_rect(&mut pm, ix, y, inner_w, 2.0, th.border);
    y += 34.0;

    if !orig.is_empty() {
        y += draw_rich_text_block(
            &mut pm,
            orig,
            ix,
            y,
            TextDrawStyle {
                size: orig_sz,
                line_height: orig_lh,
                max_width: inner_w,
                color: th.ink,
                highlight_color: th.accent,
                weight: Weight::SEMIBOLD,
                serif: true,
                ..Default::default()
            },
            None,
        );
    }
    if !orig.is_empty() && !zh.is_empty() {
        y += 28.0;
    }
    if !zh.is_empty() {
        let _ = draw_rich_text_block(
            &mut pm,
            zh,
            ix,
            y,
            TextDrawStyle {
                size: zh_sz,
                line_height: zh_lh,
                max_width: inner_w,
                color: th.sub,
                highlight_color: th.accent,
                weight: Weight::NORMAL,
                ..Default::default()
            },
            None,
        );
    }

    let footer_top = cy + ch - card_pad - footer_h;
    fill_rect(&mut pm, ix, footer_top, inner_w, 2.0, th.border);

    let fy = footer_top + 28.0;
    let brand = opts
        .brand_title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_brand);
    let tagline = opts
        .brand_tagline
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_tagline);

    let has_qr = opts
        .share_url
        .as_deref()
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let qr_sz = 124u32;
    let text_w = if has_qr {
        inner_w - qr_sz as f32 - 28.0
    } else {
        inner_w
    };

    if let Ok(logo_im) = image::load_from_memory(LOGO_PNG) {
        let resized = logo_im.resize_exact(40, 40, FilterType::Triangle).to_rgba8();
        blit_rgba_image(&mut pm, &resized, ix.round() as i32, fy.round() as i32, false);
    }
    draw_rich_text_block(
        &mut pm,
        brand,
        ix + 52.0,
        fy + 4.0,
        TextDrawStyle {
            size: 28.0,
            line_height: 36.0,
            max_width: text_w - 52.0,
            color: th.ink,
            weight: Weight::BOLD,
            ..Default::default()
        },
        Some(1),
    );
    draw_rich_text_block(
        &mut pm,
        tagline,
        ix,
        fy + 52.0,
        TextDrawStyle {
            size: 22.0,
            line_height: 30.0,
            max_width: text_w,
            color: th.muted,
            ..Default::default()
        },
        Some(1),
    );
    if has_qr {
        draw_rich_text_block(
            &mut pm,
            qr_hint,
            ix,
            fy + 92.0,
            TextDrawStyle {
                size: 22.0,
                line_height: 30.0,
                max_width: text_w,
                color: th.accent,
                weight: Weight::SEMIBOLD,
                ..Default::default()
            },
            Some(1),
        );
        if let Some(url) = opts.share_url.as_deref() {
            draw_qr(
                &mut pm,
                url.trim(),
                (ix + inner_w - qr_sz as f32).round() as i32,
                (fy - 4.0).round() as i32,
                qr_sz,
                th.qr_dark,
                th.qr_light,
            );
        }
    }

    Ok(pm.encode_png()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlights::{clip_geometry, CardCrop, ClipAspect, ClipFit};
    use crate::videoclip::png_dimensions;

    const PNG_MAGIC: &[u8] = &[0x89, 80, 78, 71, 13, 10, 26, 10];

    #[test]
    fn renders_overlay_and_cues() {
        let geom = clip_geometry(ClipAspect::V9x16, ClipFit::Card, 1920, 1080, CardCrop::C4x3, true);
        let overlay = render_static_overlay_png(&StaticOverlayOpts {
            geom,
            show_name: "No Priors".into(),
            title: "Why 【Small Teams】 Beat Giants".into(),
            card_bg: "dark".into(),
            accent: "#ffd23f".into(),
            show_brand: true,
            brand_text: "SubCast".into(),
        })
        .unwrap();
        assert!(overlay.starts_with(PNG_MAGIC));
        assert_eq!(png_dimensions(&overlay), Some((1080, 1920)));

        let cues = render_cue_overlays(
            &geom,
            &[Cue {
                start: 0.2,
                end: 2.5,
                orig: "Small teams iterate faster.".into(),
                zh: Some("小团队迭代得更快。".into()),
            }],
            true,
            "dark",
            "#ffd23f",
        )
        .unwrap();
        assert_eq!(cues.len(), 1);
        let cue_bytes = B64.decode(&cues[0].png_base64).unwrap();
        assert!(cue_bytes.starts_with(PNG_MAGIC));
        assert_eq!(png_dimensions(&cue_bytes).map(|d| d.0), Some(1080));
    }

    #[test]
    fn renders_cover_templates_and_posters() {
        for tmpl in ["impact", "split", "quote", "bold"] {
            let png = render_cover_png(&CoverOpts {
                template: tmpl.into(),
                width: 1080,
                height: 1440,
                hook_title: "Why 【Taste】 Beats Scale".into(),
                sub_title: "Episode 108 highlight".into(),
                kicker: "Founders".into(),
                show_name: "Founders".into(),
                brand: "SubCast".into(),
                duration_sec: 142.0,
                ..Default::default()
            })
            .unwrap();
            assert!(png.starts_with(PNG_MAGIC));
            assert_eq!(png_dimensions(&png), Some((1080, 1440)));
        }

        for theme in ["paper", "night"] {
            let png = render_poster_png(&PosterOpts {
                theme: theme.into(),
                show_title: "Lex Fridman Podcast".into(),
                episode_title: "#400 — Conversations on AI".into(),
                time_label: "04:12 – 04:38".into(),
                quote_orig: "Simplicity is the hardest thing to engineer.".into(),
                quote_zh: "简单是最难设计出来的东西。".into(),
                share_url: Some("https://subcast.qiaomu.ai/e/demo?t=252".into()),
                ..Default::default()
            })
            .unwrap();
            assert!(png.starts_with(PNG_MAGIC));
            let (pw, ph) = png_dimensions(&png).unwrap();
            assert_eq!(pw, 1080);
            assert!(ph >= 640);
        }
    }
}
