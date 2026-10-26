//! Serialization of nodes to HTML, and of inline `<svg>` subtrees to SVG source for usvg.

use html_escape::{encode_quoted_attribute_to_string, encode_text_to_string};
use markup5ever::local_name;
use markup5ever::{QualName, ns};
use style_traits::values::ToCss;

use super::{Node, NodeData};

#[derive(Clone, Copy)]
enum OutputStyle {
    Normal,
    Pretty,
}

#[derive(Clone, Copy)]
struct SerializeOpts<'a> {
    style: OutputStyle,
    /// Replace `currentColor` in attribute values with the element's computed `color`
    resolve_current_color: bool,
    /// Declare the `xlink` prefix on the root element so that the output is well-formed XML
    declare_xlink_ns: bool,
    /// For an inline `<svg>`: the root's rewritten `style`, and the root as
    /// `<style>` rules are matched against
    #[cfg(feature = "svg")]
    svg_root: Option<&'a SvgRootOverride<'a>>,
    #[cfg(not(feature = "svg"))]
    svg_root: Option<&'a std::convert::Infallible>,
}

/// How [`Node::svg_source`] makes the root's computed font size the one
/// usvg uses.
#[cfg(feature = "svg")]
struct SvgRootOverride<'a> {
    /// The root's `style` attribute, ending in its computed `font-size`.
    style: String,
    /// The root's serialized attributes, for matching `<style>` rules.
    matcher: crate::util::SvgRoot<'a>,
}

impl SerializeOpts<'_> {
    const fn html(style: OutputStyle) -> Self {
        Self {
            style,
            resolve_current_color: false,
            declare_xlink_ns: false,
            svg_root: None,
        }
    }
}

const XLINK_NS: &str = "http://www.w3.org/1999/xlink";

/// Writes an attribute's serialized name, per the HTML fragment serialization algorithm
fn write_attr_name(name: &QualName, writer: &mut String) {
    match name.ns {
        ns!() | ns!(html) => {}
        ns!(xml) => writer.push_str("xml:"),
        ns!(xmlns) if name.local.as_str() == "xmlns" => {}
        ns!(xmlns) => writer.push_str("xmlns:"),
        ns!(xlink) => writer.push_str("xlink:"),
        _ => {
            if let Some(prefix) = &name.prefix {
                writer.push_str(prefix);
                writer.push(':');
            }
        }
    }
    writer.push_str(&name.local);
}

fn is_xlink_ns_decl(name: &QualName) -> bool {
    (name.ns == ns!(xmlns) && name.local.as_str() == "xlink")
        || name.local.as_str() == "xmlns:xlink"
}

impl Node {
    /// Renders the HTML of this node and all its children as a `String` without extra whitespace.
    ///
    /// Example output:
    ///
    /// ```text
    /// <html><head /><body><main id="main"><div class="arbitrary-class" /></main></body></html>
    /// ```
    pub fn outer_html(&self) -> String {
        let mut output = String::new();
        self.write_outer_html(&mut output);
        output
    }

    /// Renders the HTML of this node and all its children as a `String` with whitespace for human
    /// readability.
    ///
    /// Example output:
    ///
    /// ```text
    /// <html>
    ///   <head />
    ///   <body>
    ///     <main id="main">
    ///       <div class="arbitrary-class" />
    ///     </main>
    ///   </body>
    /// </html>
    /// ```
    pub fn outer_html_pretty(&self) -> String {
        let mut output = String::new();
        self.write_outer_html_pretty(&mut output);
        output
    }

    pub fn write_outer_html(&self, writer: &mut String) {
        self.write_outer_html_in_style(writer, SerializeOpts::html(OutputStyle::Normal), 0);
    }

    pub fn write_outer_html_pretty(&self, writer: &mut String) {
        self.write_outer_html_in_style(writer, SerializeOpts::html(OutputStyle::Pretty), 0);
    }

    /// Serializes this `<svg>` subtree as a standalone SVG document for usvg, with
    /// `currentColor` resolved against each element's computed `color`.
    ///
    /// The root's computed font size `root_font_size`, in CSS pixels, is the
    /// one usvg uses, and only for the root: it ends the root's `style`, the
    /// root's `font-size` and `font` attributes are left out, and `<style>`
    /// rules that would set the root's size are rewritten so they reach only
    /// other elements (see [`crate::util::svg_style_sheet_without_root_font_size`]).
    /// Descendants, nested `<svg>` elements included, keep their cascade and
    /// inherit the computed size.
    #[cfg(feature = "svg")]
    pub(crate) fn svg_source(&self, root_font_size: f32) -> String {
        let mut output = String::new();
        let Some(data) = self.element_data() else {
            return output;
        };
        let current_color = self
            .primary_styles()
            .map(|style| style.clone_color().to_css_string());
        let mut style =
            crate::util::svg_root_style(data.attr(local_name!("style")), root_font_size);
        // Stylo serializes `currentColor` in lowercase.
        if let Some(color) = current_color.as_ref() {
            style = style.replace("currentcolor", color);
        }
        let mut attrs: Vec<(&str, &str)> = data
            .attrs()
            .iter()
            .filter(|a| a.name.ns == ns!() && !is_root_font_attr(&a.name.local))
            .map(|a| (&*a.name.local, a.value.as_str()))
            .collect();
        attrs.push(("style", style.as_str()));
        let root = SvgRootOverride {
            style: style.clone(),
            matcher: crate::util::SvgRoot {
                local_name: &data.name.local,
                attrs,
            },
        };
        let opts = SerializeOpts {
            style: OutputStyle::Normal,
            resolve_current_color: true,
            declare_xlink_ns: true,
            svg_root: Some(&root),
        };
        self.write_outer_html_in_style(&mut output, opts, 0);
        output
    }

