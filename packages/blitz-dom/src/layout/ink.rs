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
use style::properties::ComputedValues;
use style::values::computed::Filter;
use style::values::computed::outline::OutlineStyle;
use style::values::specified::border::BorderStyle;

/// Ink that can't be bounded. Large enough to contain any viewport, small
/// enough to stay finite through the transforms paint applies.
pub(crate) const UNBOUNDED: Rect = Rect::new(-1.0e15, -1.0e15, 1.0e15, 1.0e15);

/// How far, in multiples of the blur radius, a blurred shadow or filter
/// is taken to reach. Paint sizes shadow layers with 2.5; 3 leaves margin.
const BLUR_REACH: f64 = 3.0;

/// The node's own ink around its border box `border_box`, in the node's
/// device-pixel coordinates: outset box shadows, the outline, text shadows
/// and glyph overhang. `has_text` is whether the node lays out inline text.
pub(crate) fn own_ink(
    style: &ComputedValues,
    border_box: Rect,
    scale: f64,
    has_text: bool,
) -> Rect {
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

    if has_text {
        // Glyphs can reach past their line boxes: italics, tall
        // ascenders under a small line-height, combining marks. One em
        // on every side covers them.
        let em = style.get_font().font_size.computed_size().px() as f64 * scale;
        ink = ink.union(border_box.inflate(em, em));
        for shadow in style.get_inherited_text().text_shadow.0.iter() {
            let reach = (em + BLUR_REACH * shadow.blur.px() as f64 * scale).max(0.0);
            let offset =
                Vec2::new(shadow.horizontal.px() as f64, shadow.vertical.px() as f64) * scale;
            ink = ink.union(border_box.inflate(reach, reach) + offset);
        }
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
