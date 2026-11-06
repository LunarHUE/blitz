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
use crate::node::{ListItemLayout, ListItemLayoutPosition};
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
/// marker, in the node's device-pixel coordinates, from the shaped lines at
/// the positions paint draws them. Each glyph run is grown by its own font
/// size, by the decorations of every decorating box between it and the
/// root, and by its text shadows. A run whose styles can't be read makes
/// the ink unbounded. `None` when the node draws neither.
pub(crate) fn text_ink(doc: &BaseDocument, node_id: NodeId, scale: f64) -> Option<Rect> {
    let node = &doc.nodes[node_id];
    let element = node.element_data()?;
    let layout = node.final_layout();
    let pb_left = f64::from(layout.padding.left + layout.border.left);
    let pb_top = f64::from(layout.padding.top + layout.border.top);
    let mut ink: Option<Rect> = None;
    let mut add = |r: Rect| ink = Some(ink.map_or(r, |i| i.union(r)));

    if let Some(text) = element.inline_layout_data.as_ref() {
        // Paint draws the layout from the content box, moved down by the
        // block offset; the layout itself is in device pixels.
        let origin = Vec2::new(pb_left, pb_top + f64::from(text.block_offset)) * scale;
        for line in text.layout.lines() {
            let line_advance = f64::from(line.metrics().advance) / scale;
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let em = f64::from(run.run().font_size()) / scale;
                let Some(reach) = run_reach(doc, run.style().brush.id, node_id, em, line_advance)
                else {
                    return Some(UNBOUNDED);
                };
                let x0 = f64::from(run.offset());
                let x1 = x0 + f64::from(run.advance());
                let baseline = f64::from(run.baseline());
                add(Rect::new(
                    x0 - reach.side * scale,
                    baseline - reach.up * scale,
                    x1 + reach.side * scale,
                    baseline + reach.down * scale,
                ) + origin);
            }
        }
    }

    if let Some(ListItemLayout {
        position: ListItemLayoutPosition::Outside(marker),
        ..
    }) = element.list_item_data.as_deref()
    {
        // Paint right-aligns the marker up to 8px left of the border box
        // and aligns its baseline with the first line's. Its box, grown by
        // its largest font size on every side, covers both.
        let em = marker
            .lines()
            .flat_map(|l| l.items())
            .filter_map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => Some(f64::from(run.run().font_size())),
                _ => None,
            })
            .fold(0.0, f64::max);
        let width = f64::from(marker.full_width());
        let height = f64::from(marker.height());
        let text_height = element.inline_layout_data.as_ref().map_or(0.0, |t| {
            f64::from(t.layout.height() + t.block_offset * scale as f32)
        });
        let reach = 2.0 * em;
        add(Rect::new(
            -(width + 8.0 * scale) - reach,
            pb_top * scale - height - reach,
            reach,
            pb_top * scale + text_height + height + reach,
        ));
    }

    ink
}

/// How far a glyph run's ink can reach past its advance and baseline, in
/// CSS pixels.
struct Reach {
    side: f64,
    up: f64,
    down: f64,
}

/// The reach of a run of font size `em` whose innermost node is `id`, inside
/// the inline root `root`. `None` when a node on the way has no styles.
fn run_reach(
    doc: &BaseDocument,
    id: NodeId,
    root: NodeId,
    em: f64,
    line_advance: f64,
) -> Option<Reach> {
    // Glyphs can reach well past their ascent, descent and advance:
    // italics, stacked marks, tall scripts. Two ems up and down and one
    // to the side covers them.
    let mut reach = Reach {
        side: em,
        up: 2.0 * em,
        down: 2.0 * em,
    };
    let style = doc.get_node(id)?.primary_styles()?;
    for shadow in style.get_inherited_text().text_shadow.0.iter() {
        let blur = BLUR_REACH * f64::from(shadow.blur.px());
        reach.side += f64::from(shadow.horizontal.px()).abs() + blur;
        let dy = f64::from(shadow.vertical.px()).abs() + blur;
        reach.up += dy;
        reach.down += dy;
    }
    drop(style);

    // Decorations come from every decorating box from the run's node up to
    // the inline root. Double and wavy lines take up to four thicknesses.
    let lines = TextDecorationLine::UNDERLINE
        | TextDecorationLine::OVERLINE
        | TextDecorationLine::LINE_THROUGH;
    let em_length = Length::new(em as f32);
    let mut current = Some(id);
    while let Some(node_id) = current {
        let node = doc.get_node(node_id)?;
        let style = node.primary_styles()?;
        let text = style.get_text();
        if text.text_decoration_line.intersects(lines) {
            let thickness = match &text.text_decoration_thickness {
                GenericTextDecorationLength::LengthPercentage(lp) => {
                    f64::from(lp.resolve(em_length).px()).abs()
                }
                // Font metrics and the auto thickness stay within an em.
                _ => em,
            };
            let offset = style
                .get_inherited_text()
                .text_underline_offset
                .non_auto()
                .map_or(0.0, |lp| f64::from(lp.resolve(em_length).px()).abs());
            let vertical = offset + 4.0 * thickness;
            reach.up += vertical;
            reach.down += vertical;
            if let GenericTextDecorationInset::LengthPercentage { start, end } =
                &text.text_decoration_inset
            {
                let line = Length::new(line_advance as f32);
                reach.side += f64::from(start.resolve(line).px()).abs()
                    + f64::from(end.resolve(line).px()).abs();
            }
        }
        if node_id == root {
            break;
        }
        current = node.parent;
    }
    Some(reach)
}
