use crate::node::{Node, NodeData};
use color::{AlphaColor, Srgb};
use keyboard_types::Modifiers;
use std::borrow::Cow;
use style::color::AbsoluteColor;

#[cfg(target_os = "macos")]
pub(crate) const ACTION_MOD: Modifiers = Modifiers::META;
#[cfg(not(target_os = "macos"))]
pub(crate) const ACTION_MOD: Modifiers = Modifiers::CONTROL;

pub type Color = AlphaColor<Srgb>;

/// Decode raw font bytes, decompressing WOFF/WOFF2 if the `woff` feature is enabled.
/// Returns the original slice unchanged for TTF/OTF input, and also on decompression
/// failure. With the `woff` feature disabled, all input passes through unchanged.
pub fn decode_font_bytes(bytes: &[u8]) -> Cow<'_, [u8]> {
    if bytes.len() < 4 {
        return Cow::Borrowed(bytes);
    }
    match &bytes[0..4] {
        #[cfg(feature = "woff")]
        b"wOFF" => wuff::decompress_woff1(bytes)
            .map(Cow::Owned)
            .unwrap_or_else(|_| {
                #[cfg(feature = "tracing")]
                tracing::warn!("Failed to decompress woff1 font");
                Cow::Borrowed(bytes)
            }),
        #[cfg(feature = "woff")]
        b"wOF2" => wuff::decompress_woff2(bytes)
            .map(Cow::Owned)
            .unwrap_or_else(|_| {
                #[cfg(feature = "tracing")]
                tracing::warn!("Failed to decompress woff2 font");
                Cow::Borrowed(bytes)
            }),
        _ => Cow::Borrowed(bytes),
    }
}

#[cfg(feature = "svg")]
use std::sync::{Arc, LazyLock};
#[cfg(feature = "svg")]
use usvg::fontdb;
#[cfg(feature = "svg")]
pub(crate) static FONT_DB: LazyLock<Arc<fontdb::Database>> = LazyLock::new(|| {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    Arc::new(db)
});

/// Which kind of CSS image layer list (`background-image` or `mask-image`) to
/// flush from style to dedicated storage on the node.
#[derive(Clone, Copy, Debug)]
pub enum ImageLayerKind {
    Background,
    Mask,
}

