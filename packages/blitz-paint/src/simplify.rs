//! Simplified paint for small projected text, asked for per element with
//! the [`SIMPLIFY_ATTR`] attribute. It changes only what is painted: style,
//! layout and hit testing never read the attribute, so boxes and picking stay
//! those of the full document.
//!
//! Under a hinted element, an inline layout whose every run projects below
//! `text` device pixels of font size, under a transform that only scales
//! and translates, paints each glyph run as one filled box of its glyphs'
//! outline bounds, in the run's colour at [`INK_COVERAGE`], instead of its
//! glyphs. A rotated or skewed run keeps its glyphs: its box would reach
//! past the turned glyphs' extrema.
//! Decorations go with the glyphs; inline backgrounds and selection stay. A
//! layout with `text-overflow`, a faux-italic run, a glyph whose outline
//! can't be read, or a box crossing the clip rectangle around it keeps its
//! glyphs. Clips other than that rectangle, such as `clip-path` or rounded
//! overflow corners, aren't checked.
//!
//! The box lies inside what the glyphs cover: it is the unhinted outlines'
//! exact bounds, not grown for synthetic bold, and shrunk by half a device
//! pixel where hinting may pull an edge in. Out-of-flow descendants that a
//! stacking context outside the hinted element paints are drawn in full.
//!
//! Nothing else is substituted. Dropping a small element's content would
//! need its descendants' painted bounds, shadows and outlines included,
//! which layout's scrollable overflow doesn't give.

use anyrender::PaintScene;
use blitz_dom::glyph_ink::{Face, GlyphInk};
use blitz_dom::node::TextBrush;
use blitz_dom::{BaseDocument, NodeId};
use kurbo::{Affine, Rect};
use parley::{Layout, PositionedLayoutItem};
use peniko::Fill;

use crate::FONT_EMBOLDEN_ENABLED;
use crate::text::synthetic_bold;
use blitz_dom::util::ToColorColor as _;

/// The attribute that asks for simplified paint of an element and its
/// descendants, as space-separated `key=value` pairs: `text=<px>`, in device
/// pixels. A missing or unparsable pair turns the substitution off, and
/// other keys are ignored. Setting or clearing it only requests a redraw.
pub use blitz_dom::SIMPLIFY_ATTR;

/// How much of a run's ink box the replacement covers: the share of the box
/// glyphs of body text cover, roughly, so the box reads at its weight.
pub const INK_COVERAGE: f32 = 0.4;

/// The threshold a hint sets. Zero turns the substitution off.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Simplify {
    pub(crate) text: f64,
}

