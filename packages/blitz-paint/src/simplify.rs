//! Simplified paint for small projected features, asked for per element with
//! the [`SIMPLIFY_ATTR`] attribute. It changes only what is painted: style,
//! layout and hit testing never read the attribute, so boxes and picking stay
//! those of the full document.
//!
//! Under a hinted element, two substitutions apply:
//!
//! - An inline layout whose every run projects below `text` device pixels of
//!   font size paints each glyph run as one filled box of its glyphs' ink
//!   bounds, in the run's colour at [`INK_COVERAGE`], instead of its glyphs.
//!   Decorations go with the glyphs; inline backgrounds and selection stay.
//!   A layout with `text-overflow`, a faux-italic run or a glyph without
//!   outline bounds keeps its glyphs.
//! - A descendant element whose border box projects below `frame` device
//!   pixels on both sides, and whose children are laid out inside it, paints
//!   its own background, border, shadows and outline and none of its content.
//!
//! Both only remove or replace paint with paint inside what the glyphs or
//! the element would have covered. Out-of-flow descendants that a stacking
//! context outside the hinted element paints are drawn in full.

use anyrender::PaintScene;
use blitz_dom::node::TextBrush;
use blitz_dom::{BaseDocument, NodeId};
use kurbo::{Affine, Rect};
use parley::{Layout, PositionedLayoutItem};
use peniko::Fill;

use crate::FONT_EMBOLDEN_ENABLED;
use crate::text::{glyph_style, synthetic_bold};
use blitz_dom::util::ToColorColor as _;

/// The attribute that asks for simplified paint of an element and its
/// descendants, as space-separated `key=value` pairs: `text=<px>` and
/// `frame=<px>`, both in device pixels. A missing or unparsable pair turns
/// that substitution off.
pub const SIMPLIFY_ATTR: &str = "data-blitz-simplify";

/// How much of a run's ink box the replacement covers: the share of the box
/// glyphs of body text cover, roughly, so the box reads at its weight.
pub const INK_COVERAGE: f32 = 0.4;

/// The thresholds a hint sets. Zero turns a substitution off.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Simplify {
    pub(crate) text: f64,
    pub(crate) frame: f64,
}

impl Simplify {
    pub(crate) fn parse(value: &str) -> Simplify {
        let mut out = Simplify::default();
        for pair in value.split_ascii_whitespace() {
            let Some((key, v)) = pair.split_once('=') else {
                continue;
            };
            let v = v.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0);
            match key {
                "text" => out.text = v.unwrap_or(0.0),
                "frame" => out.frame = v.unwrap_or(0.0),
                _ => {}
            }
        }
        out
    }
}

/// What one paint pass simplified.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PaintStats {
    /// Hinted elements painted, past culling.
    pub hinted: u32,
    /// Inline layouts painted as ink boxes.
    pub text_boxes: u32,
    /// Elements painted without their content.
    pub flattened: u32,
}

/// How many device pixels one unit of `transform`'s output space spans
/// along an axis, on average.
pub(crate) fn linear_scale(transform: Affine) -> f64 {
    transform.determinant().abs().sqrt()
}

/// Paints `layout`'s glyph runs as ink boxes if it qualifies, and says
/// whether it did. `transform` places the layout as `stroke_text` would;
/// `text` is the threshold in device pixels.
pub(crate) fn draw_ink_boxes(
    scene: &mut impl PaintScene,
    layout: &Layout<TextBrush>,
    doc: &BaseDocument,
    transform: Affine,
    scale: f64,
    text: f64,
    boxes: &mut Vec<(Rect, crate::color::Color)>,
) -> bool {
    use skrifa::MetadataProvider as _;
    use skrifa::instance::{LocationRef, Size};

    boxes.clear();
    let k = linear_scale(transform);
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let run = glyph_run.run();
            let font_size = run.font_size();
            if f64::from(font_size) * k >= text || run.synthesis().skew().is_some() {
                return false;
            }
            let font = run.font();
            let Ok(face) = skrifa::FontRef::from_index(font.data.as_ref(), font.index) else {
                return false;
            };
            let raw: &[i16] = bytemuck::cast_slice(run.normalized_coords());
            let coords: Vec<skrifa::raw::types::F2Dot14> = raw
                .iter()
                .map(|&c| skrifa::raw::types::F2Dot14::from_bits(c))
                .collect();
            let metrics = face.glyph_metrics(Size::new(font_size), LocationRef::new(&coords));

            let node_id: NodeId = glyph_run.style().brush.id;
            let bold = synthetic_bold(doc, node_id, run);
            let hinted = !(FONT_EMBOLDEN_ENABLED || bold.is_some());
            let embolden = bold.unwrap_or_else(|| glyph_style(font_size, scale).1);

            let baseline = f64::from(glyph_run.baseline());
            let mut pen = glyph_run.offset();
            let mut ink: Option<Rect> = None;
            for glyph in glyph_run.glyphs() {
                let at = f64::from(pen + glyph.x);
                pen += glyph.advance;
                let Some(b) = metrics.bounds(skrifa::GlyphId::new(glyph.id)) else {
                    return false;
                };
                if b.x_max <= b.x_min || b.y_max <= b.y_min {
                    continue;
                }
                let y = baseline + f64::from(glyph.y);
                let r = Rect::new(
                    at + f64::from(b.x_min),
                    y - f64::from(b.y_max),
                    at + f64::from(b.x_max),
                    y - f64::from(b.y_min),
                )
                .inflate(embolden.x, embolden.y);
                ink = Some(ink.map_or(r, |i| i.union(r)));
            }
            let Some(mut ink) = ink else {
                continue;
            };
            // Hinting can pull an outline's edges up to half a device pixel
            // inside its unhinted bounds; keep the box inside either.
            if hinted {
                let half = 0.5 / k.max(f64::EPSILON);
                ink = ink.inset(-half);
                if ink.width() <= 0.0 || ink.height() <= 0.0 {
                    continue;
                }
            }
            let color = doc
                .get_node(node_id)
                .and_then(|n| n.primary_styles())
                .map(|s| s.get_inherited_text().color.as_color_color())
                .unwrap_or(crate::color::Color::BLACK);
            boxes.push((ink, color.multiply_alpha(INK_COVERAGE)));
        }
    }
    for (rect, color) in boxes.iter() {
        scene.fill(Fill::NonZero, transform, *color, None, rect);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_pairs_and_ignores_the_rest() {
        assert_eq!(
            Simplify::parse("text=6 frame=2.5"),
            Simplify {
                text: 6.0,
                frame: 2.5
            }
        );
        assert_eq!(
            Simplify::parse("frame=3 depth=9 junk"),
            Simplify {
                text: 0.0,
                frame: 3.0
            }
        );
        assert_eq!(Simplify::parse("text=-1 frame=NaN"), Simplify::default());
        assert_eq!(Simplify::parse(""), Simplify::default());
    }
}
