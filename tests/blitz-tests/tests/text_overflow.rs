//! `text-overflow: ellipsis`: painted only when a single clipped line overflows,
//! at its end edge, and with the whole text kept everywhere but the paint.

use anyrender::render_to_buffer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::{HtmlDocument, HtmlProvider};
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use std::sync::Arc;

const W: u32 = 320;
const H: u32 = 40;
/// The box, inside the page.
const LEFT: usize = 20;
const LONG: &str = "Sphinx of black quartz judge my vow and then some more";

fn doc_of(box_css: &str, inner: &str) -> HtmlDocument {
    let html = format!(
        r#"<html><body style="margin:0; background:#fff;">
        <div id="box" style="position:absolute; left:{LEFT}px; top:8px; height:20px;
             line-height:20px; font:16px sans-serif; color:#000; white-space:nowrap;
             {box_css}">{inner}</div>
        </body></html>"#
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

fn paint(doc: &mut HtmlDocument) -> Vec<u8> {
    render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| paint_scene(scene, doc.as_mut(), 1.0, W, H, 0, 0),
        W,
        H,
    )
}

fn render(box_css: &str, inner: &str) -> Vec<u8> {
    paint(&mut doc_of(box_css, inner))
}

fn inked(px: &[u8], x: usize, y: usize) -> bool {
    px[(y * W as usize + x) * 4] < 128
}

/// The rows with ink between columns `from` and `to`.
fn ink_rows(px: &[u8], from: usize, to: usize) -> Vec<usize> {
    (0..H as usize)
        .filter(|y| (from..to).any(|x| inked(px, x, *y)))
        .collect()
}

/// Whether the line between those columns ends in an ellipsis: three separate
/// marks at its cut end, low on the line, where text that merely stopped has
/// ink up its height. The cut drops whole clusters, so the marks may stop short
/// of the edge; they are read from the ink, not from a fixed place.
fn ends_in_an_ellipsis(px: &[u8], left: usize, right: usize, rtl: bool) -> bool {
    // Dark enough for a thin dot, not so faint that a glyph's antialiased edge
    // joins the mark beside it.
    let touched = |x: usize, y: usize| px[(y * W as usize + x) * 4] < 200;
    let mut marks: Vec<(usize, usize)> = Vec::new();
    let mut open = None;
    for x in left..=right {
        let ink = x < right && (0..H as usize).any(|y| touched(x, y));
        match (ink, open) {
            (true, None) => open = Some(x),
            (false, Some(from)) => {
                marks.push((from, x));
                open = None;
            }
            _ => {}
        }
    }
    if marks.len() < 4 {
        return false;
    }
    if rtl {
        marks.reverse();
    }
    let low = |&(from, to): &(usize, usize)| {
        let rows: Vec<usize> = (0..H as usize)
            .filter(|y| (from..to).any(|x| touched(x, *y)))
            .collect();
        rows.last().unwrap() - rows.first().unwrap() <= 3
    };
    let tail = &marks[marks.len() - 3..];
    tail.iter().all(low) && !low(&marks[marks.len() - 4])
}

const CLIPPED: &str = "overflow:hidden; width:120px;";
const ELLIPSIS: &str = "overflow:hidden; width:120px; text-overflow:ellipsis;";

#[test]
fn an_overflowing_line_ends_in_an_ellipsis_inside_the_box() {
    let px = render(ELLIPSIS, LONG);
    let right = LEFT + 120;
    assert!(ends_in_an_ellipsis(&px, LEFT, right, false));
    assert!(
        ink_rows(&px, right, W as usize).is_empty(),
        "nothing past the box"
    );
    assert!(
        !ink_rows(&px, LEFT, LEFT + 40).is_empty(),
        "the start stays"
    );
    assert_ne!(px, render(CLIPPED, LONG), "it isn't the plain clip");
}

#[test]
fn the_plain_clip_has_no_ellipsis() {
    let px = render(CLIPPED, LONG);
    assert!(!ends_in_an_ellipsis(&px, LEFT, LEFT + 120, false));
}

#[test]
fn a_line_that_fits_paints_as_it_did() {
    let css = "overflow:hidden; width:300px;";
    let with = format!("{css} text-overflow:ellipsis;");
    assert_eq!(render(&with, "Short"), render(css, "Short"));
}

#[test]
fn it_needs_overflow_to_clip_and_the_property_to_be_set() {
    let visible = "width:120px; text-overflow:ellipsis;";
    let without = "width:120px;";
    assert_eq!(render(visible, LONG), render(without, LONG));
    let clip = "overflow:hidden; width:120px; text-overflow:clip;";
    assert_eq!(render(clip, LONG), render(CLIPPED, LONG));
}

#[test]
fn several_lines_keep_the_plain_clip() {
    let wrap = "overflow:hidden; width:120px; height:40px; white-space:normal;";
    let with = format!("{wrap} text-overflow:ellipsis;");
    assert_eq!(render(&with, LONG), render(wrap, LONG));
}

#[test]
fn the_two_value_form_keeps_the_plain_clip() {
    let two = "overflow:hidden; width:120px; text-overflow:clip ellipsis;";
    assert_eq!(render(two, LONG), render(CLIPPED, LONG));
}

#[test]
fn an_inline_box_on_the_line_keeps_the_plain_clip() {
    let inner =
        format!(r#"<span style="display:inline-block; width:4px; height:4px;"></span>{LONG}"#);
    assert_eq!(render(ELLIPSIS, &inner), render(CLIPPED, &inner));
}

#[test]
fn only_the_paint_changes() {
    let doc = doc_of(ELLIPSIS, LONG);
    let plain = doc_of(CLIPPED, LONG);
    let id = |d: &HtmlDocument| d.query_selector("#box").unwrap().unwrap();
    let node = doc.get_node(id(&doc)).unwrap();
    assert_eq!(node.text_content(), LONG);
    let other = plain.get_node(id(&plain)).unwrap();
    assert_eq!(node.scroll_width(), other.scroll_width());
    assert!(node.scroll_width() > 120.0);
}

#[test]
fn right_to_left_keeps_the_start_on_the_right_and_cuts_on_the_left() {
    let rtl = format!("{ELLIPSIS} direction:rtl;");
    let px = render(&rtl, LONG);
    assert!(ends_in_an_ellipsis(&px, LEFT, LEFT + 120, true));
    assert!(ink_rows(&px, 0, LEFT).is_empty(), "nothing past the box");
    assert!(
        !ink_rows(&px, LEFT + 80, LEFT + 120).is_empty(),
        "the start stays"
    );
}

#[test]
fn a_narrow_box_shows_only_what_fits_and_never_panics() {
    for width in [0, 1, 4, 8, 14, 20] {
        let css = format!("overflow:hidden; width:{width}px; text-overflow:ellipsis;");
        let px = render(&css, LONG);
        assert!(
            ink_rows(&px, LEFT + width, W as usize).is_empty(),
            "nothing past a {width}px box"
        );
        let rtl = format!("{css} direction:rtl;");
        let px = render(&rtl, LONG);
        assert!(ink_rows(&px, 0, LEFT).is_empty(), "nothing before it");
    }
}

#[test]
fn unicode_is_cut_on_whole_clusters() {
    // Combining marks, a ZWJ family, a flag, and text from several fonts.
    for text in [
        "e\u{301}".repeat(60),
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".repeat(30),
        "\u{1F1EF}\u{1F1F9}".repeat(30),
        "日本語のテキストと混ざった English テキスト".repeat(5),
        "مرحبا بالعالم هذا نص طويل جدا مع English داخله".repeat(5),
    ] {
        for css in [ELLIPSIS.to_owned(), format!("{ELLIPSIS} direction:rtl;")] {
            let px = render(&css, &text);
            assert!(ink_rows(&px, LEFT + 120, W as usize).is_empty(), "{text}");
            assert!(ink_rows(&px, 0, LEFT).is_empty(), "{text}");
        }
    }
}

#[test]
fn a_name_given_way_to_in_a_flex_row_is_cut_at_what_it_was_left() {
    // As a tab or tree row lays it out: an icon, then a name with a limit.
    let inner = format!(
        r#"<span style="flex:none; width:24px; height:20px; background:#fff"></span>
        <span style="display:block; min-width:0; max-width:160px; overflow:hidden;
                     text-overflow:ellipsis;">{LONG}</span>"#
    );
    let px = render("display:flex; width:400px;", &inner);
    let (left, right) = (LEFT + 24, LEFT + 24 + 160);
    assert!(ends_in_an_ellipsis(&px, left, right, false), "at its limit");
    assert!(ink_rows(&px, right, W as usize).is_empty());

    let px = render("display:flex; width:120px;", &inner);
    let right = LEFT + 120;
    assert!(ends_in_an_ellipsis(&px, left, right, false), "given way");
    assert!(ink_rows(&px, right, W as usize).is_empty());

    let short = inner.replace(LONG, "Home");
    let px = render("display:flex; width:400px;", &short);
    assert!(!ends_in_an_ellipsis(&px, left, right, false), "fits");
}

#[test]
fn styled_runs_inside_the_line_are_cut_as_one_line() {
    let inner = format!(r#"Sphinx <b style="color:#000">of black</b> <i>quartz</i> {LONG}"#);
    let px = render(ELLIPSIS, &inner);
    let right = LEFT + 120;
    assert!(ink_rows(&px, right, W as usize).is_empty());
    assert!(ends_in_an_ellipsis(&px, LEFT, right, false));
}

#[test]
fn it_follows_the_box_as_the_box_changes() {
    let mut doc = doc_of("overflow:hidden; width:50%; text-overflow:ellipsis;", LONG);
    let wide = paint(&mut doc);
    assert!(ends_in_an_ellipsis(&wide, LEFT, LEFT + 160, false));
    doc.set_viewport(Viewport::new(W / 2, H, 1.0, ColorScheme::Light));
    doc.resolve(0.0);
    let narrow = paint(&mut doc);
    assert!(ends_in_an_ellipsis(&narrow, LEFT, LEFT + 80, false));
    assert!(ink_rows(&narrow, LEFT + 80, W as usize).is_empty());
}

/// The pieces a line is cut between, for `text` as shaped in a box.
fn groups(text: &str) -> Vec<(f32, f32)> {
    let doc = doc_of(ELLIPSIS, text);
    let id = doc.query_selector("#box").unwrap().unwrap();
    let data = doc.get_node(id).unwrap().element_data().unwrap();
    let inline = data.inline_layout_data.as_ref().unwrap();
    blitz_paint::line_groups(&inline.layout, &inline.text)
}

#[test]
fn a_grapheme_is_never_two_pieces() {
    // Whether the system's fonts can draw these or not.
    for (one, what) in [
        ("a", "a letter"),
        ("e\u{301}", "a combining mark"),
        ("\u{1F1EF}\u{1F1F9}", "a flag"),
        ("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}", "a family"),
        ("\u{1F44D}\u{1F3FD}", "a skin tone"),
        ("\u{1100}\u{1161}", "conjoining jamo"),
    ] {
        // Shaping may join more, as it joins a run of flags: a piece is never
        // smaller than a grapheme, so there are never more than there are.
        let pieces = groups(&one.repeat(5)).len();
        assert!((1..=5).contains(&pieces), "{what}: {pieces}");
    }
}

#[test]
fn plain_letters_and_marks_are_a_piece_each() {
    assert_eq!(groups(&"a".repeat(5)).len(), 5);
    assert_eq!(groups(&"e\u{301}".repeat(5)).len(), 5);
}

#[test]
fn mixed_directions_read_left_to_right_without_overlap() {
    let groups = groups("abc \u{645}\u{631}\u{62D}\u{628}\u{627} def");
    assert!(groups.len() >= 11, "{groups:?}");
    for pair in groups.windows(2) {
        assert!(pair[0].1 <= pair[1].0 + 0.01, "{pair:?}");
    }
}

#[test]
fn an_indent_that_pushes_a_fitting_line_past_the_box_ends_it_in_an_ellipsis() {
    let short = "Sphinx of black quartz";
    let css = "overflow:hidden; width:200px; text-overflow:ellipsis;";
    let px = render(css, short);
    assert!(
        !ends_in_an_ellipsis(&px, LEFT, LEFT + 200, false),
        "fits unindented"
    );
    let indented = format!("{css} text-indent:80px;");
    let px = render(&indented, short);
    assert!(
        ends_in_an_ellipsis(&px, LEFT, LEFT + 200, false),
        "past by its indent"
    );
    assert!(ink_rows(&px, LEFT + 200, W as usize).is_empty());
}

#[test]
fn a_longer_line_keeps_its_indent() {
    let css = format!("{ELLIPSIS} text-indent:40px;");
    let px = render(&css, LONG);
    assert!(
        ink_rows(&px, LEFT, LEFT + 38).is_empty(),
        "the indent stays"
    );
    assert!(ends_in_an_ellipsis(&px, LEFT, LEFT + 120, false));
    assert!(ink_rows(&px, LEFT + 120, W as usize).is_empty());
}

/// Columns `from..to` of every row.
fn columns(px: &[u8], from: usize, to: usize) -> Vec<u8> {
    (0..H as usize)
        .flat_map(|y| (from..to).map(move |x| px[(y * W as usize + x) * 4]))
        .collect()
}

#[test]
fn right_to_left_hanging_space_leaves_the_right_edge_as_it_was() {
    let rtl = format!("{ELLIPSIS} direction:rtl;");
    let plain = render(&rtl, LONG);
    let hanging = render(&rtl, &format!("{LONG}\u{3000}"));
    assert!(!ink_rows(&plain, LEFT + 60, LEFT + 120).is_empty());
    assert_eq!(
        columns(&plain, LEFT + 60, W as usize),
        columns(&hanging, LEFT + 60, W as usize),
        "the start, at the right, doesn't move"
    );
    assert!(ink_rows(&hanging, LEFT + 120, W as usize).is_empty());
}

#[test]
fn right_to_left_keeps_its_indent_at_the_right() {
    let css = format!("{ELLIPSIS} direction:rtl; text-indent:30px;");
    let px = render(&css, LONG);
    assert!(
        ink_rows(&px, LEFT + 92, W as usize).is_empty(),
        "the indent stays"
    );
    assert!(ends_in_an_ellipsis(&px, LEFT, LEFT + 92, true));
}

#[test]
fn a_grapheme_split_across_font_sizes_is_one_piece() {
    // A ZWJ family whose last part is set larger, so it shapes in two runs.
    let split = "<span style=\"font-size:16px\">\u{1F468}\u{200D}\u{1F469}</span>\
                 <span style=\"font-size:24px\">\u{200D}\u{1F467}</span>";
    assert_eq!(groups(split).len(), 1, "{:?}", groups(split));
    let after = format!("{split}ab");
    assert_eq!(groups(&after).len(), 3, "{:?}", groups(&after));
}
