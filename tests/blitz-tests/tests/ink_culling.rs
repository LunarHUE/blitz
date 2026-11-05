//! Paint culls an element only when nothing it paints can reach the
//! viewport. Each element here has its box just outside a 100x100 viewport,
//! past its right edge, and paints red across that edge with a shadow, an
//! outline or a filter. The pixel at (90, 40) must be red.

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

#[test]
fn a_drop_shadow_filter_reaching_the_viewport_is_painted() {
    let mut doc = document(&outside(
        "background: #ff0000; filter: drop-shadow(-30px 0px 0px #ff0000);",
    ));
    assert!(is_red(pixel(&mut doc, 90, 40)));
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
