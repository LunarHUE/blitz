//! Ink overflow: a conservative bound on everything a node and its
//! descendants may paint, used by paint culling.
//!
//! Scrollable overflow covers boxes only. Outset box shadows, outlines,
//! filters, text shadows and glyphs that overhang their line boxes paint
//! outside it, so culling against scrollable overflow alone drops paint that
//! would be visible. The bounds here over-approximate each effect; an effect
//! whose extent can't be bounded (an SVG `url()` filter) makes the node's
//! ink unbounded, so it is never culled.

use kurbo::{Rect, Vec2};
use parley::PositionedLayoutItem;
use style::properties::ComputedValues;
use style::values::computed::outline::OutlineStyle;
use style::values::computed::{Filter, Length, TextDecorationLine};
use style::values::generics::text::{GenericTextDecorationInset, GenericTextDecorationLength};
use style::values::specified::border::BorderStyle;

use crate::BaseDocument;
use crate::glyph_ink::{Face, GlyphInk};
use crate::node::{ListItemLayout, ListItemLayoutPosition, Marker, TextBrush};
use blitz_traits::node_id::NodeId;

/// Ink that can't be bounded. Large enough to contain any viewport, small
/// enough to stay finite through the transforms paint applies.
pub(crate) const UNBOUNDED: Rect = Rect::new(-1.0e15, -1.0e15, 1.0e15, 1.0e15);

/// How far, in multiples of the blur radius, a blurred shadow or filter
/// is taken to reach. Paint sizes shadow layers with 2.5; 3 leaves margin.
const BLUR_REACH: f64 = 3.0;

/// The node's own box ink around its border box `border_box`, in the
/// node's device-pixel coordinates: outset box shadows and the outline.
/// [`text_ink`] adds its text.
pub(crate) fn own_ink(style: &ComputedValues, border_box: Rect, scale: f64) -> Rect {
    let mut ink = border_box;

    for shadow in style.get_effects().box_shadow.0.iter().filter(|s| !s.inset) {
        let reach = (shadow.spread.px() as f64 + BLUR_REACH * shadow.base.blur.px() as f64)
            .max(0.0)
            * scale;
        let offset = Vec2::new(
            shadow.base.horizontal.px() as f64,
            shadow.base.vertical.px() as f64,
        ) * scale;
        ink = ink.union(border_box.inflate(reach, reach) + offset);
    }

    let outline = style.get_outline();
    let drawn = match outline.outline_style {
        OutlineStyle::Auto => true,
        OutlineStyle::BorderStyle(s) => !matches!(s, BorderStyle::None | BorderStyle::Hidden),
    };
    if drawn {
        let width = outline.outline_width.0.to_f64_px();
        let offset = outline.outline_offset.to_f64_px().max(0.0);
        let reach = (width + offset) * scale;
        ink = ink.union(border_box.inflate(reach, reach));
    }

    ink
}

/// Grows `ink`, the node's own ink and its descendants', by the node's
/// filters, which apply to all of it.
pub(crate) fn filtered(style: &ComputedValues, mut ink: Rect, scale: f64) -> Rect {
    for filter in style.get_effects().filter.0.iter() {
        match filter {
            Filter::Blur(radius) => {
                let reach = BLUR_REACH * radius.px() as f64 * scale;
                ink = ink.inflate(reach, reach);
            }
            Filter::DropShadow(shadow) => {
                let reach = BLUR_REACH * shadow.blur.px() as f64 * scale;
                let offset =
                    Vec2::new(shadow.horizontal.px() as f64, shadow.vertical.px() as f64) * scale;
                ink = ink.union(ink.inflate(reach, reach) + offset);
            }
            Filter::Url(_) => return UNBOUNDED,
            _ => {}
        }
    }
    ink
}