impl ImageLayerKind {
    pub fn image_type(self, idx: usize) -> ImageType {
        match self {
            Self::Background => ImageType::Background(idx),
            Self::Mask => ImageType::Mask(idx),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ImageType {
    Image,
    Background(usize),
    Mask(usize),
}

/// A point
#[derive(Clone, Debug, Copy, Eq, PartialEq)]
pub struct Point<T> {
    /// The x coordinate
    pub x: T,
    /// The y coordinate
    pub y: T,
}

impl Point<f64> {
    pub const ZERO: Self = Point { x: 0.0, y: 0.0 };
}

// Debug print an RcDom
pub fn walk_tree(indent: usize, node: &Node) {
    // Skip all-whitespace text nodes entirely
    if let NodeData::Text(data) = &node.data {
        if data.content.chars().all(|c| c.is_ascii_whitespace()) {
            return;
        }
    }

    print!("{}", " ".repeat(indent));
    let id = node.id;
    match &node.data {
        NodeData::Document(_) => println!("#Document {id}"),

        NodeData::Text(data) => {
            if data.content.chars().all(|c| c.is_ascii_whitespace()) {
                println!("{id} #text: <whitespace>");
            } else {
                let content = data.content.trim();
                if content.len() > 10 {
                    println!(
                        "#text {id}: {}...",
                        content
                            .split_at(content.char_indices().take(10).last().unwrap().0)
                            .0
                            .escape_default()
                    )
                } else {
                    println!("#text {id}: {}", data.content.trim().escape_default())
                }
            }
        }

        NodeData::Comment { .. } => println!("<!-- COMMENT {id} -->"),

        NodeData::AnonymousBlock(_) => println!("{id} AnonymousBlock"),

        NodeData::Element(data) => {
            print!("<{} {id}", data.name.local);
            for attr in data.attrs.iter() {
                print!(" {}=\"{}\"", attr.name.local, attr.value);
            }
            if !node.children.is_empty() {
                println!(">");
            } else {
                println!("/>");
            }
        } // NodeData::Doctype {
          //     ref name,
          //     ref public_id,
          //     ref system_id,
          // } => println!("<!DOCTYPE {} \"{}\" \"{}\">", name, public_id, system_id),
          // NodeData::ProcessingInstruction { .. } => unreachable!(),
    }

    if !node.children.is_empty() {
        for child_id in node.children.iter() {
            walk_tree(indent + 2, node.with(*child_id));
        }

        if let NodeData::Element(data) = &node.data {
            println!("{}</{}>", " ".repeat(indent), data.name.local);
        }
    }
}

/// The font size an SVG document loaded as an image starts from: CSS's
/// initial `medium`. usvg's own default is 12px.
#[cfg(feature = "svg")]
pub(crate) const SVG_IMAGE_FONT_SIZE: f32 = 16.0;

/// Parse an SVG image. `font_size` is the root's font size in CSS pixels,
/// which `em` lengths in the SVG resolve against, and `style_sheet` is
/// applied before the svg's own style sheets.
#[cfg(feature = "svg")]
pub(crate) fn parse_svg_image(
    source: &[u8],
    font_size: f32,
    style_sheet: Option<String>,
) -> Result<crate::node::SvgImageData, usvg::Error> {
    let options = usvg::Options {
        fontdb: Arc::clone(&*FONT_DB),
        font_size,
        style_sheet,
        ..Default::default()
    };
    crate::node::SvgImageData::from_data(source, &options)
}

pub trait ToColorColor {
    /// Converts a color into the `AlphaColor<Srgb>` type from the `color` crate
    fn as_color_color(&self) -> Color;
}
impl ToColorColor for AbsoluteColor {
    fn as_color_color(&self) -> Color {
        Color::new(
            *self
                .to_color_space(style::color::ColorSpace::Srgb)
                .raw_components(),
        )
    }
}

#[cfg(all(test, feature = "svg"))]
mod svg_tests {
    use super::parse_svg_image;

    #[test]
    fn svg_without_xmlns_parses() {
        let src = br#"<svg viewBox="0 0 24 24"><path d="M0 0h24v24H0z"/></svg>"#;
        let svg = parse_svg_image(src, super::SVG_IMAGE_FONT_SIZE, None).unwrap();
        assert!(!svg.tree.root().children().is_empty());
    }

    #[test]
    fn missing_height_is_computed_from_width_and_viewbox_ratio() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="200"><rect width="100%" height="100%" fill="green"/></svg>"#;
        let svg = parse_svg_image(src, super::SVG_IMAGE_FONT_SIZE, None).unwrap();
        assert_eq!(svg.intrinsic_width(), Some(200.0));
        assert_eq!(svg.intrinsic_height(), None);
        assert_eq!(svg.viewbox_aspect_ratio(), Some(1.0));
        assert_eq!(svg.tree.size().width(), 200.0);
        assert_eq!(svg.intrinsic_size(), (200.0, 200.0));
    }

    #[test]
    fn viewbox_only_has_no_intrinsic_dimensions() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 485 58"></svg>"#;
        let svg = parse_svg_image(src, super::SVG_IMAGE_FONT_SIZE, None).unwrap();
        assert_eq!(svg.intrinsic_width(), None);
        assert_eq!(svg.intrinsic_height(), None);
        // The aspect ratio is still available from the viewBox.
        assert!((svg.aspect_ratio() - (485.0 / 58.0)).abs() < 1e-3);
    }

    #[test]
    fn absolute_dimensions_are_intrinsic() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="16" viewBox="0 0 48 32"></svg>"#;
        let svg = parse_svg_image(src, super::SVG_IMAGE_FONT_SIZE, None).unwrap();
        assert_eq!(svg.intrinsic_width(), Some(24.0));
        assert_eq!(svg.intrinsic_height(), Some(16.0));
    }

    #[test]
    fn percentage_dimensions_are_not_intrinsic() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="50%" viewBox="0 0 200 100"></svg>"#;
        let svg = parse_svg_image(src, super::SVG_IMAGE_FONT_SIZE, None).unwrap();
        assert_eq!(svg.intrinsic_width(), None);
        assert_eq!(svg.intrinsic_height(), None);
    }

    #[test]
    fn unit_lengths_are_intrinsic() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" width="24px" height="1.5em" viewBox="0 0 48 32"></svg>"#;
        let svg = parse_svg_image(src, super::SVG_IMAGE_FONT_SIZE, None).unwrap();
        assert!(svg.intrinsic_width().is_some());
        assert!(svg.intrinsic_height().is_some());
    }

    #[test]
    fn non_numeric_dimensions_are_not_intrinsic() {
        let src = br#"<svg xmlns="http://www.w3.org/2000/svg" width="auto" height="foo" viewBox="0 0 200 100"></svg>"#;
        let svg = parse_svg_image(src, super::SVG_IMAGE_FONT_SIZE, None).unwrap();
        assert_eq!(svg.intrinsic_width(), None);
        assert_eq!(svg.intrinsic_height(), None);
    }
}