impl Simplify {
    pub(crate) fn parse(value: &str) -> Simplify {
        let mut out = Simplify::default();
        for pair in value.split_ascii_whitespace() {
            let Some((key, v)) = pair.split_once('=') else {
                continue;
            };
            let v = v.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0);
            if key == "text" {
                out.text = v.unwrap_or(0.0);
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
}

/// Whether `transform` keeps a rectangle a rectangle with the same edges:
/// scales and translations only, mirrors included. A rotated or skewed
/// box reaches past the extrema of the glyphs inside it.
pub(crate) fn axis_aligned(transform: Affine) -> bool {
    let [a, b, c, d, _, _] = transform.as_coeffs();
    let tolerance = 1e-9 * (a.abs() + d.abs()).max(f64::MIN_POSITIVE);
    b.abs() <= tolerance && c.abs() <= tolerance
}

/// The least and most a unit length is stretched by `transform`'s linear
/// part, over every direction: its singular values. Skew and unequal axis
/// scales give different values; a rotation or uniform scale the same.
pub(crate) fn stretch(transform: Affine) -> (f64, f64) {
    let [a, b, c, d, _, _] = transform.as_coeffs();
    let sum = a * a + b * b + c * c + d * d;
    let det = a * d - b * c;
    let root = (sum * sum - 4.0 * det * det).max(0.0).sqrt();
    (
        ((sum - root) / 2.0).max(0.0).sqrt(),
        ((sum + root) / 2.0).sqrt(),
    )
}

/// Paints `layout`'s glyph runs as ink boxes if it qualifies, and says
/// whether it did. `transform` places the layout as `stroke_text` would;
/// `text` is the threshold in device pixels. Every box must lie inside
/// `clip`, in the coordinates `screen` maps the layout to: a box spans the
/// gaps between its glyphs, so a clip through it could leave paint where
/// the clipped glyphs leave none.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_ink_boxes(
    scene: &mut impl PaintScene,
    layout: &Layout<TextBrush>,
    doc: &BaseDocument,
    transform: Affine,
    screen: Affine,
    clip: Rect,
    text: f64,
    boxes: &mut Vec<(Rect, crate::color::Color)>,
) -> bool {
    boxes.clear();
    if !axis_aligned(transform) {
        return false;
    }
    let (least, most) = stretch(transform);
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let run = glyph_run.run();
            let font_size = run.font_size();
            // NaN from a degenerate transform fails the comparison too.
            let projected = f64::from(font_size) * most;
            if projected.is_nan() || projected >= text || run.synthesis().skew().is_some() {
                return false;
            }
            let face = Face::of(run);
            let em = f64::from(font_size) / f64::from(face.units_per_em().max(1));

            let node_id: NodeId = glyph_run.style().brush.id;
            // Emboldening only grows the outlines, so their bounds stay
            // inside the ink; it matters here only because it turns hinting
            // off.
            let bold = synthetic_bold(doc, node_id, run);
            let hinted = !(FONT_EMBOLDEN_ENABLED || bold.is_some());

            let baseline = f64::from(glyph_run.baseline());
            let mut pen = glyph_run.offset();
            let mut ink: Option<Rect> = None;
            for glyph in glyph_run.glyphs() {
                let at = f64::from(pen + glyph.x);
                pen += glyph.advance;
                let b = match face.ink(glyph.id) {
                    GlyphInk::Box(b) => b,
                    GlyphInk::Empty => continue,
                    GlyphInk::Unknown => return false,
                };
                let y = baseline + f64::from(glyph.y);
                let r = Rect::new(
                    at + f64::from(b[0]) * em,
                    y - f64::from(b[3]) * em,
                    at + f64::from(b[2]) * em,
                    y - f64::from(b[1]) * em,
                );
                ink = Some(ink.map_or(r, |i| i.union(r)));
            }
            let Some(mut ink) = ink else {
                continue;
            };
            // Hinting can pull an outline's edges up to half a device pixel
            // inside its unhinted bounds; keep the box inside either. Half a
            // device pixel spans the most layout units along the least
            // stretched direction.
            if hinted {
                let half = 0.5 / least.max(f64::EPSILON);
                ink = ink.inset(-half);
                if ink.width() <= 0.0 || ink.height() <= 0.0 {
                    continue;
                }
            }
            let on_screen = screen.transform_rect_bbox(ink);
            if on_screen.x0 < clip.x0
                || on_screen.y0 < clip.y0
                || on_screen.x1 > clip.x1
                || on_screen.y1 > clip.y1
            {
                return false;
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
    fn only_scales_and_translations_are_axis_aligned() {
        assert!(axis_aligned(Affine::IDENTITY));
        assert!(axis_aligned(Affine::scale_non_uniform(10.0, 0.1)));
        assert!(axis_aligned(Affine::translate((5.0, 7.0)) * Affine::FLIP_X));
        assert!(!axis_aligned(Affine::rotate(0.01)));
        assert!(!axis_aligned(Affine::skew(0.2, 0.0)));
    }

    #[test]
    fn stretch_is_the_least_and_most_scale_over_directions() {
        let close = |(a, b): (f64, f64), (c, d): (f64, f64)| {
            assert!((a - c).abs() < 1e-9 && (b - d).abs() < 1e-9, "{a} {b}");
        };
        close(stretch(Affine::scale(2.0)), (2.0, 2.0));
        close(stretch(Affine::scale_non_uniform(10.0, 0.1)), (0.1, 10.0));
        close(
            stretch(Affine::rotate(1.0) * Affine::scale(3.0)),
            (3.0, 3.0),
        );
        // skewX(45deg): [1 1; 0 1] stretches by the golden ratio.
        let phi = (1.0 + 5f64.sqrt()) / 2.0;
        close(stretch(Affine::skew(1.0, 0.0)), (1.0 / phi, phi));
        close(stretch(Affine::scale(0.0)), (0.0, 0.0));
    }

    #[test]
    fn parses_known_pairs_and_ignores_the_rest() {
        assert_eq!(Simplify::parse("text=6"), Simplify { text: 6.0 });
        assert_eq!(
            Simplify::parse("frame=3 text=2.5 junk"),
            Simplify { text: 2.5 }
        );
        assert_eq!(Simplify::parse("text=-1"), Simplify::default());
        assert_eq!(Simplify::parse("text=NaN"), Simplify::default());
        assert_eq!(Simplify::parse(""), Simplify::default());
    }
}
