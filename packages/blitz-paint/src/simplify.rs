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
use std::collections::HashMap;

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
            let face = Face::of(run);
            let em = f64::from(font_size) / f64::from(face.units_per_em);

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
                let b = match face.ink(glyph.id) {
                    Ink::Box(b) => b,
                    Ink::Empty => continue,
                    Ink::Unknown => return false,
                };
                let y = baseline + f64::from(glyph.y);
                let r = Rect::new(
                    at + f64::from(b[0]) * em,
                    y - f64::from(b[3]) * em,
                    at + f64::from(b[2]) * em,
                    y - f64::from(b[1]) * em,
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

/// A glyph's ink in font units at a run's variation coordinates: its
/// outline's exact bounds, y up.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Ink {
    Box([f32; 4]),
    /// No outline to paint, as for a space.
    Empty,
    /// No outline could be read; the layout keeps its glyphs.
    Unknown,
}

/// Font, face index and variation coordinates, hashed.
type FaceKey = (u64, u32, u64);

/// Ink of every glyph seen, kept across paints so a zoom reads it from
/// here rather than from the font. Cleared whole when it grows past
/// [`INK_CACHE_LIMIT`] glyphs.
const INK_CACHE_LIMIT: usize = 1 << 16;

thread_local! {
    static INK_CACHE: std::cell::RefCell<HashMap<(FaceKey, u32), Ink>> =
        std::cell::RefCell::new(HashMap::new());
    static UPEM_CACHE: std::cell::RefCell<HashMap<(u64, u32), u16>> =
        std::cell::RefCell::new(HashMap::new());
}

/// A run's face, read from the font only for glyphs not yet cached.
struct Face<'a> {
    key: FaceKey,
    data: &'a [u8],
    index: u32,
    coords: &'a [i16],
    units_per_em: u16,
}

impl<'a> Face<'a> {
    fn of(run: &'a parley::Run<'a, TextBrush>) -> Face<'a> {
        use std::hash::{Hash as _, Hasher as _};
        let font = run.font();
        let coords: &[i16] = bytemuck::cast_slice(run.normalized_coords());
        let mut h = std::collections::hash_map::DefaultHasher::new();
        coords.hash(&mut h);
        let id = (font.data.id(), font.index);
        let units_per_em = UPEM_CACHE.with(|c| {
            *c.borrow_mut().entry(id).or_insert_with(|| {
                use skrifa::raw::TableProvider as _;
                skrifa::FontRef::from_index(font.data.as_ref(), font.index)
                    .ok()
                    .and_then(|f| f.head().ok())
                    .map_or(0, |head| head.units_per_em())
            })
        });
        Face {
            key: (id.0, id.1, h.finish()),
            data: font.data.as_ref(),
            index: font.index,
            coords,
            units_per_em: units_per_em.max(1),
        }
    }

    fn ink(&self, glyph: u32) -> Ink {
        if self.units_per_em <= 1 {
            return Ink::Unknown;
        }
        let key = (self.key, glyph);
        if let Some(ink) = INK_CACHE.with(|c| c.borrow().get(&key).copied()) {
            return ink;
        }
        let ink = self.read(glyph);
        INK_CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() >= INK_CACHE_LIMIT {
                c.clear();
            }
            c.insert(key, ink);
        });
        ink
    }

    fn read(&self, glyph: u32) -> Ink {
        use skrifa::MetadataProvider as _;
        use skrifa::instance::{LocationRef, Size};
        use skrifa::outline::DrawSettings;
        let Ok(font) = skrifa::FontRef::from_index(self.data, self.index) else {
            return Ink::Unknown;
        };
        let Some(outline) = font.outline_glyphs().get(skrifa::GlyphId::new(glyph)) else {
            return Ink::Unknown;
        };
        let coords: Vec<skrifa::raw::types::F2Dot14> = self
            .coords
            .iter()
            .map(|&c| skrifa::raw::types::F2Dot14::from_bits(c))
            .collect();
        let mut pen = PathPen(kurbo::BezPath::new());
        let settings = DrawSettings::unhinted(Size::unscaled(), LocationRef::new(&coords));
        if outline.draw(settings, &mut pen).is_err() {
            return Ink::Unknown;
        }
        if pen.0.elements().is_empty() {
            return Ink::Empty;
        }
        // Exact: kurbo bounds curves by their extrema, not control points.
        let b = kurbo::Shape::bounding_box(&pen.0);
        if b.width() <= 0.0 || b.height() <= 0.0 {
            return Ink::Empty;
        }
        Ink::Box([b.x0 as f32, b.y0 as f32, b.x1 as f32, b.y1 as f32])
    }
}

struct PathPen(kurbo::BezPath);

impl skrifa::outline::OutlinePen for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to((f64::from(x), f64::from(y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to((f64::from(x), f64::from(y)));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.quad_to(
            (f64::from(cx0), f64::from(cy0)),
            (f64::from(x), f64::from(y)),
        );
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.curve_to(
            (f64::from(cx0), f64::from(cy0)),
            (f64::from(cx1), f64::from(cy1)),
            (f64::from(x), f64::from(y)),
        );
    }
    fn close(&mut self) {
        self.0.close_path();
    }
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