/// Creates an markup5ever::QualName.
/// Given a local name and an optional namespace
#[macro_export]
macro_rules! qual_name {
    ($local:tt $(, $ns:ident)?) => {
        $crate::QualName {
            prefix: None,
            ns: $crate::ns!($($ns)?),
            local: $crate::local_name!($local),
        }
    };
}

/// Parses an SVG `font-size` presentation attribute with Stylo's own
/// `font-size` grammar (keywords, relative sizes, lengths, percentages,
/// `calc()`), allowing unitless numbers as SVG user units. Returns `None`
/// for a value CSS would reject, as an invalid presentation attribute is
/// ignored.
#[cfg(feature = "svg")]
pub(crate) fn parse_svg_font_size_attr(
    value: &str,
) -> Option<style::properties::PropertyDeclaration> {
    use style::parser::ParserContext;
    use style::properties::{
        LonghandId, PropertyDeclaration, PropertyId, SourcePropertyDeclaration,
    };
    use style::stylesheets::{CssRuleType, Origin};
    use style_traits::ParsingMode;

    let url = crate::url::DocumentUrl::default().url_extra_data();
    let context = ParserContext::new(
        Origin::Author,
        &url,
        Some(CssRuleType::Style),
        ParsingMode::ALLOW_UNITLESS_LENGTH,
        selectors::matching::QuirksMode::NoQuirks,
        Default::default(),
        None,
        None,
        Default::default(),
    );
    let mut declarations = SourcePropertyDeclaration::default();
    let mut parser = cssparser::Parser::new(value);
    PropertyDeclaration::parse_into(
        &mut declarations,
        PropertyId::NonCustom(LonghandId::FontSize.into()),
        &context,
        &mut parser,
    )
    .ok()?;
    parser.expect_exhausted().ok()?;
    declarations.drain().declarations.last()
}

/// The `style` attribute to serialize on an inline `<svg>` root for usvg:
/// `style` parsed as Stylo parses style attributes, with every declaration
/// that sets `font-size` (the longhand, the `font` shorthand, `!important`
/// or not) removed, serialized back, then `font-size` set to the root's
/// computed size in CSS pixels. [`svg_style_sheet_without_root_font_size`]
/// keeps rules in the svg's own `<style>` from overriding it.
#[cfg(feature = "svg")]
pub(crate) fn svg_root_style(style: Option<&str>, font_size: f32) -> String {
    use style::properties::{LonghandId, PropertyId, parse_style_attribute};
    use style::stylesheets::CssRuleType;
    use style_traits::CssStringWriter;

    let mut out = CssStringWriter::new();
    if let Some(style) = style {
        let url = crate::url::DocumentUrl::default().url_extra_data();
        let mut block = parse_style_attribute(
            style,
            &url,
            None,
            selectors::matching::QuirksMode::NoQuirks,
            CssRuleType::Style,
        );
        let font_size = PropertyId::NonCustom(LonghandId::FontSize.into());
        if let Some(first) = block.first_declaration_to_remove(&font_size) {
            block.remove_property(&font_size, first);
        }
        let _ = block.to_css(&mut out);
        if !out.is_empty() {
            out.push(' ');
        }
    }
    out.push_str(&format!("font-size: {font_size}px;"));
    out
}

/// The root of an inline `<svg>` as usvg's CSS matching sees it: no parent
/// and no previous sibling, with the attributes Blitz serializes for it.
#[cfg(feature = "svg")]
pub(crate) struct SvgRoot<'a> {
    pub local_name: &'a str,
    pub attrs: Vec<(&'a str, &'a str)>,
}

#[cfg(feature = "svg")]
impl simplecss::Element for &SvgRoot<'_> {
    fn parent_element(&self) -> Option<Self> {
        None
    }
    fn prev_sibling_element(&self) -> Option<Self> {
        None
    }
    fn has_local_name(&self, name: &str) -> bool {
        self.local_name == name
    }
    fn attribute_matches(&self, name: &str, operator: simplecss::AttributeOperator<'_>) -> bool {
        self.attrs
            .iter()
            .find(|(n, _)| *n == name)
            .is_some_and(|(_, v)| operator.matches(v))
    }
    // As usvg's matcher: the root is a first child; other pseudo-classes
    // never match.
    fn pseudo_class_matches(&self, class: simplecss::PseudoClass<'_>) -> bool {
        matches!(class, simplecss::PseudoClass::FirstChild)
    }
}

