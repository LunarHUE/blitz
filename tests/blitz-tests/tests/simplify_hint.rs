//! `data-blitz-simplify`: small text paints as ink boxes and small nested
//! frames paint only their own box, without moving a layout box or changing
//! what a point hits, and without painting outside what full paint covers.

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

fn fills_of(commands: &[RenderCommand], rgb: [u8; 3]) -> usize {
    commands
        .iter()
        .filter(|c| match c {
            RenderCommand::Fill(f) => match &f.brush {
                anyrender::Paint::Solid(color) => {
                    let c = color.to_rgba8();
                    [c.r, c.g, c.b] == rgb
                }
                _ => false,
            },
            _ => false,
        })
        .count()
}

const NESTED: &str = r#"<div HINT style="width: 100px; height: 50px">
    <div style="width: 3px; height: 3px; background: #ff0000">
        <div style="width: 1px; height: 1px; background: #00ff00"></div>
    </div>
    <div style="width: 3px; height: 3px; background: #0000ff">
        <div style="width: 10px; height: 1px; background: #ffff00"></div>
    </div>
</div>"#;

#[test]
fn a_small_nested_frame_paints_only_its_own_box() {
    let (mut hinted, mut plain) = pair(NESTED, "frame=4");
    let (commands, stats) = record(&mut hinted);
    assert_eq!(fills_of(&commands, [255, 0, 0]), 1, "its own background");
    assert_eq!(fills_of(&commands, [0, 255, 0]), 0, "not its child");
    // A child laid out past the frame's box keeps the frame whole.
    assert_eq!(fills_of(&commands, [0, 0, 255]), 1);
    assert_eq!(fills_of(&commands, [255, 255, 0]), 1);
    assert_eq!(stats.flattened, 1);

    let simplified = ink(&pixels(&mut hinted)).unwrap();
    let full = ink(&pixels(&mut plain)).unwrap();
    assert!(inside(simplified, full), "{simplified:?} outside {full:?}");
}

#[test]
fn frames_at_or_above_the_threshold_keep_their_content() {
    let (mut hinted, _) = pair(NESTED, "frame=3");
    let (commands, stats) = record(&mut hinted);
    assert_eq!(fills_of(&commands, [0, 255, 0]), 1);
    assert_eq!(stats.flattened, 0);
}

#[test]
fn the_hinted_element_itself_is_never_flattened() {
    let (mut hinted, _) = pair(
        r#"<div HINT style="width: 3px; height: 3px; background: #ff0000">
            <div style="width: 1px; height: 1px; background: #00ff00"></div>
        </div>"#,
        "frame=100",
    );
    let (commands, stats) = record(&mut hinted);
    assert_eq!(fills_of(&commands, [0, 255, 0]), 1);
    assert_eq!(stats.flattened, 0);
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