/// Ink of the text an inline root lays out and of a list item's outside
/// marker, in the node's device-pixel coordinates, at the positions paint
/// draws them: each glyph's outline bounds at its shaped position, grown
/// for synthetic bold, faux italic and hinting, plus the decorations of
/// every decorating box between the run and the root, and text shadows. A
/// glyph without a readable outline, or a run whose styles can't be read,
/// makes the ink unbounded. `None` when the node draws neither.
pub(crate) fn text_ink(doc: &BaseDocument, node_id: NodeId, scale: f64) -> Option<Rect> {
    let node = &doc.nodes[node_id];
    let element = node.element_data()?;
    let layout = node.final_layout();
    let pb_left = f64::from(layout.padding.left + layout.border.left);
    let pb_top = f64::from(layout.padding.top + layout.border.top);
    let mut ink: Option<Rect> = None;

    if let Some(text) = element.inline_layout_data.as_ref() {
        // Paint draws the layout from the content box, moved down by the
        // block offset; the layout itself is in device pixels.
        let origin = Vec2::new(pb_left, pb_top + f64::from(text.block_offset)) * scale;
        ink = union(
            ink,
            lines_ink(doc, &text.layout, node_id, scale).map(|r| r + origin),
        );
    }

    if let Some(ListItemLayout {
        marker,
        position: ListItemLayoutPosition::Outside(layout),
    }) = element.list_item_data.as_deref()
    {
        // Where paint puts the marker: right-aligned left of the border
        // box, 8px further for a bullet, with its first baseline on the
        // item's first baseline.
        let padding = match marker {
            Marker::Char(_) => 8.0,
            Marker::String(_) => 0.0,
        };
        let marker_scale = f64::from(layout.scale());
        let x = -(f64::from(layout.full_width()) / marker_scale + padding);
        let first = |l: &parley::Layout<TextBrush>| l.lines().next().map(|l| l.metrics().baseline);
        let y = match (element.inline_layout_data.as_ref(), first(layout)) {
            (Some(text), Some(marker_baseline)) => match first(&text.layout) {
                Some(text_baseline) => {
                    f64::from(text_baseline - marker_baseline) / marker_scale
                        + f64::from(text.block_offset)
                }
                None => 0.0,
            },
            _ => 0.0,
        };
        let origin = Vec2::new(x, pb_top + y) * scale;
        ink = union(
            ink,
            lines_ink(doc, layout, node_id, scale).map(|r| r + origin),
        );
    }

    ink
}

fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, b) => a.or(b),
    }
}

/// The ink of every glyph run in `layout`, in its own coordinates, or
/// [`UNBOUNDED`] when a run's is unknown. `None` when it draws nothing.
fn lines_ink(
    doc: &BaseDocument,
    layout: &parley::Layout<TextBrush>,
    root: NodeId,
    scale: f64,
) -> Option<Rect> {
    let mut ink = None;
    for line in layout.lines() {
        let advance = f64::from(line.metrics().advance);
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(run) = item else {
                continue;
            };
            match run_ink(doc, &run, root, advance, scale) {
                Some(r) => ink = union(ink, r),
                None => return Some(UNBOUNDED),
            }
        }
    }
    ink
}

