//! Paint culls an element only when nothing it paints can reach the
//! viewport. Each element here has its box just outside a 100x100 viewport,
//! past its right edge, and paints red across that edge with a shadow, an
//! outline or a transform. The pixel at (90, 40) must be red. Filters,
//! which the CPU renderer doesn't draw yet, are checked through the bound.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const SIZE: u32 = 100;

fn document(body: &str) -> HtmlDocument {
    let html = format!("<html><body style='margin:0; background:#fff'>{body}</body></html>");
    let mut doc = HtmlDocument::from_html(
        &html,
        DocumentConfig {
            viewport: Some(Viewport::new(SIZE, SIZE, 1.0, ColorScheme::Light)),
            html_parser_provider: Some(Arc::new(HtmlProvider) as _),
            ..Default::default()
        },
    );
    doc.resolve(0.0);
    doc
}

fn pixel(doc: &mut HtmlDocument, x: usize, y: usize) -> [u8; 3] {
    let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc, 1.0, SIZE, SIZE, 0, 0),
        SIZE,
        SIZE,
    );
    let i = (y * SIZE as usize + x) * 4;
    [buffer[i], buffer[i + 1], buffer[i + 2]]
}

fn is_red(px: [u8; 3]) -> bool {
    px[0] > 200 && px[1] < 60 && px[2] < 60
}

/// A 40x40 box at (110, 20), outside the viewport, with `style`.
fn outside(style: &str) -> String {
    format!(
        "<div style='position:absolute; left:110px; top:20px; width:40px; height:40px; {style}'>\
         </div>"
    )
}

#[test]
fn an_outset_shadow_reaching_the_viewport_is_painted() {
    let mut doc = document(&outside("box-shadow: -30px 0px 0px 0px #ff0000;"));
    assert!(is_red(pixel(&mut doc, 90, 40)));
}

#[test]
fn a_blurred_shadow_reaching_the_viewport_is_painted() {
    // The blur alone carries it across the edge: the shadow's box ends at
    // 110 and its blur reaches about 20px further out.
    let mut doc = document(&outside("box-shadow: 0px 0px 20px 4px #ff0000;"));
    let px = pixel(&mut doc, 99, 40);
    assert!(px[0] > px[1], "{px:?}");
}

#[test]
fn an_outline_reaching_the_viewport_is_painted() {
    let mut doc = document(&outside("outline: 25px solid #ff0000;"));
    assert!(is_red(pixel(&mut doc, 90, 40)));
}

#[test]
fn an_outline_offset_reaching_the_viewport_is_painted() {
    let mut doc = document(&outside(
        "outline: 4px solid #ff0000; outline-offset: 18px;",
    ));
    assert!(is_red(pixel(&mut doc, 90, 40)));
}

/// The ink bound of the element with id `box`, in its own device pixels.
fn ink(style: &str) -> kurbo::Rect {
    let doc = document(&outside(style).replace("<div ", "<div id='box' "));
    let id = doc.get_element_by_id("box").unwrap();
    *doc.get_node(id).unwrap().ink_overflow()
}

#[test]
fn filters_widen_the_ink_bound() {
    // The CPU renderer draws neither filter yet, so this checks the bound
    // paint culls against rather than pixels.
    assert_eq!(ink(""), kurbo::Rect::new(0.0, 0.0, 40.0, 40.0));
    let shadow = ink("filter: drop-shadow(-30px 0px 2px #ff0000);");
    assert!(shadow.x0 <= -36.0 && shadow.x1 >= 40.0, "{shadow:?}");
    let blur = ink("filter: blur(5px);");
    assert!(blur.x0 <= -15.0 && blur.y1 >= 55.0, "{blur:?}");
}

#[test]
fn a_descendants_shadow_keeps_its_ancestors_painted() {
    // Neither box reaches the viewport; the child's shadow does.
    let mut doc = document(&outside("").replace(
        "</div>",
        "<div style='width:40px; height:40px; box-shadow: -30px 0px 0px 0px #ff0000;'></div></div>",
    ));
    assert!(is_red(pixel(&mut doc, 90, 40)));
}

#[test]
fn a_transformed_box_reaching_the_viewport_is_painted() {
    let mut doc = document(&outside(
        "background: #ff0000; transform: translateX(-30px);",
    ));
    assert!(is_red(pixel(&mut doc, 90, 40)));
}

