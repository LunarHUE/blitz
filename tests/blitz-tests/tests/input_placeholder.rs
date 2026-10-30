//! A text input's `placeholder`: painted in the `::placeholder` style
//! while the input holds no text, hidden by typed or composed text, clipped
//! to the input, and exposed to assistive tech apart from the name.
//!
//! The pixel checks colour the value black and the hint pure red or blue,
//! then count pixels of each hue in a CPU render.

use accesskit::{Node as AccessKitNode, Role};
use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_html::HtmlDocument;
use blitz_paint::paint_scene;
use blitz_test_harness::{Harness, HarnessOptions};
use blitz_traits::events::BlitzImeEvent;
use blitz_traits::shell::{ColorScheme, Viewport};
use keyboard_types::{Key, NamedKey};
use markup5ever::{QualName, local_name, ns};
use test_that::prelude::*;

const W: u32 = 320;
const H: u32 = 60;

fn page(input: &str) -> Harness<HtmlDocument> {
    let html = format!(
        r#"<html><head><style>
            body {{ margin: 0; background: #fff; }}
            input {{
                font: 16px sans-serif; color: #000; background: #fff;
                border: 0; padding: 0 4px; margin: 0;
                width: 200px; height: 30px; box-sizing: border-box;
            }}
            input::placeholder {{ color: #ff0000; }}
            input:disabled::placeholder {{ color: #0000ff; }}
            #narrow {{ width: 80px; }}
            #plain::placeholder {{ color: revert; }}
        </style></head><body>{input}</body></html>"#
    );
    Harness::from_html_with(
        &html,
        HarnessOptions {
            width: W,
            height: H,
            ..Default::default()
        },
    )
}

fn render(h: &mut Harness<HtmlDocument>) -> Vec<u8> {
    h.pump();
    let mut doc = h.base_mut();
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, &mut doc, 1.0, W, H, 0, 0),
        W,
        H,
    )
}

#[derive(Clone, Copy)]
enum Hue {
    Red,
    Blue,
    Dark,
}

fn is(px: &[u8], hue: Hue) -> bool {
    let (r, g, b) = (px[0] as i32, px[1] as i32, px[2] as i32);
    match hue {
        Hue::Red => r > g + 100 && r > b + 100,
        Hue::Blue => b > r + 100 && b > g + 100,
        Hue::Dark => r < 90 && g < 90 && b < 90,
    }
}

/// Pixels of `hue`, and the largest x among them.
fn count(buf: &[u8], hue: Hue) -> (usize, usize) {
    let mut n = 0;
    let mut max_x = 0;
    for (i, px) in buf.as_chunks::<4>().0.iter().enumerate() {
        if is(px, hue) {
            n += 1;
            max_x = max_x.max(i % W as usize);
        }
    }
    (n, max_x)
}

#[test]
fn an_empty_input_paints_its_placeholder() {
    let mut h = page(r#"<input id="a" placeholder="Search by name">"#);
    let buf = render(&mut h);
    let (red, max_x) = count(&buf, Hue::Red);
    assert!(red > 20, "the hint draws ({red} red pixels)");
    assert!(max_x < 200, "inside the input");
    assert_eq!(count(&buf, Hue::Dark).0, 0, "and doesn't draw as a value");
}

#[test]
fn a_value_hides_the_placeholder() {
    let mut h = page(r#"<input id="a" placeholder="Search by name" value="Ada">"#);
    let buf = render(&mut h);
    assert_eq!(count(&buf, Hue::Red).0, 0);
    assert!(count(&buf, Hue::Dark).0 > 0, "the value draws");
}

#[test]
fn typing_hides_it_and_clearing_brings_it_back() {
    let mut h = page(r#"<input id="a" placeholder="Search by name">"#);
    h.click("#a");
    h.type_text("x");
    let buf = render(&mut h);
    assert_eq!(count(&buf, Hue::Red).0, 0, "typed text hides it");

    h.press(Key::Named(NamedKey::Backspace));
    let buf = render(&mut h);
    assert!(
        count(&buf, Hue::Red).0 > 20,
        "an emptied, focused input shows it"
    );
}

#[test]
fn composing_text_hides_it() {
    let mut h = page(r#"<input id="a" placeholder="Search by name">"#);
    h.click("#a");
    h.ime(BlitzImeEvent::Enabled);
    h.ime(BlitzImeEvent::Preedit("ka".into(), Some((2, 2))));
    let buf = render(&mut h);
    assert_eq!(count(&buf, Hue::Red).0, 0, "no hint under a preedit");
    assert!(count(&buf, Hue::Dark).0 > 0, "the preedit draws");

    h.ime(BlitzImeEvent::Preedit(String::new(), None));
    let buf = render(&mut h);
    assert!(
        count(&buf, Hue::Red).0 > 20,
        "a cancelled composition shows it again"
    );
}

#[test]
fn a_long_placeholder_is_clipped_to_the_input() {
    let mut h = page(
        r#"<input id="narrow" placeholder="A placeholder far longer than the field is wide">"#,
    );
    let buf = render(&mut h);
    let (red, max_x) = count(&buf, Hue::Red);
    assert!(red > 20);
    // Content box: 80px less 4px of padding each side.
    assert!(max_x < 76, "clipped at the content box, drew to x={max_x}");
}

#[test]
fn disabled_and_readonly_inputs_show_it_in_their_own_style() {
    let mut h = page(r#"<input id="a" placeholder="Search by name" disabled>"#);
    let buf = render(&mut h);
    assert!(
        count(&buf, Hue::Blue).0 > 20,
        "input:disabled::placeholder applies"
    );
    assert_eq!(count(&buf, Hue::Red).0, 0);

    let mut h = page(r#"<input id="a" placeholder="Search by name" readonly>"#);
    let buf = render(&mut h);
    assert!(count(&buf, Hue::Red).0 > 20);
}

#[test]
fn without_a_rule_it_takes_the_ua_colour() {
    let mut h = page(r#"<input id="plain" placeholder="Search by name">"#);
    let buf = render(&mut h);
    assert_eq!(count(&buf, Hue::Dark).0, 0, "never value-black");
    let grey = buf
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] == p[1] && p[1] == p[2] && (150..=200).contains(&p[0]))
        .count();
    assert!(grey > 5, "darkgray glyphs ({grey} pixels)");
}

#[test]
fn changing_the_attribute_updates_it() {
    let mut h = page(r#"<input id="a">"#);
    assert_eq!(count(&render(&mut h), Hue::Red).0, 0);

    let id = h.node("#a");
    let name = QualName::new(None, ns!(), local_name!("placeholder"));
    h.base_mut()
        .mutate()
        .set_attribute(id, name.clone(), "Search by name");
    assert!(count(&render(&mut h), Hue::Red).0 > 20, "added");

    h.base_mut().mutate().clear_attribute(id, name);
    assert_eq!(count(&render(&mut h), Hue::Red).0, 0, "removed");
}

#[test]
fn showing_it_moves_nothing() {
    let h = page(r#"<input id="a" placeholder="Search by name"><input id="b">"#);
    let a = h.layout_rect("#a");
    let b = h.layout_rect("#b");
    assert_eq!((a.width, a.height), (b.width, b.height), "same box");

    let doc = h.base();
    let input = |sel: &str| {
        let id = doc.query_selector(sel).unwrap().unwrap();
        doc.get_node(id).unwrap()
    };
    let (a, b) = (input("#a"), input("#b"));
    assert_eq!(
        a.text_input_v_centering_offset(1.0),
        b.text_input_v_centering_offset(1.0),
        "the text sits at the same height"
    );
    let data = a.element_data().unwrap().text_input_data().unwrap();
    let (hint, _) = data.shown_placeholder().expect("shown");
    let value = data.editor.try_layout().unwrap();
    assert_eq!(
        hint.height(),
        value.height(),
        "one line of the value's height"
    );
}

#[test]
fn single_line_inputs_drop_line_breaks() {
    let h = page("<input id=\"a\" placeholder=\"Search&#10;by name\">");
    let doc = h.base();
    let id = doc.query_selector("#a").unwrap().unwrap();
    let data = doc
        .get_node(id)
        .unwrap()
        .element_data()
        .unwrap()
        .text_input_data()
        .unwrap();
    assert_eq!(data.placeholder_text(), Some("Searchby name"));
}

#[test]
fn it_is_the_placeholder_not_the_name() -> TestResult<()> {
    let mut h = page(
        r#"<input aria-label="Members" placeholder="Search by name"><input placeholder="Only a hint">"#,
    );
    let tree = h.base_mut().build_accessibility_tree();
    verify_that!(
        tree.nodes,
        contains((
            anything(),
            matches_pattern!(AccessKitNode {
                role(): eq(Role::TextInput),
                label(): some(eq("Members")),
                placeholder(): some(eq("Search by name")),
            })
        ))
    )?;
    verify_that!(
        tree.nodes,
        contains((
            anything(),
            matches_pattern!(AccessKitNode {
                role(): eq(Role::TextInput),
                label(): none(),
                placeholder(): some(eq("Only a hint")),
            })
        ))
    )
}

#[test]
fn a_smaller_placeholder_font_moves_nothing() {
    let mut h = page(
        r#"<style>#small::placeholder { font-size: 10px; }</style>
           <input id="small" placeholder="Search by name"><input id="twin">"#,
    );
    let a = h.layout_rect("#small");
    let b = h.layout_rect("#twin");
    assert_eq!((a.width, a.height), (b.width, b.height), "same box");
    {
        let doc = h.base();
        let node = |sel: &str| {
            doc.get_node(doc.query_selector(sel).unwrap().unwrap())
                .unwrap()
        };
        assert_eq!(
            node("#small").text_input_v_centering_offset(1.0),
            node("#twin").text_input_v_centering_offset(1.0),
            "the caret sits where it would without a hint"
        );
    }
    // The hint draws smaller than the 16px one would.
    let small = count(&render(&mut h), Hue::Red).0;
    let mut big = page(r#"<input id="a" placeholder="Search by name">"#);
    let full = count(&render(&mut big), Hue::Red).0;
    assert!(
        small > 10 && small < full,
        "{small} red pixels at 10px, {full} at 16px"
    );
}

/// A page whose inputs restyle their hint without reconstructing a box.
fn restyled_page() -> Harness<HtmlDocument> {
    page(
        r#"<style>
             /* No focus ring, so blue pixels are the hint's alone. */
             input:focus { outline: none; }
             input:focus::placeholder { color: #0000ff; }
             .night input::placeholder { color: #0000ff; }
           </style>
           <div id="wrap"><input id="a" placeholder="Search by name"><input id="b"></div>"#,
    )
}

#[test]
fn focus_and_blur_restyle_it() {
    let mut h = restyled_page();
    let (a, b) = (h.node("#a"), h.node("#b"));
    assert!(count(&render(&mut h), Hue::Red).0 > 20, "blurred: red");

    h.base_mut().set_focus_to(a);
    let buf = render(&mut h);
    assert!(
        count(&buf, Hue::Blue).0 > 20,
        "input:focus::placeholder applies"
    );
    assert_eq!(count(&buf, Hue::Red).0, 0);

    h.base_mut().set_focus_to(b);
    let buf = render(&mut h);
    assert!(count(&buf, Hue::Red).0 > 20, "blurred again: red");
    assert_eq!(count(&buf, Hue::Blue).0, 0);
}

#[test]
fn a_colour_only_theme_change_restyles_it() {
    let mut h = restyled_page();
    let wrap = h.node("#wrap");
    let class = QualName::new(None, ns!(), local_name!("class"));
    h.base_mut()
        .mutate()
        .set_attribute(wrap, class.clone(), "night");
    let buf = render(&mut h);
    assert!(
        count(&buf, Hue::Blue).0 > 20,
        "an ancestor's theme class applies"
    );
    assert_eq!(count(&buf, Hue::Red).0, 0);

    h.base_mut().mutate().clear_attribute(wrap, class);
    assert!(count(&render(&mut h), Hue::Red).0 > 20, "and comes off");
}

#[test]
fn a_colour_scheme_change_restyles_it() {
    let mut h = page(
        r#"<style>@media (prefers-color-scheme: dark) { #themed::placeholder { color: #0000ff; } }</style>
           <input id="themed" placeholder="Search by name">"#,
    );
    assert!(count(&render(&mut h), Hue::Red).0 > 20, "light: red");
    {
        let mut doc = h.base_mut();
        let v = doc.get_viewport();
        doc.set_viewport(Viewport::new(
            v.window_size.0,
            v.window_size.1,
            v.hidpi_scale,
            ColorScheme::Dark,
        ));
    }
    let buf = render(&mut h);
    assert!(
        count(&buf, Hue::Blue).0 > 20,
        "dark: the media rule applies"
    );
    assert_eq!(count(&buf, Hue::Red).0, 0);
}

#[test]
fn it_follows_the_display_scale() {
    let mut h = page(r#"<input id="a" placeholder="Search by name">"#);
    let at_1 = count(&render(&mut h), Hue::Red).0;
    {
        let mut doc = h.base_mut();
        let v = doc.get_viewport();
        doc.set_viewport(Viewport::new(W * 2, H * 2, 2.0, v.color_scheme));
    }
    h.pump();
    let buf = {
        let mut doc = h.base_mut();
        render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| paint_scene(scene, &mut doc, 2.0, W * 2, H * 2, 0, 0),
            W * 2,
            H * 2,
        )
    };
    let mut red = 0;
    let mut max_x = 0;
    for (i, px) in buf.as_chunks::<4>().0.iter().enumerate() {
        if is(px, Hue::Red) {
            red += 1;
            max_x = max_x.max(i % (W as usize * 2));
        }
    }
    assert!(
        red > at_1 * 2,
        "drawn at 2x: {red} red pixels against {at_1} at 1x"
    );
    assert!(max_x < 400, "still inside the 200px input");
}

#[test]
fn a_textarea_shows_it_until_text_arrives() {
    let mut h = page(
        r#"<textarea id="t" placeholder="Notes" style="font: 16px sans-serif; color: #000; width: 200px; height: 40px; border: 0; padding: 0;"></textarea>
           <style>textarea::placeholder { color: #ff0000; }</style>"#,
    );
    assert!(
        count(&render(&mut h), Hue::Red).0 > 10,
        "empty textarea: hint"
    );
    h.click("#t");
    h.type_text("x");
    assert_eq!(count(&render(&mut h), Hue::Red).0, 0, "typed: gone");
}