/// A `<style>` element's text inside an inline `<svg>`, changed so no rule
/// can set the root's font size, which Blitz gives the root from Stylo's
/// computed value. Everything else is copied from the source as written:
/// selectors, values, comments, at-rules and rules that cannot match the
/// root, so escapes and quoting survive.
///
/// Rule structure comes from cssparser, Stylo's tokenizer, and each
/// selector is matched against the root with simplecss, the CSS engine
/// usvg uses. A rule that matches the root and declares `font-size` or
/// `font` is emitted, per matching selector `S` in its list, as two rules
/// with `S`'s specificity, in its place:
///
/// - `S { … }` with the declarations as written, except that `font-size`
///   is dropped and `font` is expanded the way usvg expands it (every
///   longhand reset, then the declared parts set), minus its size. Order
///   and `!important` are kept, so for the root every other property
///   resolves as before.
/// - `* > S { … }` with the original declarations unchanged. It cannot
///   match the root, which has no parent, and for every other element it
///   follows the first rule directly and sets every property that rule
///   does to the same value, plus the size, so their cascade is unchanged.
#[cfg(feature = "svg")]
pub(crate) fn svg_style_sheet_without_root_font_size(css: &str, root: &SvgRoot<'_>) -> String {
    let mut out = String::with_capacity(css.len() + 64);
    for item in css_items(css) {
        match item {
            CssItem::Verbatim(text) => out.push_str(text),
            CssItem::Rule {
                whole,
                prelude,
                body,
            } => {
                let sets_size = simplecss::DeclarationTokenizer::from(body)
                    .any(|d| matches!(d.name, "font-size" | "font"));
                let selectors = kept_selectors(prelude);
                let matches_root =
                    |s: &str| simplecss::Selector::parse(s).is_some_and(|sel| sel.matches(&root));
                if !sets_size || !selectors.iter().any(|s| matches_root(s)) {
                    out.push_str(whole);
                    continue;
                }
                for selector in selectors {
                    if matches_root(selector) {
                        out.push_str(selector);
                        out.push_str(" {");
                        out.push_str(&declarations_without_font_size(body));
                        out.push_str("}\n* > ");
                    }
                    out.push_str(selector);
                    out.push_str(" {");
                    out.push_str(body);
                    out.push_str("}\n");
                }
            }
        }
    }
    out
}

#[cfg(feature = "svg")]
enum CssItem<'a> {
    /// Text copied as written: whitespace, comments, at-rules, and anything
    /// that is not a complete rule.
    Verbatim(&'a str),
    /// A qualified rule: its whole text, the selector list, and the text
    /// between its braces.
    Rule {
        whole: &'a str,
        prelude: &'a str,
        body: &'a str,
    },
}

/// Splits a style sheet into top-level items with cssparser.
#[cfg(feature = "svg")]
fn css_items(css: &str) -> Vec<CssItem<'_>> {
    use cssparser::Token;
    let mut items = Vec::new();
    let mut parser = cssparser::Parser::new(css);
    loop {
        let start = parser.position();
        let mut at_rule = false;
        let mut first = true;
        loop {
            let before = parser.position();
            let token = match parser.next_including_whitespace_and_comments() {
                Ok(token) => token.clone(),
                Err(_) => {
                    let rest = parser.slice_from(start);
                    if !rest.is_empty() {
                        items.push(CssItem::Verbatim(rest));
                    }
                    return items;
                }
            };
            match token {
                Token::WhiteSpace(_) | Token::Comment(_) if first => {
                    // Leading whitespace and comments stand alone.
                    items.push(CssItem::Verbatim(parser.slice_from(start)));
                    break;
                }
                Token::AtKeyword(_) if first => at_rule = true,
                Token::Function(_) | Token::ParenthesisBlock | Token::SquareBracketBlock => {
                    // cssparser skips a block's contents only on the next
                    // read; consume it now so the positions taken before
                    // later tokens fall after its closing bracket.
                    skip_block(&mut parser);
                }
                Token::Semicolon if at_rule => {
                    items.push(CssItem::Verbatim(parser.slice_from(start)));
                    break;
                }
                Token::CurlyBracketBlock => {
                    let prelude = parser.slice(start..before);
                    let body = parser
                        .parse_nested_block(|block| {
                            let inner = block.position();
                            while block.next_including_whitespace_and_comments().is_ok() {}
                            Ok::<_, cssparser::ParseError<()>>(block.slice_from(inner))
                        })
                        .unwrap_or("");
                    let whole = parser.slice_from(start);
                    items.push(if at_rule {
                        CssItem::Verbatim(whole)
                    } else {
                        CssItem::Rule {
                            whole,
                            prelude,
                            body,
                        }
                    });
                    break;
                }
                _ => {}
            }
            first = false;
        }
    }
}

