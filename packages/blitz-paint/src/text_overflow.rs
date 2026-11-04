//! `text-overflow: ellipsis` on one line of inline content.
//!
//! This only changes what is painted. Layout, `scrollWidth`, the DOM text,
//! selection and hit testing all keep the whole line.
//!
//! An ellipsis applies to a block container with `overflow-x` other than
//! `visible` whose inline content is one line wider than its content box. The
//! line is cut at its end edge: the right for `direction: ltr`, the left for
//! `rtl`, where the line is also moved so that its start stays in view. Whole
//! grapheme clusters and whole ligatures are dropped until the ellipsis fits
//! beside what remains. A box too narrow for any text shows the ellipsis alone,
//! clipped like anything else.
//!
//! Anything else keeps the plain clip it had: several lines, inline boxes on
//! the line (they paint apart from the text, so they can't be cut with it), a
//! container scrolled by script, the two-value and `<string>` forms of the
//! property, and a font with neither `…` nor `.`. The ellipsis takes the
//! container's color and the font of the run at the line's start, which may be
//! a fallback font rather than the container's own first font. It isn't
//! synthesized bold or italic. Inline backgrounds under the cut stay as they
//! are, up to the clip.

use anyrender::PaintScene;
use blitz_dom::node::TextBrush;
use blitz_dom::util::ToColorColor as _;
use color::{AlphaColor, Srgb};
use kurbo::Affine;
use parley::{FontData, Layout, Line, PositionedLayoutItem};
use peniko::Fill;
use skrifa::instance::{LocationRef, Size};
use skrifa::raw::types::F2Dot14;
use skrifa::{FontRef, GlyphId, MetadataProvider as _};
use style::properties::ComputedValues;
use style::values::computed::Overflow;
use style::values::specified::text::TextOverflowSide;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::text::glyph_style;

/// Slack for float error: this line just fits.
const FITS: f32 = 0.01;

/// How one line is cut. All positions are in the layout's own units, before
/// `shift`.
pub(crate) struct Ellipsis {
    /// Moves the whole line, so that the end of an `rtl` line's overflow, not
    /// its start, is what goes past the box.
    pub shift: f32,
    /// The part of the line still painted, on whole cluster edges.
    pub keep: (f32, f32),
    font: FontData,
    font_size: f32,
    coords: Vec<i16>,
    color: AlphaColor<Srgb>,
    baseline: f32,
    /// Each glyph of the ellipsis and its position.
    glyphs: Vec<(u32, f32)>,
}

impl Ellipsis {
    /// `available` is the container's content box width, in layout units.
    pub(crate) fn plan(
        styles: &ComputedValues,
        layout: &Layout<TextBrush>,
        text: &str,
        available: f32,
        scrolled: bool,
    ) -> Option<Self> {
        if scrolled || !applies(styles) || layout.len() != 1 {
            return None;
        }
        let line = layout.get(0)?;
        let metrics = line.metrics();
        let width = metrics.advance - metrics.hanging_advance;
        if width <= available + FITS {
            return None;
        }
        let mut baseline = None;
        for item in line.items() {
            match item {
                PositionedLayoutItem::GlyphRun(run) => {
                    baseline.get_or_insert(run.baseline());
                }
                PositionedLayoutItem::InlineBox(_) => return None,
            }
        }
        let baseline = baseline?;
        let rtl = layout.is_rtl();

        let (first, last) = (metrics.offset, metrics.offset + width);
        let shift = if rtl { available - last } else { -first };
        // The line's start is at the far end of the runs when `rtl`.
        let run = if rtl {
            line.runs().last()
        } else {
            line.runs().next()
        }?;
        let font = run.font().clone();
        let font_size = run.font_size();
        let coords: Vec<i16> = bytemuck::cast_slice(run.normalized_coords()).to_vec();
        let (marks, mark_width) = marks(&font, font_size, &coords)?;

        let groups = groups(&line, text);
        let keep = if rtl {
            let (lo, hi) = keep_end(&groups, mark_width - shift).unwrap_or((last, last));
            (lo, hi)
        } else {
            keep_start(&groups, available - mark_width - shift).unwrap_or((first, first))
        };
        let start = if rtl { keep.0 - mark_width } else { keep.1 };
        Some(Ellipsis {
            shift,
            keep,
            font,
            font_size,
            coords,
            color: styles.get_inherited_text().color.as_color_color(),
            baseline,
            glyphs: marks.into_iter().map(|(g, x)| (g, start + x)).collect(),
        })
    }

