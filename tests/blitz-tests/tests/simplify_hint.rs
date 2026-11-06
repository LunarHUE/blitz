//! `data-blitz-simplify`: small text paints as ink boxes, without moving a
//! layout box or changing what a point hits, and without painting outside
//! what full paint covers. Nothing else changes.

use anyrender::recording::RenderCommand;
use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{BaseDocument, DocumentConfig};
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::{PaintStats, paint_scene_with_stats};
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const W: u32 = 240;
const H: u32 = 120;

fn doc(body: &str) -> HtmlDocument {
    let html = format!(
        r#"<html><head><style>
            body {{ margin: 0; background: #fff; font: 12px sans-serif; color: #000; }}
        </style></head><body>{body}</body></html>"#
    );
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(W, H, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

/// The same body with every `HINT` replaced: by the hint, and by nothing.
fn pair(body: &str, hint: &str) -> (HtmlDocument, HtmlDocument) {
    let hinted = body.replace("HINT", &format!(r#"data-blitz-simplify="{hint}""#));
    let plain = body.replace("HINT", "");
    (doc(&hinted), doc(&plain))
}

fn record(doc: &mut HtmlDocument) -> (Vec<RenderCommand>, PaintStats) {
    let mut scene = anyrender::Scene::new();
    let stats = paint_scene_with_stats(&mut scene, doc, 1.0, W, H, 0, 0);
    (scene.commands, stats)
}

fn glyph_runs(commands: &[RenderCommand]) -> usize {
    commands
        .iter()
        .filter(|c| matches!(c, RenderCommand::GlyphRun(_)))
        .count()
}

fn pixels(doc: &mut HtmlDocument) -> Vec<u8> {
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| {
            paint_scene_with_stats(scene, doc, 1.0, W, H, 0, 0);
        },
        W,
        H,
    )
}

/// The pixel box around everything not white, as (x0, y0, x1, y1)
/// inclusive.
fn ink(buffer: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let mut out: Option<(u32, u32, u32, u32)> = None;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if buffer[i..i + 3].iter().all(|&c| c == 255) {
                continue;
            }
            out = Some(match out {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    out
}

fn inside(inner: (u32, u32, u32, u32), outer: (u32, u32, u32, u32)) -> bool {
    inner.0 >= outer.0 && inner.1 >= outer.1 && inner.2 <= outer.2 && inner.3 <= outer.3
}

/// Every element's layout, in tree order.
fn layouts(doc: &BaseDocument) -> Vec<(blitz_dom::NodeId, taffy::Layout)> {
    let mut out = Vec::new();
    let mut stack = vec![doc.root_node().id];
    while let Some(id) = stack.pop() {
        let node = doc.get_node(id).unwrap();
        if node.is_element() {
            out.push((node.id, *node.final_layout()));
        }
        stack.extend(node.children.iter().rev().copied());
    }
    out
}

const TEXT: &str = r#"<div HINT style="padding: 10px; width: 200px">
    Small words in a hinted box, wrapped onto a second line of text here.
</div>"#;

#[test]
fn small_text_paints_ink_boxes_inside_the_glyphs() {
    let (mut hinted, mut plain) = pair(TEXT, "text=100");
    let (commands, stats) = record(&mut hinted);
    assert_eq!(glyph_runs(&commands), 0);
    assert_eq!(stats.text_boxes, 1);
    assert_eq!(stats.hinted, 1);
    let (commands, stats) = record(&mut plain);
    assert!(glyph_runs(&commands) > 0);
    assert_eq!(stats, PaintStats::default());

    let simplified = ink(&pixels(&mut hinted)).expect("ink boxes paint");
    let full = ink(&pixels(&mut plain)).expect("glyphs paint");
    assert!(
        inside(simplified, full),
        "simplified {simplified:?} outside full {full:?}"
    );
}

#[test]
fn the_hint_moves_no_box_and_changes_no_hit() {
    let (hinted, plain) = pair(
        r#"<div HINT style="padding: 10px; width: 200px; position: relative">
            <span>Some text</span>
            <div id="tiny" style="width: 3px; height: 3px; background: red">
                <div style="width: 1px; height: 1px; background: green"></div>
            </div>
            <div style="position: absolute; left: 150px; top: 5px; width: 20px; height: 20px;
                        background: blue; z-index: 2"></div>
        </div>"#,
        "text=100 frame=4",
    );
    let mut a = layouts(&hinted);
    let b = layouts(&plain);
    // The hinted div is the only element whose attributes differ.
    assert_eq!(a.len(), b.len());
    for ((id_a, la), (id_b, lb)) in a.drain(..).zip(b) {
        assert_eq!(id_a, id_b);
        assert_eq!(la, lb, "node {id_a:?} moved");
    }
    for (x, y) in [
        (12.0, 15.0),
        (11.0, 28.0),
        (10.5, 29.5),
        (160.0, 12.0),
        (230.0, 100.0),
    ] {
        let ha = hinted.hit(x, y).map(|h| h.node_id);
        let hb = plain.hit(x, y).map(|h| h.node_id);
        assert_eq!(ha, hb, "hit at ({x}, {y})");
    }
}

#[test]
fn text_at_or_above_the_threshold_keeps_its_glyphs() {
    let (mut hinted, _) = pair(TEXT, "text=4");
    let (commands, stats) = record(&mut hinted);
    assert!(glyph_runs(&commands) > 0);
    assert_eq!(stats.text_boxes, 0);
}

#[test]
fn a_scaled_down_ancestor_makes_text_project_small() {
    let body = r#"<div style="transform: scale(0.25); transform-origin: 0 0">
        <div HINT style="width: 400px">Text drawn at a quarter of its size.</div>
    </div>"#;
    let (mut hinted, mut plain) = pair(body, "text=4");
    let (commands, stats) = record(&mut hinted);
    assert_eq!(glyph_runs(&commands), 0);
    assert_eq!(stats.text_boxes, 1);
    let simplified = ink(&pixels(&mut hinted)).unwrap();
    let full = ink(&pixels(&mut plain)).unwrap();
    assert!(inside(simplified, full), "{simplified:?} outside {full:?}");
}

#[test]
fn text_overflow_keeps_its_glyphs() {
    let (mut hinted, _) = pair(
        r#"<div HINT style="width: 60px; overflow: hidden; white-space: nowrap;
                             text-overflow: ellipsis">A line too long to fit</div>"#,
        "text=100",
    );
    let (commands, stats) = record(&mut hinted);
    assert!(glyph_runs(&commands) > 0);
    assert_eq!(stats.text_boxes, 0);
}

#[test]
fn small_nested_frames_keep_their_content() {
    // A frame key from an older hint substitutes nothing.
    let (mut hinted, mut plain) = pair(
        r#"<div HINT style="width: 100px; height: 50px">
            <div style="width: 3px; height: 3px; background: #ff0000">
                <div style="width: 1px; height: 1px; background: #00ff00"></div>
            </div>
        </div>"#,
        "frame=100",
    );
    let (_, stats) = record(&mut hinted);
    assert_eq!(stats.hinted, 1);
    assert!(
        pixels(&mut hinted) == pixels(&mut plain),
        "the hint changed paint"
    );
}

#[test]
fn the_hint_ends_with_its_element() {
    let (mut hinted, _) = pair(
        r#"<div HINT style="width: 200px">Hinted text</div>
           <div style="width: 200px">Sibling text</div>"#,
        "text=100",
    );
    let (commands, stats) = record(&mut hinted);
    assert_eq!(stats.text_boxes, 1);
    assert_eq!(glyph_runs(&commands), 1, "the sibling keeps its glyphs");
}

#[test]
fn synthetic_bold_boxes_stay_inside_the_glyphs() {
    // A static face with no bold, so weight 700 is drawn emboldened.
    let (mut hinted, mut plain) = pair(
        r#"<div HINT style="width: 220px; font: 700 14px 'DejaVu Math TeX Gyre'">
            Bold words drawn by emboldening
        </div>"#,
        "text=100",
    );
    let (commands, _) = record(&mut plain);
    assert!(
        commands.iter().any(|c| matches!(c,
            RenderCommand::GlyphRun(g) if g.embolden.x > 0.0)),
        "the run is synthetic bold"
    );
    let (_, stats) = record(&mut hinted);
    assert_eq!(stats.text_boxes, 1);
    let simplified = ink(&pixels(&mut hinted)).unwrap();
    let full = ink(&pixels(&mut plain)).unwrap();
    assert!(inside(simplified, full), "{simplified:?} outside {full:?}");
}

/// Text 12px tall under `transform`, hinted with a 20 device pixel
/// threshold.
fn stretched(transform: &str) -> (HtmlDocument, HtmlDocument) {
    pair(
        &format!(
            r#"<div style="transform: {transform}; transform-origin: 0 0; width: 200px">
                <div HINT>Stretched words</div>
            </div>"#
        ),
        "text=20",
    )
}

#[test]
fn text_stretched_along_one_axis_counts_its_longest_extent() {
    // The area scale is 1, but glyphs span 120 device pixels across.
    let (mut hinted, _) = stretched("scale(10, 0.1)");
    let (commands, stats) = record(&mut hinted);
    assert_eq!(stats.text_boxes, 0);
    assert!(glyph_runs(&commands) > 0);
}

#[test]
fn skewed_text_counts_its_longest_extent() {
    // A skew keeps area but stretches one diagonal past 5x.
    let (mut hinted, _) = stretched("skewX(80deg)");
    let (_, stats) = record(&mut hinted);
    assert_eq!(stats.text_boxes, 0);
}

#[test]
fn text_small_along_both_axes_still_simplifies() {
    let (mut hinted, mut plain) = stretched("scale(0.5, 0.25)");
    let (_, stats) = record(&mut hinted);
    assert_eq!(stats.text_boxes, 1);
    let simplified = ink(&pixels(&mut hinted)).unwrap();
    let full = ink(&pixels(&mut plain)).unwrap();
    assert!(inside(simplified, full), "{simplified:?} outside {full:?}");
}

#[test]
fn small_frames_keep_descendant_shadows_and_outlines() {
    // Each frame is 3px with a 1px child, so layout overflow stays inside
    // it, but the child's shadow and outline paint far outside.
    let (mut hinted, mut plain) = pair(
        r#"<div HINT style="width: 200px; height: 100px; padding: 30px">
            <div style="width: 3px; height: 3px; background: #0000ff">
                <div style="width: 1px; height: 1px; box-shadow: 0 0 0 20px #ff0000"></div>
            </div>
            <div style="width: 3px; height: 3px; margin-top: 40px; background: #0000ff">
                <div style="width: 1px; height: 1px; outline: 10px solid #00ff00"></div>
            </div>
        </div>"#,
        "text=100 frame=4",
    );
    assert!(
        pixels(&mut hinted) == pixels(&mut plain),
        "the hint changed paint"
    );
}

fn simplify_attr() -> blitz_dom::QualName {
    blitz_dom::QualName::new(None, blitz_dom::ns!(), blitz_paint::SIMPLIFY_ATTR.into())
}

#[test]
fn changing_the_hint_repaints_without_restyle_or_layout() {
    let mut doc = doc(r#"<div id="t" style="width: 200px">Small words</div>"#);
    let id = doc.get_element_by_id("t").unwrap();
    let root = doc.root_node().id;
    let clean = |doc: &HtmlDocument| !doc.get_node(root).unwrap().has_dirty_descendants();
    assert!(clean(&doc));

    doc.mutate().set_attribute(id, simplify_attr(), "text=100");
    assert!(clean(&doc), "setting the hint asked for a restyle");
    assert_eq!(record(&mut doc).1.text_boxes, 1, "paint reads the new hint");

    doc.mutate().set_attribute(id, simplify_attr(), "text=1");
    assert!(clean(&doc));
    assert_eq!(record(&mut doc).1.text_boxes, 0);

    doc.mutate().clear_attribute(id, simplify_attr());
    assert!(clean(&doc), "clearing the hint asked for a restyle");
    assert_eq!(record(&mut doc).1.hinted, 0);
}

#[test]
fn text_crossing_its_clip_keeps_its_glyphs() {
    // A box over a run covers the gaps between its glyphs, so cut by a clip
    // it can paint where the cut glyphs don't.
    let (mut hinted, mut plain) = pair(
        r#"<div HINT style="width: 40px; height: 10px; overflow: hidden">
            <div style="width: 200px">Words much wider than the clip around them</div>
        </div>"#,
        "text=100",
    );
    let (_, stats) = record(&mut hinted);
    assert_eq!(stats.text_boxes, 0);
    assert!(
        pixels(&mut hinted) == pixels(&mut plain),
        "the hint changed paint"
    );
}

#[test]
fn text_inside_its_clip_simplifies_inside_it() {
    let (mut hinted, mut plain) = pair(
        r#"<div HINT style="width: 120px; height: 30px; overflow: hidden">Short words</div>"#,
        "text=100",
    );
    let (_, stats) = record(&mut hinted);
    assert_eq!(stats.text_boxes, 1);
    let simplified = ink(&pixels(&mut hinted)).unwrap();
    assert!(
        inside(simplified, (0, 0, 119, 29)),
        "{simplified:?} outside the clip"
    );
    let full = ink(&pixels(&mut plain)).unwrap();
    assert!(inside(simplified, full), "{simplified:?} outside {full:?}");
}

#[test]
fn ink_boxes_keep_their_place_in_the_stacking_order() {
    // An opaque box stacked above the text, and one below it, both in the
    // hinted element's own stacking context.
    let (mut hinted, mut plain) = pair(
        r#"<div HINT style="position: relative; z-index: 0; width: 200px; height: 40px">
            <div style="position: absolute; left: 0; top: 0; width: 200px; height: 40px;
                        background: #00ff00; z-index: -1"></div>
            <div>Words partly covered by a box above them</div>
            <div style="position: absolute; left: 50px; top: 0; width: 60px; height: 40px;
                        background: #0000ff; z-index: 1"></div>
        </div>"#,
        "text=100",
    );
    let (_, stats) = record(&mut hinted);
    assert_eq!(stats.text_boxes, 1);
    let (a, b) = (pixels(&mut hinted), pixels(&mut plain));
    let at = |buf: &[u8], x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2]]
    };
    for y in 0..40 {
        for x in 50..110 {
            assert_eq!(
                at(&a, x, y),
                [0, 0, 255],
                "the box above is covered at ({x}, {y})"
            );
        }
    }
    // Where the text isn't, both show the box below.
    assert_eq!(at(&a, 199, 39), at(&b, 199, 39));
    assert_eq!(at(&a, 199, 39), [0, 255, 0]);
}

#[test]
fn rotated_text_keeps_its_glyphs() {
    // A box rotated with its run reaches past the rotated glyphs' extrema,
    // as a round letter's corners would.
    let (mut hinted, _) = stretched("translate(80px, 10px) rotate(45deg) scale(0.5)");
    let (commands, stats) = record(&mut hinted);
    assert_eq!(stats.text_boxes, 0);
    assert!(glyph_runs(&commands) > 0);
}

/// Whether the hint substitutes nothing in `body` and paints the same
/// pixels as without it.
fn keeps_full(body: &str) {
    let (mut hinted, mut plain) = pair(body, "text=100");
    let (_, stats) = record(&mut hinted);
    assert_eq!(stats.text_boxes, 0, "{body}");
    assert!(
        pixels(&mut hinted) == pixels(&mut plain),
        "the hint changed paint"
    );
}

#[test]
fn text_under_a_rounded_clip_keeps_its_glyphs() {
    // Inside the clip's rectangle, but a box could fill a cut corner.
    keeps_full(
        r#"<div HINT style="width: 120px; height: 30px; overflow: hidden;
                             border-radius: 15px">Short words</div>"#,
    );
}

#[test]
fn text_under_a_clip_path_keeps_its_glyphs() {
    keeps_full(
        r#"<div HINT style="width: 120px; height: 30px;
                             clip-path: circle(20px at 30px 10px)">Short words</div>"#,
    );
}

#[test]
fn text_under_a_css_clip_keeps_its_glyphs() {
    keeps_full(
        r#"<div HINT style="position: absolute; left: 0; top: 0; width: 120px;
                             height: 30px; clip: rect(0, 40px, 30px, 0)">Short words</div>"#,
    );
}

#[test]
fn text_under_a_rotated_clip_keeps_its_glyphs() {
    // The clip turns with its element; the text is turned back, so only the
    // clip's edges are slanted.
    keeps_full(
        r#"<div HINT style="position: absolute; left: 60px; top: 30px; width: 100px;
                             height: 40px; overflow: hidden; transform: rotate(20deg)">
            <div style="transform: rotate(-20deg)">Short words</div>
        </div>"#,
    );
}

#[test]
fn a_clip_edge_between_two_words_keeps_their_glyphs() {
    // The clip's right edge falls in the space between the words, which
    // their run's box covers and their glyphs don't.
    keeps_full(
        r#"<div HINT style="width: 18px; height: 20px; overflow: hidden;
                             white-space: nowrap; font: 12px monospace">ab cd</div>"#,
    );
}