    fn write_outer_html_in_style(&self, writer: &mut String, opts: SerializeOpts, nesting: usize) {
        const INDENT: &str = "  ";
        let style = opts.style;
        let has_children = !self.children.is_empty();
        let current_color = opts
            .resolve_current_color
            .then(|| self.primary_styles())
            .flatten()
            .map(|style| style.clone_color())
            .map(|color| color.to_css_string());

        match &self.data {
            NodeData::Document(_) => {}
            NodeData::Comment { .. } => {}
            NodeData::AnonymousBlock(_) => {}
            // NodeData::Doctype { name, .. } => write!(s, "DOCTYPE {name}"),
            NodeData::Text(data) => {
                if matches!(style, OutputStyle::Pretty) {
                    for _ in 0..nesting {
                        writer.push_str(INDENT);
                    }
                }
                let in_raw_text_element = self
                    .parent
                    .and_then(|id| self.tree()[id].data.downcast_element())
                    .is_some_and(|el| {
                        // Documents are parsed with scripting disabled, so `<noscript>` is not raw text
                        el.name.ns == ns!(html)
                            && matches!(
                                el.name.local.as_ref(),
                                "style"
                                    | "script"
                                    | "xmp"
                                    | "iframe"
                                    | "noembed"
                                    | "noframes"
                                    | "plaintext"
                            )
                    });
                if in_raw_text_element {
                    writer.push_str(data.content.as_str());
                } else {
                    encode_text_to_string(data.content.as_str(), writer);
                }
                if matches!(style, OutputStyle::Pretty) {
                    writer.push('\n');
                }
            }
            NodeData::Element(data) => {
                if matches!(style, OutputStyle::Pretty) {
                    for _ in 0..nesting {
                        writer.push_str(INDENT);
                    }
                }
                writer.push('<');
                writer.push_str(&data.name.local);

                if opts.declare_xlink_ns
                    && nesting == 0
                    && !data.attrs().iter().any(|attr| is_xlink_ns_decl(&attr.name))
                {
                    writer.push_str(" xmlns:xlink=\"");
                    writer.push_str(XLINK_NS);
                    writer.push('"');
                }

                #[cfg(feature = "svg")]
                let root = opts.svg_root.filter(|_| nesting == 0);
                #[cfg(not(feature = "svg"))]
                let root: Option<&()> = None;
                for attr in data.attrs() {
                    let name: &str = &attr.name.local;
                    if root.is_some() && attr.name.ns == ns!() && is_root_font_attr(name) {
                        // Written below from the computed size.
                        continue;
                    }
                    writer.push(' ');
                    write_attr_name(&attr.name, writer);
                    writer.push_str("=\"");
                    #[allow(clippy::unnecessary_unwrap)] // Convert to if-let chain once stabilised
                    if current_color.is_some() && attr.value.contains("currentColor") {
                        let value = attr
                            .value
                            .replace("currentColor", current_color.as_ref().unwrap());
                        encode_quoted_attribute_to_string(&value, writer);
                    } else {
                        encode_quoted_attribute_to_string(&attr.value, writer);
                    }
                    writer.push('"');
                }
                #[cfg(feature = "svg")]
                if let Some(root) = root {
                    writer.push_str(" style=\"");
                    encode_quoted_attribute_to_string(&root.style, writer);
                    writer.push('"');
                }
                if !has_children {
                    writer.push_str(" /");
                }
                writer.push('>');
                if matches!(style, OutputStyle::Pretty) {
                    writer.push('\n');
                }

                // A <style> inside an inline svg, as usvg reads one.
                #[cfg(feature = "svg")]
                let rewritten_sheet = opts.svg_root.filter(|_| {
                    nesting > 0
                        && data.name.local == local_name!("style")
                        && data
                            .attr(local_name!("type"))
                            .is_none_or(|t| t == "text/css")
                });
                #[cfg(not(feature = "svg"))]
                let rewritten_sheet: Option<&()> = None;

                if let Some(_root) = rewritten_sheet {
                    #[cfg(feature = "svg")]
                    encode_text_to_string(
                        crate::util::svg_style_sheet_without_root_font_size(
                            &self.text_content(),
                            &_root.matcher,
                        ),
                        writer,
                    );
                } else if has_children {
                    for &child_id in &self.children {
                        self.tree()[child_id].write_outer_html_in_style(writer, opts, nesting + 1);
                    }
                }

                if has_children {
                    if matches!(style, OutputStyle::Pretty) {
                        for _ in 0..nesting {
                            writer.push_str(INDENT);
                        }
                    }
                    writer.push_str("</");
                    writer.push_str(&data.name.local);
                    writer.push('>');
                    if matches!(style, OutputStyle::Pretty) {
                        writer.push('\n');
                    }
                }
            }
        }
    }
}

/// Root attributes whose font size [`Node::svg_source`] replaces.
fn is_root_font_attr(name: &str) -> bool {
    matches!(name, "font-size" | "font" | "style")
}