#[test]
fn an_outline_added_by_a_restyle_is_painted() {
    // Outline changes carry repaint damage only; the bound must still grow.
    let mut doc = document(&outside("").replace("<div ", "<div id='box' "));
    assert!(!is_red(pixel(&mut doc, 90, 40)));
    let id = doc.get_element_by_id("box").unwrap();
    doc.set_style_property(id, "outline", "25px solid #ff0000");
    doc.resolve(0.0);
    assert!(is_red(pixel(&mut doc, 90, 40)));
}

#[test]
fn a_far_element_is_still_culled() {
    let count = |body: &str| {
        let mut doc = document(body);
        let mut scene = anyrender::Scene::new();
        paint_scene(&mut scene, &mut doc, 1.0, SIZE, SIZE, 0, 0);
        scene.commands.len()
    };
    let far = "<div style='position:absolute; left:400px; top:20px; width:40px; height:40px; \
               background:#ff0000; box-shadow: -30px 0px 10px 0px #ff0000; \
               outline: 4px solid #ff0000;'></div>";
    assert_eq!(count(far), count(""));
}

/// A 40x40 box with a red shadow 60px to its left, inside a 100x100
/// wrapper transformed by `transform`.
fn shadowed_under(transform: &str, left: i32) -> String {
    format!(
        "<div style='position:absolute; left:0px; top:0px; width:100px; height:100px; \
         transform:{transform};'>\
         <div style='position:absolute; left:{left}px; top:20px; width:40px; height:40px; \
         box-shadow: -60px 0px 0px 0px #ff0000;'></div></div>"
    )
}

#[test]
fn a_shadow_under_a_scaled_ancestor_moves_with_the_scale() {
    // The box is at 240..280 in the wrapper, 120..140 on screen; its
    // shadow is at 180..220 in the wrapper, 90..110 on screen.
    let mut doc = document(&shadowed_under(
        "scale(0.5); transform-origin: 0px 0px",
        240,
    ));
    assert!(is_red(pixel(&mut doc, 95, 20)));
    assert!(!is_red(pixel(&mut doc, 70, 20)));
}

#[test]
fn a_shadow_under_a_rotated_ancestor_turns_with_it() {
    // Turned half way about the wrapper's center: the box at 110..150
    // lands at -50..-10, its shadow at 50..90 lands at 10..50.
    let mut doc = document(&shadowed_under("rotate(180deg)", 110));
    assert!(is_red(pixel(&mut doc, 30, 60)));
}

/// Paint commands recorded for `body`, less those recorded with the
/// element marked `id='box'` not displayed. Positive when it paints.
fn painted_by_box(body: &str) -> usize {
    let count = |body: &str| {
        let mut doc = document(body);
        let mut scene = anyrender::Scene::new();
        paint_scene(&mut scene, &mut doc, 1.0, SIZE, SIZE, 0, 0);
        scene.commands.len()
    };
    let hidden = body.replacen("id='box' style='", "id='box' style='display:none; ", 1);
    count(body).saturating_sub(count(&hidden))
}

#[test]
fn a_long_line_from_a_narrow_box_is_painted() {
    // The box ends 290px left of the viewport; its unwrapped line doesn't.
    assert!(
        painted_by_box(
            "<div id='box' style='position:absolute; left:-300px; top:20px; width:10px; \
         white-space:pre; font-size:20px; color:#ff0000;'>\
         MMMMMMMMMMMMMMMMMMMMMMMMMMMMMM</div>"
        ) > 0
    );
}

#[test]
fn a_far_underline_is_painted() {
    // Text 180px above the viewport, underlined 200px below its baseline.
    assert!(
        painted_by_box(
            "<div id='box' style='position:absolute; left:10px; top:-180px; width:80px; \
         font-size:10px; text-decoration:underline; text-underline-offset:200px; \
         text-decoration-thickness:4px;'>Hamburgefonstiv</div>"
        ) > 0
    );
}

#[test]
fn a_larger_inline_run_than_its_root_is_painted() {
    // A one pixel line box 15px below the viewport, holding an 80px run
    // whose glyphs rise into view.
    assert!(
        painted_by_box(
            "<div id='box' style='position:absolute; left:10px; top:115px; width:80px; \
         font-size:10px; line-height:1px;'><span style='font-size:80px;'>H</span></div>"
        ) > 0
    );
}

#[test]
fn a_wide_outside_marker_is_painted() {
    // The item's box starts 15px right of the viewport; its nine digit
    // marker is drawn to the left of it, into view.
    assert!(
        painted_by_box(
            "<ol style='margin:0; padding:0;' start='100000000'>\
         <li id='box' style='position:absolute; left:115px; top:20px; width:40px; \
         font-size:10px; list-style-position:outside;'>x</li></ol>"
        ) > 0
    );
}