/// Consumes the block whose opening token `parser` just returned.
#[cfg(feature = "svg")]
fn skip_block(parser: &mut cssparser::Parser<'_>) {
    let _ = parser.parse_nested_block(|block| {
        while block.next_including_whitespace_and_comments().is_ok() {}
        Ok::<_, cssparser::ParseError<()>>(())
    });
}

/// The selectors of a selector list that simplecss keeps, as written, in
/// source order.
///
/// simplecss reads a list in source order: it keeps each selector it can
/// parse, and stops at one it cannot unless a comma follows; only then does
/// it sort the sheet by specificity. So each selector is kept exactly when
/// the list up to and including it gives simplecss one more rule than the
/// list before it. Counting is unaffected by the sort, and duplicates count
/// as simplecss counts them.
#[cfg(feature = "svg")]
fn kept_selectors(prelude: &str) -> Vec<&str> {
    let rules = |prefix: &str| {
        simplecss::StyleSheet::parse(&format!("{prefix}{{x:y}}"))
            .rules
            .len()
    };
    let mut kept = Vec::new();
    let mut before = 0;
    for part in top_level_comma_split(prelude) {
        // The prefix ends where the part ends in the source, so simplecss
        // sees exactly the characters it would see in the whole list.
        let end = part.as_ptr() as usize - prelude.as_ptr() as usize + part.len();
        let count = rules(&prelude[..end]);
        if count > before {
            kept.push(trim_css_whitespace(part));
        }
        before = count;
    }
    kept
}

/// The selectors of a selector list as written, split at top-level commas,
/// each with the whitespace around it.
#[cfg(feature = "svg")]
fn top_level_comma_split(prelude: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut parser = cssparser::Parser::new(prelude);
    let mut start = parser.position();
    loop {
        let before = parser.position();
        match parser.next_including_whitespace_and_comments().cloned() {
            Ok(cssparser::Token::Comma) => {
                parts.push(parser.slice(start..before));
                start = parser.position();
            }
            Ok(
                cssparser::Token::Function(_)
                | cssparser::Token::ParenthesisBlock
                | cssparser::Token::SquareBracketBlock
                | cssparser::Token::CurlyBracketBlock,
            ) => skip_block(&mut parser),
            Ok(_) => {}
            Err(_) => {
                parts.push(parser.slice_from(start));
                break;
            }
        }
    }
    parts
}

/// `s` without leading and trailing CSS whitespace: space, tab, line feed,
/// carriage return and form feed. Other Unicode spaces, such as U+00A0, are
/// not CSS whitespace and stay part of the selector.
#[cfg(feature = "svg")]
fn trim_css_whitespace(s: &str) -> &str {
    s.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C'))
}

/// A declaration block for the root: as written, without `font-size`, and
/// with `font` expanded as usvg expands it but without its size. A `font`
/// value usvg cannot parse is left out, as usvg ignores it.
#[cfg(feature = "svg")]
fn declarations_without_font_size(body: &str) -> String {
    let mut out = String::new();
    let mut push = |name: &str, value: &str, important: bool| {
        out.push(' ');
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        if important {
            out.push_str(" !important");
        }
        out.push(';');
    };
    for d in simplecss::DeclarationTokenizer::from(body) {
        match d.name {
            "font-size" => {}
            "font" => {
                let Ok(font) = svgtypes::FontShorthand::from_str(d.value) else {
                    continue;
                };
                for (name, initial) in [
                    ("font-style", "normal"),
                    ("font-variant", "normal"),
                    ("font-weight", "normal"),
                    ("font-stretch", "normal"),
                    ("line-height", "normal"),
                    ("font-size-adjust", "none"),
                    ("font-kerning", "auto"),
                    ("font-variant-caps", "normal"),
                    ("font-variant-ligatures", "normal"),
                    ("font-variant-numeric", "normal"),
                    ("font-variant-east-asian", "normal"),
                    ("font-variant-position", "normal"),
                ] {
                    push(name, initial, d.important);
                }
                for (name, value) in [
                    ("font-stretch", font.font_stretch),
                    ("font-weight", font.font_weight),
                    ("font-variant", font.font_variant),
                    ("font-style", font.font_style),
                ] {
                    if let Some(value) = value {
                        push(name, value, d.important);
                    }
                }
                push("font-family", font.font_family, d.important);
            }
            name => push(name, d.value, d.important),
        }
    }
    out.push(' ');
    out
}