    /// Paints the ellipsis, under a `transform` that already holds `shift`.
    pub(crate) fn draw(&self, scene: &mut impl PaintScene, transform: Affine, scale: f64) {
        let (hint, embolden) = glyph_style(self.font_size, scale);
        scene.draw_glyphs(
            &self.font,
            self.font_size,
            hint,
            &self.coords,
            embolden,
            Fill::NonZero,
            &anyrender::Paint::from(self.color),
            1.0,
            transform,
            None,
            self.glyphs.iter().map(|&(id, x)| anyrender::Glyph {
                id: id as _,
                x,
                y: self.baseline,
            }),
        );
    }
}

/// Whether `styles` ask for an ellipsis at the line's end and may clip.
fn applies(styles: &ComputedValues) -> bool {
    let wanted = styles.get_text().text_overflow.clone();
    wanted.sides_are_logical
        && matches!(wanted.second, TextOverflowSide::Ellipsis)
        && !matches!(styles.get_box().overflow_x, Overflow::Visible)
}

/// The ellipsis as glyphs from `font`, and its width: `…`, or else three dots.
fn marks(font: &FontData, size: f32, coords: &[i16]) -> Option<(Vec<(u32, f32)>, f32)> {
    let face = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    let map = face.charmap();
    let location: Vec<F2Dot14> = coords.iter().map(|c| F2Dot14::from_bits(*c)).collect();
    let metrics = face.glyph_metrics(Size::new(size), LocationRef::new(&location));
    let advance = |g: GlyphId| metrics.advance_width(g);
    if let Some(g) = map.map('\u{2026}').filter(|g| g.to_u32() != 0) {
        return Some((vec![(g.to_u32(), 0.0)], advance(g)?));
    }
    let dot = map.map('.').filter(|g| g.to_u32() != 0)?;
    let width = advance(dot)?;
    let glyphs = (0..3).map(|i| (dot.to_u32(), width * i as f32)).collect();
    Some((glyphs, width * 3.0))
}

/// The line's indivisible pieces as `(left, right)`, left to right: grapheme
/// clusters, and ligatures, which share one glyph.
fn groups(line: &Line<'_, TextBrush>, text: &str) -> Vec<(f32, f32)> {
    let boundaries: std::collections::HashSet<usize> =
        text.grapheme_indices(true).map(|(i, _)| i).collect();
    let mut groups: Vec<(f32, f32)> = Vec::new();
    let mut x = line.metrics().offset;
    // Runs come in visual order, and within a run clusters are walked
    // visually for their positions, then logically to join them.
    for run in line.runs() {
        let mut placed = Vec::new();
        for cluster in run.visual_clusters() {
            let right = x + cluster.advance();
            placed.push((cluster, x, right));
            x = right;
        }
        if run.is_rtl() {
            placed.reverse();
        }
        let from = groups.len();
        for (cluster, left, right) in placed {
            let joined = groups.len() > from
                && (cluster.is_ligature_continuation()
                    || !boundaries.contains(&cluster.text_range().start));
            if joined {
                let last = groups.last_mut().expect("just checked");
                last.0 = last.0.min(left);
                last.1 = last.1.max(right);
            } else {
                groups.push((left, right));
            }
        }
    }
    groups.sort_by(|a, b| a.0.total_cmp(&b.0));
    groups
}

/// What stays of `groups` from the left, with nothing past `limit`.
fn keep_start(groups: &[(f32, f32)], limit: f32) -> Option<(f32, f32)> {
    let kept = groups.iter().take_while(|g| g.1 <= limit + FITS);
    let last = kept.last()?;
    Some((groups[0].0, last.1))
}

/// What stays of `groups` from the right, with nothing before `limit`.
fn keep_end(groups: &[(f32, f32)], limit: f32) -> Option<(f32, f32)> {
    let kept = groups.iter().rev().take_while(|g| g.0 >= limit - FITS);
    let first = kept.last()?;
    Some((first.0, groups.last()?.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROUPS: [(f32, f32); 4] = [(0.0, 10.0), (10.0, 20.0), (20.0, 30.0), (30.0, 40.0)];

    #[test]
    fn the_start_stays_and_no_group_is_split() {
        assert_eq!(keep_start(&GROUPS, 25.0), Some((0.0, 20.0)));
        assert_eq!(keep_start(&GROUPS, 20.0), Some((0.0, 20.0)));
        assert_eq!(keep_start(&GROUPS, 9.0), None, "too narrow for any");
    }

    #[test]
    fn the_end_stays_when_the_line_runs_right_to_left() {
        assert_eq!(keep_end(&GROUPS, 15.0), Some((20.0, 40.0)));
        assert_eq!(keep_end(&GROUPS, 20.0), Some((20.0, 40.0)));
        assert_eq!(keep_end(&GROUPS, 31.0), None, "too narrow for any");
    }
}