/// One glyph run's ink in layout coordinates. `None` when unknown.
fn run_ink(
    doc: &BaseDocument,
    glyph_run: &parley::GlyphRun<'_, TextBrush>,
    root: NodeId,
    line_advance: f64,
    scale: f64,
) -> Option<Option<Rect>> {
    let run = glyph_run.run();
    let font_size = f64::from(run.font_size());
    let face = Face::of(run);
    let units = f64::from(face.units_per_em());
    if units == 0.0 {
        return None;
    }
    let s = font_size / units;
    let baseline = f64::from(glyph_run.baseline());

    let mut glyphs: Option<Rect> = None;
    for g in glyph_run.positioned_glyphs() {
        match face.ink(g.id) {
            GlyphInk::Unknown => return None,
            GlyphInk::Empty => {}
            GlyphInk::Box([x0, y0, x1, y1]) => {
                let (x, y) = (f64::from(g.x), f64::from(g.y));
                let r = Rect::new(
                    x + f64::from(x0) * s,
                    y - f64::from(y1) * s,
                    x + f64::from(x1) * s,
                    y - f64::from(y0) * s,
                );
                glyphs = union(glyphs, Some(r));
            }
        }
    }
    let mut ink = glyphs.map(|mut r| {
        // Faux italic shears about the baseline.
        if let Some(angle) = run.synthesis().skew() {
            let lean = f64::from(angle).to_radians().tan().abs()
                * (baseline - r.y0).abs().max((r.y1 - baseline).abs());
            r = r.inflate(lean, 0.0);
        }
        // Synthetic bold grows outlines by ppem / 48 a side, the platform
        // embolden by under 0.3px, and hinting moves edges by under a
        // device pixel.
        let grow = font_size / 48.0 + 0.3 * scale + 1.0;
        r.inflate(grow, grow)
    });

    // Decorations span the run's advance, moved by any negative inset, at
    // the font's underline, strike-through or ascent, moved by any
    // underline offset. Double and wavy lines reach two thicknesses and a
    // pixel from where they're placed.
    let metrics = run.font_metrics();
    let em = font_size / scale;
    let em_length = Length::new(em as f32);
    let lines = TextDecorationLine::UNDERLINE
        | TextDecorationLine::OVERLINE
        | TextDecorationLine::LINE_THROUGH;
    let x0 = f64::from(glyph_run.offset());
    let x1 = x0 + f64::from(glyph_run.advance());
    let mut current = Some(glyph_run.style().brush.id);
    while let Some(node_id) = current {
        let node = doc.get_node(node_id)?;
        let style = node.primary_styles()?;
        let text = style.get_text();
        if text.text_decoration_line.intersects(lines) {
            let thickness = match &text.text_decoration_thickness {
                GenericTextDecorationLength::LengthPercentage(lp) => {
                    f64::from(lp.resolve(em_length).px()).abs() * scale
                }
                GenericTextDecorationLength::FromFont => f64::from(metrics.underline_size),
                GenericTextDecorationLength::Auto => (em / 10.0).max(1.0) * scale,
            };
            let offset = style
                .get_inherited_text()
                .text_underline_offset
                .non_auto()
                .map_or(0.0, |lp| {
                    f64::from(lp.resolve(em_length).px()).abs() * scale
                });
            let font_offset = f64::from(metrics.underline_offset)
                .abs()
                .max(f64::from(metrics.strikethrough_offset).abs());
            let reach = 2.0 * thickness + scale + offset + font_offset;
            let inset = match &text.text_decoration_inset {
                GenericTextDecorationInset::LengthPercentage { start, end } => {
                    let line = Length::new((line_advance / scale) as f32);
                    (f64::from(start.resolve(line).px()).abs()
                        + f64::from(end.resolve(line).px()).abs())
                        * scale
                }
                GenericTextDecorationInset::Auto => 0.0,
            };
            let deco = Rect::new(
                x0 - inset,
                baseline - f64::from(metrics.ascent) - reach,
                x1 + inset,
                baseline + f64::from(metrics.descent) + reach,
            );
            ink = union(ink, Some(deco));
        }
        if node_id == root {
            break;
        }
        current = node.parent;
    }

    // Text shadows repeat the glyphs and decorations, moved and blurred.
    let style = doc.get_node(glyph_run.style().brush.id)?.primary_styles()?;
    if let Some(drawn) = ink {
        for shadow in style.get_inherited_text().text_shadow.0.iter() {
            let blur = BLUR_REACH * f64::from(shadow.blur.px()) * scale;
            let offset = Vec2::new(
                f64::from(shadow.horizontal.px()),
                f64::from(shadow.vertical.px()),
            ) * scale;
            ink = union(ink, Some(drawn.inflate(blur, blur) + offset));
        }
    }
    Some(ink)
}
