use blitz_traits::node_id::NodeId;
use core::str;
use std::sync::Arc;

use markup5ever::{QualName, local_name, ns};
use parley::{
    FontContext, InlineBox, InlineBoxKind, LayoutContext, StyleProperty, TreeBuilder,
    WhiteSpaceCollapse,
};
use style::{
    computed_values::position::T as PositionProperty,
    data::ElementData as StyloElementData,
    shared_lock::StylesheetGuards,
    values::{
        computed::{Content, ContentItem, Display, Float},
        specified::box_::{DisplayInside, DisplayOutside},
    },
};
use thin_vec::ThinVec;

use crate::{
    BaseDocument, ElementData, Node, NodeData,
    layout::damage::{
        CONSTRUCT_BOX, CONSTRUCT_DESCENDENT, CONSTRUCT_FC, sort_layout_children_by_order,
    },
    node::{
        ListItemLayout, ListItemLayoutPosition, Marker, NodeFlags, NodeKind, SpecialElementData,
        TextBrush, TextInputData, TextLayout,
    },
    qual_name, stylo_to_parley,
    traversal::{iter_children, iter_children_and_pseudos},
};

use super::{
    damage::ALL_DAMAGE,
    list::{BULLET_FONT_FAMILY, collect_list_item_children},
    replaced::is_inline_box_element,
    table::build_table_context,
    text_transform::{CaseTransform, TextTransformer},
};

const DUMMY_NAME: QualName = qual_name!("div", html);

#[derive(Clone)]
pub(crate) struct ConstructionTask {
    pub(crate) node_id: NodeId,
    pub(crate) data: ConstructionTaskData,
}

pub(crate) struct ConstructionTaskResult {
    pub(crate) node_id: NodeId,
    pub(crate) data: ConstructionTaskResultData,
}

#[derive(Clone)]
pub(crate) enum ConstructionTaskData {
    InlineLayout(Box<TextLayout>),
}

pub(crate) enum ConstructionTaskResultData {
    InlineLayout(Box<TextLayout>),
}

/// Accumulator threaded through layout-child collection.
///
/// `children` is the list of layout children being built up, and
/// `anonymous_block_id` is the currently-open anonymous block container (if
/// any) that wrapping children are being appended to.
#[derive(Default)]
pub(crate) struct LayoutChildren {
    pub(crate) children: ThinVec<NodeId>,
    pub(crate) anonymous_block_id: Option<NodeId>,
    /// All anonymous blocks created while collecting these layout children.
    ///
    /// These are recorded on the container node so they can be deallocated the
    /// next time it is reconstructed.
    pub(crate) anonymous_blocks: ThinVec<NodeId>,
}

impl LayoutChildren {
    /// Append a single layout child.
    fn push(&mut self, child_id: NodeId, doc: &mut BaseDocument) {
        self.maybe_push_anon_block(doc);
        self.children.push(child_id);
    }

    /// Append all layout children in `slice`.
    fn extend(&mut self, slice: &[NodeId], doc: &mut BaseDocument) {
        self.maybe_push_anon_block(doc);
        self.children.extend_from_slice(slice);
    }

    fn maybe_push_anon_block(&mut self, doc: &mut BaseDocument) {
        fn block_is_only_whitespace(doc: &BaseDocument, node_id: NodeId) -> bool {
            for child_id in doc.nodes[node_id].children.iter().copied() {
                let child = &doc.nodes[child_id];
                if !child.is_collapsible_whitespace_node() {
                    return false;
                }
            }

            true
        }

        // If anonymous block node only contains whitespace then delete it
        if let Some(anon_id) = self.anonymous_block_id {
            if block_is_only_whitespace(doc, anon_id) {
                // Remove by identity, not pop(): hoisted display:contents
                // children may have been pushed after the anon block.
                if let Some(pos) = self.children.iter().rposition(|id| *id == anon_id) {
                    self.children.remove(pos);
                }
                self.anonymous_blocks.retain(|id| *id != anon_id);
                doc.remove_node_from_tree(anon_id);
            }
        }

        self.anonymous_block_id = None;
    }

    fn push_wrapped(
        &mut self,
        container_node_id: NodeId,
        child_id: NodeId,
        doc: &mut BaseDocument,
    ) {
        if self.anonymous_block_id.is_none() {
            self.create_anonymous_block(container_node_id, doc);
        }
        doc.nodes[self.anonymous_block_id.unwrap()]
            .children
            .push(child_id);
    }

    fn create_anonymous_block(&mut self, container_node_id: NodeId, doc: &mut BaseDocument) {
        use style::selector_parser::PseudoElement;

        const NAME: QualName = QualName {
            prefix: None,
            ns: ns!(html),
            local: local_name!("div"),
        };
        let node_id = doc.create_node(NodeData::AnonymousBlock(Box::new(ElementData::new(
            NAME,
            Vec::new(),
        ))));

        // Set style data
        let parent_style = doc.nodes[container_node_id].primary_styles().unwrap();
        let read_guard = doc.guard.read();
        let guards = StylesheetGuards::same(&read_guard);
        let style = doc.stylist.style_for_anonymous::<&Node>(
            &guards,
            &PseudoElement::ServoAnonymousBox,
            &parent_style,
        );
        let mut stylo_element_data = StyloElementData {
            damage: ALL_DAMAGE,
            ..Default::default()
        };
        drop(parent_style);

        stylo_element_data.styles.primary = Some(style);
        stylo_element_data.set_restyled();

        *doc.nodes[node_id]
            .stylo_element_data_mut()
            .ensure_init_mut() = stylo_element_data;

        if doc.nodes[container_node_id]
            .flags
            .contains(NodeFlags::IS_IN_DOCUMENT)
        {
            doc.nodes[node_id].flags.insert(NodeFlags::IS_IN_DOCUMENT);
        }
        doc.nodes[node_id].parent = Some(container_node_id);
        doc.nodes[node_id]
            .layout_parent
            .set(Some(container_node_id));

        self.children.push(node_id);
        self.anonymous_block_id = Some(node_id);
        self.anonymous_blocks.push(node_id);
    }
}

fn push_children_and_pseudos(layout_children: &mut ThinVec<NodeId>, node: &Node) {
    if let Some(before) = node.before() {
        layout_children.push(before);
    }
    layout_children.extend(node.children.iter().copied().filter(|child_id| {
        let child_node = node.with(*child_id);
        child_node.data.kind() != NodeKind::Comment
    }));
    if let Some(after) = node.after() {
        layout_children.push(after);
    }
}

/// The nearest ancestor of `node_id` that is not `display: contents`, whose
/// span the text of a contents element is painted with.
fn nearest_boxed_ancestor(nodes: &crate::NodeTree, node_id: NodeId) -> NodeId {
    let mut id = node_id;
    while let Some(parent) = nodes[id].parent {
        id = parent;
        let contents = nodes[id]
            .display_style()
            .is_some_and(|d| matches!(d.inside(), DisplayInside::Contents));
        if !contents {
            break;
        }
    }
    id
}

/// Wrapping policy of the nearest non-contents ancestor container. Children
/// hoisted out of display:contents nodes must be wrapped in anonymous blocks
/// exactly as if they were direct children of that container: without this,
/// a bare text node could end up as the layout child of a block/flex/grid
/// container, which cannot be laid out (text nodes carry no style).
#[derive(Copy, Clone)]
struct WrapContext {
    /// The container whose style anonymous blocks inherit from.
    container_node_id: NodeId,
    needs_wrap: fn(NodeKind, DisplayOutside) -> bool,
}

fn block_item_needs_wrap(child_node_kind: NodeKind, display_outside: DisplayOutside) -> bool {
    child_node_kind == NodeKind::Text || display_outside == DisplayOutside::Inline
}

/// Used for flex/grid containers, and for flow containers whose in-flow
/// children are all out-of-flow (where inline elements are pushed raw, but
/// bare text still cannot be laid out directly).
fn text_item_needs_wrap(child_node_kind: NodeKind, _display_outside: DisplayOutside) -> bool {
    child_node_kind == NodeKind::Text
}

/// Push a single hoisted child, recursing through display:contents nodes and
/// wrapping text/inline children per the ancestor container's `WrapContext`.
fn push_hoisted_child(
    doc: &mut BaseDocument,
    child_id: NodeId,
    out: &mut LayoutChildren,
    wrap: Option<WrapContext>,
) {
    let child = &doc.nodes[child_id];
    let child_display = child.display_style().unwrap_or(Display::inline());
    if matches!(child_display.inside(), DisplayInside::Contents) {
        collect_layout_children_with_wrap(doc, child_id, out, wrap);
        return;
    }
    if let Some(wrap_ctx) = wrap {
        let child_node_kind = child.data.kind();
        let display_outside = if child.is_or_contains_block() {
            DisplayOutside::Block
        } else {
            child_display.outside()
        };
        if (wrap_ctx.needs_wrap)(child_node_kind, display_outside) {
            out.push_wrapped(wrap_ctx.container_node_id, child_id, doc);
            return;
        }
    }
    out.push(child_id, doc);
}

/// Push the container's children (and ::before/::after pseudos) as layout
/// children, hoisting transparently through display:contents nodes and
/// filtering out comments and whitespace.
fn push_hoisted_children_and_pseudos(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    out: &mut LayoutChildren,
    wrap: Option<WrapContext>,
) {
    if let Some(before) = doc.nodes[container_node_id].before() {
        push_hoisted_child(doc, before, out, wrap);
    }
    // Take children array from node to avoid borrow checker issues.
    let children = std::mem::take(&mut doc.nodes[container_node_id].children);
    for child_id in children.iter().copied() {
        let child = &doc.nodes[child_id];
        if child.data.kind() == NodeKind::Comment || child.is_collapsible_whitespace_node() {
            continue;
        }
        push_hoisted_child(doc, child_id, out, wrap);
    }
    doc.nodes[container_node_id].children = children;
    if let Some(after) = doc.nodes[container_node_id].after() {
        push_hoisted_child(doc, after, out, wrap);
    }
}

fn push_non_whitespace_children_and_pseudos(layout_children: &mut ThinVec<NodeId>, node: &Node) {
    if let Some(before) = node.before() {
        layout_children.push(before);
    }
    layout_children.extend(node.children.iter().copied().filter(|child_id| {
        let child_node = node.with(*child_id);
        !child_node.is_whitespace_node() && child_node.data.kind() != NodeKind::Comment
    }));
    if let Some(after) = node.after() {
        layout_children.push(after);
    }
}

/// Result of classifying the in-flow children of a flow container as
/// all-block, all-inline and/or all-out-of-flow.
struct FlowClassification {
    all_block: bool,
    all_inline: bool,
    all_out_of_flow: bool,
    has_contents: bool,
}

impl Default for FlowClassification {
    fn default() -> Self {
        Self {
            all_block: true,
            all_inline: true,
            all_out_of_flow: true,
            has_contents: false,
        }
    }
}

/// Classify a flow container's children (including its ::before/::after
/// pseudo-elements) for inline-vs-block layout, recursing transparently
/// through display:contents nodes (whose children participate in the
/// container's formatting context).
fn classify_flow_children(
    doc: &BaseDocument,
    container_node_id: NodeId,
    classification: &mut FlowClassification,
) {
    let node = &doc.nodes[container_node_id];
    // ::before/::after pseudos with display:contents are transparent for box
    // generation, so their children (e.g. generated text) participate in the
    // container's formatting context and vote in the classification. Pseudos
    // with any other display value generate their own box, which every
    // construction arm already pushes explicitly, so they cast no vote.
    let pseudo_ids = node
        .before()
        .into_iter()
        .chain(node.after())
        .filter(|pe_id| {
            let display = doc.nodes[*pe_id]
                .display_style()
                .unwrap_or(Display::inline());
            matches!(display.inside(), DisplayInside::Contents)
        });
    let child_ids = node.children.iter().copied().chain(pseudo_ids);
    for child_id in child_ids {
        let child = &doc.nodes[child_id];

        // Comment nodes generate no boxes and must not affect the
        // inline-vs-block classification: an unstyled comment would
        // default to display:inline below and force an inline
        // formatting context on the container, swallowing element
        // siblings into the inline layout (zero-sizing any
        // out-of-flow ones).
        if child.data.kind() == NodeKind::Comment {
            continue;
        }

        // Unwraps on Text and SVG nodes
        let style = child.primary_styles();
        let style = style.as_ref();
        let display = child.display_style().unwrap_or(Display::inline());
        if matches!(display.inside(), DisplayInside::Contents) {
            // Transparent for box generation: the contents node casts
            // no vote itself — its children decide.
            classification.has_contents = true;
            classify_flow_children(doc, child_id, classification);
        } else if matches!(display.inside(), DisplayInside::None) {
            // display:none children generate no boxes and cast no vote.
            continue;
        } else {
            let position = style
                .map(|s| s.clone_position())
                .unwrap_or(PositionProperty::Static);
            let float = style.map(|s| s.clone_float()).unwrap_or(Float::None);

            // Ignore nodes whose whitespace is entirely collapsed away
            if child.is_collapsible_whitespace_node() {
                continue;
            }

            // display:none children generate no boxes and cast no vote
            if matches!(display.outside(), DisplayOutside::None) {
                continue;
            }

            let is_in_flow = matches!(
                position,
                PositionProperty::Static | PositionProperty::Relative | PositionProperty::Sticky
            ) && matches!(float, Float::None);

            if !is_in_flow {
                continue;
            }

            classification.all_out_of_flow = false;
            match display.outside() {
                DisplayOutside::None => {}
                DisplayOutside::Block
                | DisplayOutside::TableCaption
                | DisplayOutside::InternalTable => classification.all_inline = false,
                DisplayOutside::Inline => {
                    classification.all_block = false;

                    // We need the "complex" tree fixing when an inline contains a block
                    if child.is_or_contains_block() {
                        classification.all_inline = false;
                    }
                }
            }
        }
    }
}

pub(crate) fn collect_layout_children(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    out: &mut LayoutChildren,
) {
    collect_layout_children_with_wrap(doc, container_node_id, out, None)
}

fn collect_layout_children_with_wrap(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    out: &mut LayoutChildren,
    wrap: Option<WrapContext>,
) {
    // Record the `display` the box is being constructed with
    if let Some(display) = doc.nodes[container_node_id].display_style() {
        *doc.nodes[container_node_id].display_constructed_as_mut() = display;
    }

    // Reset construction flags
    // TODO: make incremental and only remove this if the element is no longer an inline root
    doc.nodes[container_node_id]
        .flags
        .reset_construction_flags();
    if let Some(element_data) = doc.nodes[container_node_id].element_data_mut() {
        element_data.take_inline_layout();
    }

    flush_pseudo_elements(doc, container_node_id);

    if let Some(el) = doc.nodes[container_node_id].data.downcast_element() {
        // Handle text inputs
        let tag_name = el.name.local.as_ref();
        if matches!(tag_name, "input" | "textarea") {
            let type_attr: Option<&str> = doc.nodes[container_node_id]
                .data
                .downcast_element()
                .and_then(|el| el.attr(local_name!("type")));
            if tag_name == "textarea" {
                create_text_editor(doc, container_node_id, true);
                return;
            } else if matches!(
                type_attr,
                None | Some("text" | "password" | "email" | "number" | "search" | "tel" | "url")
            ) {
                create_text_editor(doc, container_node_id, false);
                return;
            } else if matches!(type_attr, Some("checkbox" | "radio")) {
                create_checkbox_input(doc, container_node_id);
                return;
            }
        }

        #[cfg(feature = "svg")]
        if matches!(tag_name, "svg") {
            // `em` lengths inside the SVG resolve against the root's computed size.
            let font_size = doc.nodes[container_node_id]
                .primary_styles()
                .map_or(crate::util::SVG_IMAGE_FONT_SIZE, |style| {
                    style.get_font().font_size.used_size.0.px()
                });
            let outer_html = doc
                .get_node(container_node_id)
                .unwrap()
                .svg_source(font_size);

            // Remove contruction damage from subtree
            doc.iter_subtree_mut(container_node_id, |id: NodeId, doc: &mut BaseDocument| {
                doc.nodes[id].remove_damage(CONSTRUCT_BOX | CONSTRUCT_DESCENDENT | CONSTRUCT_FC);
            });

            match crate::util::parse_svg_image(outer_html.as_bytes(), font_size, None) {
                Ok(svg) => {
                    doc.get_node_mut(container_node_id)
                        .unwrap()
                        .element_data_mut()
                        .unwrap()
                        .special_data =
                        SpecialElementData::Image(Box::new(crate::node::ImageData::Svg(svg)));
                }
                Err(err) => {
                    #[cfg(feature = "tracing")]
                    tracing::warn!(
                        node_id = ?container_node_id,
                        html = outer_html,
                        error = ?err,
                        "SVG parse failed",
                    );
                    #[cfg(not(feature = "tracing"))]
                    let _ = err;
                }
            };
            return;
        }

        //Only ol tags have start and reversed attributes
        let (mut index, reversed) = if tag_name == "ol" {
            (
                el.attr_parsed(local_name!("start"))
                    .map(|start: usize| start - 1)
                    .unwrap_or(0),
                el.attr_parsed(local_name!("reversed")).unwrap_or(false),
            )
        } else {
            (0, false)
        };
        collect_list_item_children(doc, &mut index, reversed, container_node_id);
    }

    // Skip further construction if the node has no children or psuedo-children
    {
        let node = &doc.nodes[container_node_id];
        if node.children.is_empty() && node.before().is_none() && node.after().is_none() {
            return;
        }
    }

    let container_display = doc.nodes[container_node_id].display_style().unwrap_or(
        match doc.nodes[container_node_id].data.kind() {
            NodeKind::AnonymousBlock => Display::Block,
            _ => Display::Inline,
        },
    );

    match container_display.inside() {
        DisplayInside::None => {}
        DisplayInside::Contents => {
            doc.nodes[container_node_id]
                .remove_damage(CONSTRUCT_BOX | CONSTRUCT_DESCENDENT | CONSTRUCT_FC);
            // display:contents is transparent for box generation: hoist the
            // children THEMSELVES (not their layout children) into the
            // parent, recursing only through nested contents nodes.
            push_hoisted_children_and_pseudos(doc, container_node_id, out, wrap);
        }
        DisplayInside::Flex | DisplayInside::Grid => {
            // ::before/::after pseudos must be checked too: a pseudo with
            // display:contents hoists its text content into the container.
            let container = &doc.nodes[container_node_id];
            let has_text_node_or_contents = container
                .children
                .iter()
                .copied()
                .chain(container.before())
                .chain(container.after())
                .map(|child_id| &doc.nodes[child_id])
                .any(|child| {
                    let display = child.display_style().unwrap_or(Display::inline());
                    let node_kind = child.data.kind();
                    display.inside() == DisplayInside::Contents || node_kind == NodeKind::Text
                });

            if !has_text_node_or_contents {
                push_non_whitespace_children_and_pseudos(
                    &mut out.children,
                    &doc.nodes[container_node_id],
                );
            } else {
                collect_complex_layout_children(
                    doc,
                    container_node_id,
                    out,
                    true,
                    text_item_needs_wrap,
                );
            }

            // Flex/grid items are laid out in order-modified document order.
            sort_layout_children_by_order(&doc.nodes, &mut out.children);
        }

        DisplayInside::Table => {
            let (table_context, tlayout_children) = build_table_context(doc, container_node_id);
            #[allow(clippy::arc_with_non_send_sync)]
            let data = SpecialElementData::TableRoot(Arc::new(table_context));
            doc.nodes[container_node_id]
                .flags
                .insert(NodeFlags::IS_TABLE_ROOT);
            doc.nodes[container_node_id]
                .data
                .downcast_element_mut()
                .unwrap()
                .special_data = data;
            if let Some(before) = doc.nodes[container_node_id].before() {
                out.push(before, doc);
            }
            out.extend(&tlayout_children, doc);
            if let Some(after) = doc.nodes[container_node_id].after() {
                out.push(after, doc);
            }
        }

        // Flow, FlowRoot and TableCell, plus internal table displays (row,
        // row group, column, ...) occurring outside of a table. Blitz does
        // not yet generate anonymous table wrapper boxes for the latter, so
        // they are laid out as flow containers, which crucially ensures
        // their text children get wrapped rather than being pushed as bare
        // layout children (text nodes carry no style and cannot be laid out
        // as block/flex/grid items).
        _ => {
            let mut classification = FlowClassification::default();
            classify_flow_children(doc, container_node_id, &mut classification);

            if classification.all_out_of_flow {
                // Contents-transparent: a display:contents child may be
                // holding the out-of-flow elements (otherwise the contents
                // node itself would be pushed as a layout box).
                let wrap = Some(WrapContext {
                    container_node_id,
                    needs_wrap: text_item_needs_wrap,
                });
                return push_hoisted_children_and_pseudos(doc, container_node_id, out, wrap);
            }

            // TODO: fix display:contents
            if classification.all_inline {
                let existing_layout = doc.nodes[container_node_id]
                    .element_data_mut()
                    .and_then(|el| el.inline_layout_data.take());
                let layout = existing_layout.unwrap_or_else(|| Box::new(TextLayout::new()));

                // Queue node for inline layout construction. Deferring construction of inline layouts to a
                // dedicated phase allows us to multithread the expensive text shaping step.
                doc.deferred_construction_nodes.push(ConstructionTask {
                    node_id: container_node_id,
                    data: ConstructionTaskData::InlineLayout(layout),
                });
                doc.nodes[container_node_id]
                    .flags
                    .insert(NodeFlags::IS_INLINE_ROOT);

                find_inline_layout_embedded_boxes(doc, container_node_id, &mut out.children);
                return;
            }

            // If the children are either all inline or all block then simply return the regular children
            // as the layout children
            if classification.all_block & !classification.has_contents {
                return push_non_whitespace_children_and_pseudos(
                    &mut out.children,
                    &doc.nodes[container_node_id],
                );
            } else if classification.all_inline & !classification.has_contents {
                return push_children_and_pseudos(&mut out.children, &doc.nodes[container_node_id]);
            }

            collect_complex_layout_children(
                doc,
                container_node_id,
                out,
                false,
                block_item_needs_wrap,
            );
        }
    }
}

/// Extract the text generated by a pseudo-element's `content` property
/// (only string content items are currently supported; other item types
/// are skipped).
fn pe_content_text(style: &style::properties::ComputedValues) -> Option<String> {
    match &style.get_counters().content {
        Content::Items(item_data) => {
            let items = &item_data.items[0..item_data.alt_start];
            let mut text = String::new();
            let mut has_string = false;
            for item in items {
                // TODO: other types of content
                if let ContentItem::String(owned_str) = item {
                    text.push_str(owned_str);
                    has_string = true;
                }
            }
            has_string.then_some(text)
        }
        _ => None,
    }
}

fn flush_pseudo_elements(doc: &mut BaseDocument, node_id: NodeId) {
    let (before_style, after_style, before_node_id, after_node_id) = {
        let node = &doc.nodes[node_id];

        let before_node_id = node.before();
        let after_node_id = node.after();

        // Note: yes these are kinda backwards
        let style_data = node.try_stylo_element_data().and_then(|s| s.get());
        let before_style = style_data
            .as_ref()
            .and_then(|d| d.styles.pseudos.as_array()[1].clone());
        let after_style = style_data
            .as_ref()
            .and_then(|d| d.styles.pseudos.as_array()[0].clone());

        (before_style, after_style, before_node_id, after_node_id)
    };

    // Sync pseudo element
    // TODO: Make incremental
    for (idx, pe_style, pe_node_id) in [
        (1, before_style, before_node_id),
        (0, after_style, after_node_id),
    ] {
        // Delete psuedo element if it exists but shouldn't
        if let (Some(pe_node_id), None) = (pe_node_id, &pe_style) {
            doc.remove_and_drop_pe(pe_node_id);
            let node = &mut doc.nodes[node_id];
            node.set_pe_by_index(idx, None);
            node.insert_damage(ALL_DAMAGE);
        }

        // Create pseudo element if it should exist but doesn't
        if let (None, Some(pe_style)) = (pe_node_id, &pe_style) {
            let new_node_id = doc.create_node(NodeData::AnonymousBlock(Box::new(
                ElementData::new(DUMMY_NAME, Vec::new()),
            )));
            doc.nodes[new_node_id].parent = Some(node_id);
            doc.nodes[new_node_id].layout_parent.set(Some(node_id));
            if doc.nodes[node_id].flags.contains(NodeFlags::IS_IN_DOCUMENT) {
                doc.nodes[new_node_id]
                    .flags
                    .insert(NodeFlags::IS_IN_DOCUMENT);
            }

            if let Some(text) = pe_content_text(pe_style) {
                let text_node_id = doc.create_text_node(&text);
                doc.nodes[text_node_id].parent = Some(new_node_id);
                doc.nodes[new_node_id].children.push(text_node_id);
            }

            let mut element_data = StyloElementData::default();
            element_data.styles.primary = Some(pe_style.clone());
            element_data.set_restyled();
            element_data.damage = ALL_DAMAGE;
            *doc.nodes[new_node_id]
                .stylo_element_data_mut()
                .ensure_init_mut() = element_data;

            let node = &mut doc.nodes[node_id];
            node.set_pe_by_index(idx, Some(new_node_id));
            node.insert_damage(ALL_DAMAGE);

            doc.pending_style_image_nodes.push(new_node_id);
        }

        // Else: Update psuedo element
        if let (Some(pe_node_id), Some(pe_style)) = (pe_node_id, pe_style) {
            // Sync the pseudo-element's text node with its `content` style, which
            // may have changed (e.g. `details[open] summary::after { content: ... }`).
            //
            // Note: this deliberately compares the text itself rather than relying on
            // the style-pointer comparison below, as the pseudo-element's style may
            // already have been updated by `sync_pseudo_element_styles` during the
            // style traversal without the text having been updated.
            let new_text = pe_content_text(&pe_style);
            let existing_text_node_id = doc.nodes[pe_node_id]
                .children
                .first()
                .copied()
                .filter(|&child_id| doc.nodes[child_id].is_text_node());
            match (existing_text_node_id, new_text) {
                (Some(text_node_id), Some(new_text)) => {
                    let text_data = doc.nodes[text_node_id].text_data_mut().unwrap();
                    if text_data.content != new_text {
                        text_data.content = new_text;
                        doc.nodes[node_id].insert_damage(ALL_DAMAGE);
                    }
                }
                (None, Some(new_text)) => {
                    let text_node_id = doc.create_text_node(&new_text);
                    doc.nodes[text_node_id].parent = Some(pe_node_id);
                    doc.nodes[pe_node_id].children.push(text_node_id);
                    doc.nodes[node_id].insert_damage(ALL_DAMAGE);
                }
                (Some(text_node_id), None) => {
                    doc.nodes[pe_node_id]
                        .children
                        .retain(|&child_id| child_id != text_node_id);
                    doc.remove_node_from_tree(text_node_id);
                    doc.nodes[node_id].insert_damage(ALL_DAMAGE);
                }
                (None, None) => {}
            }

            let mut node_styles = doc.nodes[pe_node_id]
                .try_stylo_element_data_mut()
                .and_then(|s| s.get_mut());
            let node_styles = &mut node_styles.as_mut().unwrap();
            node_styles.damage.insert(ALL_DAMAGE);
            let primary_styles = &mut node_styles.styles.primary;

            if !std::ptr::eq(&**primary_styles.as_ref().unwrap(), &*pe_style) {
                *primary_styles = Some(pe_style);
                node_styles.set_restyled();
                doc.pending_style_image_nodes.push(pe_node_id);
            }
        }
    }
}

/// Handles the cases where there are text nodes or inline nodes that need to be wrapped in an anonymous block node
fn collect_complex_layout_children(
    doc: &mut BaseDocument,
    container_node_id: NodeId,
    out: &mut LayoutChildren,
    hide_whitespace: bool,
    needs_wrap: fn(NodeKind, DisplayOutside) -> bool,
) {
    doc.iter_children_and_pseudos_mut(container_node_id, |child_id, doc| {
        // Get node kind (text, element, comment, etc)
        let child_node_kind = doc.nodes[child_id].data.kind();

        // Get Display style. Default to inline because nodes without styles are probably text nodes
        let contains_block = doc.nodes[child_id].is_or_contains_block();
        let child_display = &doc.nodes[child_id]
            .display_style()
            .unwrap_or(Display::inline());
        let display_inside = child_display.inside();
        let display_outside = if contains_block {
            DisplayOutside::Block
        } else {
            child_display.outside()
        };

        let is_whitespace_node = doc.nodes[child_id].is_whitespace_node();

        // Skip comment nodes. Note that we do *not* skip `Display::None` nodes as they may need to be hidden.
        // Taffy knows how to deal with `Display::None` children.
        //
        // Also hide all-whitespace flexbox children as these should be ignored
        if child_node_kind == NodeKind::Comment || (hide_whitespace && is_whitespace_node) {
            // return;
        }
        // Recurse into `Display::Contents` nodes, wrapping hoisted text and
        // inline children as if they were direct children of this container
        else if display_inside == DisplayInside::Contents {
            let wrap = Some(WrapContext {
                container_node_id,
                needs_wrap,
            });
            collect_layout_children_with_wrap(doc, child_id, out, wrap)
        }
        // Push nodes that need wrapping into the current "anonymous block container".
        // If there is not an open one then we create one.
        else if needs_wrap(child_node_kind, display_outside) {
            out.push_wrapped(container_node_id, child_id, doc);
        }
        // Else push the child directly (and close any open "anonymous block container")
        else {
            out.push(child_id, doc);
        }
    });

    // If anonymous block node only contains whitespace then delete it, else push it
    out.maybe_push_anon_block(doc);
}

fn create_text_editor(doc: &mut BaseDocument, input_element_id: NodeId, is_multiline: bool) {
    let node = &mut doc.nodes[input_element_id];
    let parley_style = node
        .primary_styles()
        .as_ref()
        .map(|s| stylo_to_parley::style(node.id, s))
        .unwrap_or_default();
    let alignment = node
        .primary_styles()
        .map(|s| stylo_to_parley::text_align(s.clone_text_align()))
        .unwrap_or(parley::layout::Alignment::Start);
    let base_direction = node
        .primary_styles()
        .map(|s| stylo_to_parley::base_direction(s.clone_direction(), s.clone_unicode_bidi()))
        .unwrap_or(parley::BaseDirection::Auto);

    // A textarea's text is its children in HTML, but a framework that
    // controls it (Dioxus sets `value`) has usually set the attribute
    // before the editor exists, so that wins when present.
    let initial_text = if is_multiline {
        node.attr(local_name!("value"))
            .map(str::to_string)
            .unwrap_or_else(|| node.text_content())
    } else {
        node.attr(local_name!("value")).unwrap_or("").to_string()
    };
    let element = &mut node.data.downcast_element_mut().unwrap();
    if !matches!(element.special_data, SpecialElementData::TextInput(_)) {
        let mut text_input_data = TextInputData::new(is_multiline);
        let editor = &mut text_input_data.editor;
        editor.set_text(&initial_text);
        if !is_multiline && element.attr(local_name!("type")) == Some("password") {
            text_input_data.mask_new();
        }
        element.special_data = SpecialElementData::TextInput(text_input_data);
    }

    let SpecialElementData::TextInput(text_input_data) = &mut element.special_data else {
        unreachable!();
    };

    let editor = &mut text_input_data.editor;
    editor.set_scale(doc.viewport.scale_f64() as f32);
    editor.set_width(None);

    let styles = editor.edit_styles();
    styles.retain(|_| false);
    styles.insert(StyleProperty::FontFamily(parley_style.font_family));
    styles.insert(StyleProperty::FontSize(parley_style.font_size));
    styles.insert(StyleProperty::FontWidth(parley_style.font_width));
    styles.insert(StyleProperty::FontStyle(parley_style.font_style));
    styles.insert(StyleProperty::FontWeight(parley_style.font_weight));
    styles.insert(StyleProperty::FontVariations(parley_style.font_variations));
    styles.insert(StyleProperty::FontFeatures(parley_style.font_features));
    styles.insert(StyleProperty::LineHeight(parley_style.line_height));
    styles.insert(StyleProperty::WordSpacing(parley_style.word_spacing));
    styles.insert(StyleProperty::LetterSpacing(parley_style.letter_spacing));
    styles.insert(StyleProperty::WordBreak(parley_style.word_break));
    styles.insert(StyleProperty::OverflowWrap(parley_style.overflow_wrap));
    styles.insert(StyleProperty::TextWrapMode(parley_style.text_wrap_mode));
    styles.insert(StyleProperty::WhiteSpaceCollapse(
        parley_style.white_space_collapse,
    ));
    styles.insert(StyleProperty::Brush(parley_style.brush));
    editor.set_alignment(alignment);
    editor.set_base_direction(base_direction);

    editor.refresh_layout(&mut doc.font_ctx.lock().unwrap(), &mut doc.layout_ctx);
}

fn create_checkbox_input(doc: &mut BaseDocument, input_element_id: NodeId) {
    let node = &mut doc.nodes[input_element_id];

    let element = &mut node.data.downcast_element_mut().unwrap();
    if !matches!(element.special_data, SpecialElementData::CheckboxInput(_)) {
        let checked = element.has_attr(local_name!("checked"));
        element.special_data = SpecialElementData::CheckboxInput(checked);
        element.set_checkbox_input_checked(checked);
    }
}

/// Find and return the "layout_children" (inline boxes) for an inline layout
/// without actually constructing the layout. This allows us to defer the expensive
/// construction of the Parley layout (which invokes text shaping) to a paralell phase.
pub(crate) fn find_inline_layout_embedded_boxes(
    doc: &mut BaseDocument,
    inline_context_root_node_id: NodeId,
    layout_children: &mut ThinVec<NodeId>,
) {
    flush_inline_pseudos_recursive(doc, inline_context_root_node_id);

    iter_children_and_pseudos!(doc.nodes[inline_context_root_node_id], |child_id| {
        find_inline_layout_embedded_boxes_recursive(
            &mut doc.nodes,
            inline_context_root_node_id,
            child_id,
            layout_children,
        );
    });

    fn flush_inline_pseudos_recursive(doc: &mut BaseDocument, node_id: NodeId) {
        doc.iter_children_mut(node_id, |child_id, doc| {
            flush_pseudo_elements(doc, child_id);
            let display = doc.nodes[node_id]
                .display_style()
                .unwrap_or(Display::inline());
            let do_recurse = match (display.outside(), display.inside()) {
                (DisplayOutside::None, DisplayInside::Contents) => true,
                (DisplayOutside::Inline, DisplayInside::Flow) => true,
                (_, _) => false,
            };
            if do_recurse {
                flush_inline_pseudos_recursive(doc, child_id);
            }
        });
    }

    fn find_inline_layout_embedded_boxes_recursive(
        nodes: &mut crate::NodeTree,
        parent_id: NodeId,
        node_id: NodeId,
        layout_children: &mut ThinVec<NodeId>,
    ) {
        let node = &mut nodes[node_id];

        // Set layout_parent for node.
        node.layout_parent.set(Some(parent_id));

        match &node.data {
            NodeData::Element(element_data) | NodeData::AnonymousBlock(element_data) => {
                // if the input type is hidden, hide it
                if *element_data.name.local == *"input" {
                    if let Some("hidden") = element_data.attr(local_name!("type")) {
                        return;
                    }
                }

                let display = node.display_style().unwrap_or(Display::inline());

                match (display.outside(), display.inside()) {
                    (DisplayOutside::None, DisplayInside::None) => {
                        node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                    }
                    (DisplayOutside::None, DisplayInside::Contents) => {
                        node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                        iter_children!(nodes[node_id], |child_id| {
                            find_inline_layout_embedded_boxes_recursive(
                                nodes,
                                parent_id,
                                child_id,
                                layout_children,
                            );
                        });
                    }
                    (DisplayOutside::Inline, DisplayInside::Flow) => {
                        let tag_name = &element_data.name.local;

                        if is_inline_box_element(tag_name) {
                            layout_children.push(node_id);
                        } else if *tag_name == local_name!("br") {
                            node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                        } else {
                            node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                            iter_children_and_pseudos!(nodes[node_id], |child_id| {
                                find_inline_layout_embedded_boxes_recursive(
                                    nodes,
                                    node_id,
                                    child_id,
                                    layout_children,
                                );
                            });
                        }
                    }
                    // Inline box
                    (_, _) => {
                        layout_children.push(node_id);
                    }
                };
            }
            NodeData::Comment { .. } | NodeData::Text(_) => {
                node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
            }
            NodeData::Document(_) => unreachable!(),
        }
    }
}

pub(crate) fn build_inline_layout_into(
    nodes: &crate::NodeTree,
    layout_ctx: &mut LayoutContext<TextBrush>,
    font_ctx: &mut FontContext,
    text_layout: &mut TextLayout,
    scale: f32,
    inline_context_root_node_id: NodeId,
) {
    // Get the inline context's root node's text styles
    let root_node = &nodes[inline_context_root_node_id];
    let root_node_style = root_node.primary_styles().or_else(|| {
        root_node
            .parent
            .and_then(|parent_id| nodes[parent_id].primary_styles())
    });

    let parley_style = root_node_style
        .as_ref()
        .map(|s| stylo_to_parley::style(inline_context_root_node_id, s))
        .unwrap_or_else(|| parley::TextStyle {
            white_space_collapse: WhiteSpaceCollapse::Collapse,
            ..Default::default()
        });

    // Create a parley tree builder
    let mut builder = layout_ctx.tree_builder(font_ctx, scale, true, &parley_style);
    if let Some(style) = root_node_style.as_deref() {
        builder.set_base_direction(stylo_to_parley::base_direction(
            style.clone_direction(),
            style.clone_unicode_bidi(),
        ));
    }

    let text_transform = root_node_style
        .as_deref()
        .map(|s| CaseTransform::from_style(s))
        .unwrap_or(CaseTransform::NONE);

    // Render position-inside list items
    if let Some(ListItemLayout {
        marker,
        position: ListItemLayoutPosition::Inside,
    }) = root_node
        .element_data()
        .and_then(|el| el.list_item_data.as_deref())
    {
        match marker {
            // Bullet glyphs live in the bundled bullet font. The position-outside
            // path already asks for it; without the same span here a marker like
            // disclosure-closed (U+25B8) falls back to the element's own font and
            // renders as a missing glyph.
            Marker::Char(char) => {
                let mut marker_style = parley_style.clone();
                marker_style.font_family = BULLET_FONT_FAMILY.into();
                builder.push_style_span(marker_style);
                builder.push_text(&format!("{char} "));
                builder.pop_style_span();
            }
            Marker::String(str) => builder.push_text(str),
        }
    };
    // The marker is a separate box, so words in the content don't continue from it.
    let mut text_transformer = TextTransformer::default();
    text_transformer.word_break(&builder);

    if let Some(before_id) = root_node.before() {
        build_inline_layout_recursive(
            &mut builder,
            &mut text_transformer,
            nodes,
            before_id,
            &text_transform,
        );
    }
    for child_id in root_node.children.iter().copied() {
        build_inline_layout_recursive(
            &mut builder,
            &mut text_transformer,
            nodes,
            child_id,
            &text_transform,
        );
    }
    if let Some(after_id) = root_node.after() {
        build_inline_layout_recursive(
            &mut builder,
            &mut text_transformer,
            nodes,
            after_id,
            &text_transform,
        );
    }

    text_layout.text = builder.build_into(&mut text_layout.layout);
    return;

    fn build_inline_layout_recursive(
        builder: &mut TreeBuilder<TextBrush>,
        text_transformer: &mut TextTransformer,
        nodes: &crate::NodeTree,
        node_id: NodeId,
        parent_text_transform: &CaseTransform,
    ) {
        let node = &nodes[node_id];

        let style = node.primary_styles();
        let style = style.as_ref();

        let text_transform = style
            .map(|s| CaseTransform::from_style(s))
            .unwrap_or(CaseTransform::NONE);

        match &node.data {
            NodeData::Element(element_data) | NodeData::AnonymousBlock(element_data) => {
                // if the input type is hidden, hide it
                if *element_data.name.local == *"input" {
                    if let Some("hidden") = element_data.attr(local_name!("type")) {
                        return;
                    }
                }

                let display = node.display_style().unwrap_or(Display::inline());
                let position = style
                    .map(|s| s.clone_position())
                    .unwrap_or(PositionProperty::Static);
                let float = style.map(|s| s.clone_float()).unwrap_or(Float::None);
                let box_kind = if position.is_absolutely_positioned() {
                    InlineBoxKind::OutOfFlow
                } else if float.is_floating() {
                    InlineBoxKind::CustomOutOfFlow
                } else {
                    InlineBoxKind::InFlow
                };

                match (display.outside(), display.inside()) {
                    (DisplayOutside::None, DisplayInside::None) => {
                        // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                    }
                    (DisplayOutside::None, DisplayInside::Contents) => {
                        // Paint keeps the brush of the nearest element with a
                        // box; white space follows this element's style.
                        let brush = TextBrush {
                            id: nearest_boxed_ancestor(nodes, node_id),
                            white_space_node: node_id,
                        };
                        let whitespace = style.map(|s| {
                            [
                                parley::StyleProperty::Brush(brush),
                                parley::StyleProperty::WhiteSpaceCollapse(
                                    stylo_to_parley::white_space_collapse(
                                        s.clone_white_space_collapse(),
                                    ),
                                ),
                                parley::StyleProperty::TextWrapMode(
                                    stylo_to_parley::text_wrap_mode(s.clone_text_wrap_mode()),
                                ),
                            ]
                        });
                        builder.push_style_modification_span(
                            whitespace
                                .as_ref()
                                .map_or(&[][..], |styles| styles.as_slice()),
                        );
                        for child_id in node.children.iter().copied() {
                            // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                            build_inline_layout_recursive(
                                builder,
                                text_transformer,
                                nodes,
                                child_id,
                                &text_transform,
                            );
                        }
                        builder.pop_style_span();
                    }
                    (DisplayOutside::Inline, DisplayInside::Flow) => {
                        let tag_name = &element_data.name.local;

                        if is_inline_box_element(tag_name) {
                            if box_kind == InlineBoxKind::InFlow {
                                text_transformer.word_break(builder);
                            }
                            builder.push_inline_box(InlineBox {
                                id: node_id.as_u64(),
                                kind: box_kind,
                                // Overridden by push_inline_box method
                                index: 0,
                                // Width and height are set during layout
                                width: 0.0,
                                height: 0.0,
                                baseline: None,
                                vertical_align: node
                                    .primary_styles()
                                    .map(|s| stylo_to_parley::vertical_align(&s))
                                    .unwrap_or_default(),
                            });
                        } else if *tag_name == local_name!("br") {
                            // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                            // TODO: update span id for br spans
                            builder.push_style_modification_span(&[
                                parley::StyleProperty::WhiteSpaceCollapse(
                                    WhiteSpaceCollapse::Preserve,
                                ),
                            ]);
                            builder.push_text("\n");
                            builder.pop_style_span();
                            text_transformer.word_break(builder);
                        } else {
                            // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                            let style = node
                                .primary_styles()
                                .map(|s| stylo_to_parley::style(node.id, &s))
                                .unwrap_or_else(|| parley::TextStyle {
                                    white_space_collapse: WhiteSpaceCollapse::Collapse,
                                    ..Default::default()
                                });

                            builder.push_style_span(style);

                            if let Some(before_id) = node.before() {
                                build_inline_layout_recursive(
                                    builder,
                                    text_transformer,
                                    nodes,
                                    before_id,
                                    &text_transform,
                                );
                            }

                            for child_id in node.children.iter().copied() {
                                build_inline_layout_recursive(
                                    builder,
                                    text_transformer,
                                    nodes,
                                    child_id,
                                    &text_transform,
                                );
                            }
                            if let Some(after_id) = node.after() {
                                build_inline_layout_recursive(
                                    builder,
                                    text_transformer,
                                    nodes,
                                    after_id,
                                    &text_transform,
                                );
                            }

                            builder.pop_style_span();
                        }
                    }
                    // Inline box
                    (_, _) => {
                        if box_kind == InlineBoxKind::InFlow {
                            text_transformer.word_break(builder);
                        }
                        builder.push_inline_box(InlineBox {
                            id: node_id.as_u64(),
                            kind: box_kind,
                            // Overridden by push_inline_box method
                            index: 0,
                            // Width and height are set during layout
                            width: 0.0,
                            height: 0.0,
                            baseline: None,
                            vertical_align: node
                                .primary_styles()
                                .map(|s| stylo_to_parley::vertical_align(&s))
                                .unwrap_or_default(),
                        });
                    }
                };
            }
            NodeData::Text(data) => {
                // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
                // dbg!(&data.content);

                let text =
                    text_transformer.transform(&data.content, parent_text_transform, builder);
                builder.push_text(text);
            }
            NodeData::Comment { .. } => {
                // node.remove_damage(CONSTRUCT_DESCENDENT | CONSTRUCT_FC | CONSTRUCT_BOX);
            }
            NodeData::Document(_) => unreachable!(),
        }
    }
}

#[cfg(all(test, feature = "svg"))]
mod inline_svg_font_size_tests {
    use crate::{Attribute, BaseDocument, DocumentConfig, UserAgentStylesheet, qual_name};
    use blitz_traits::shell::{ColorScheme, Viewport};
    use markup5ever::{LocalName, QualName, ns};

    /// The stroke width usvg parsed for `stroke-width="0.1em"` on a path
    /// inside an `<svg>` with `svg_attrs`, under a 20px div.
    fn stroke_width(svg_attrs: &[(&str, &str)], sheet: &str) -> f32 {
        stroke_width_with(svg_attrs, sheet, "")
    }

    /// As [`stroke_width`], with `embedded` as a `<style>` inside the svg.
    fn stroke_width_with(svg_attrs: &[(&str, &str)], sheet: &str, embedded: &str) -> f32 {
        stroke_width_nested(svg_attrs, sheet, embedded, None)
    }

    /// As [`stroke_width_with`], with the path inside a nested `<svg>` with
    /// `nested` attributes when given.
    fn stroke_width_nested(
        svg_attrs: &[(&str, &str)],
        sheet: &str,
        embedded: &str,
        nested: Option<&[(&str, &str)]>,
    ) -> f32 {
        with_tree(svg_attrs, sheet, embedded, nested, &[], |tree| {
            fn find(group: &usvg::Group) -> Option<f32> {
                group.children().iter().find_map(|node| match node {
                    usvg::Node::Path(p) => p.stroke().map(|s| s.width().get()),
                    usvg::Node::Group(g) => find(g),
                    _ => None,
                })
            }
            find(tree.root()).expect("a stroked path")
        })
    }

    /// Builds the test document and hands `inspect` the svg's usvg tree.
    /// `path_attrs` go on the path; a `<text>` sits next to it.
    fn with_tree<R>(
        svg_attrs: &[(&str, &str)],
        sheet: &str,
        embedded: &str,
        nested: Option<&[(&str, &str)]>,
        path_attrs: &[(&str, &str)],
        inspect: impl FnOnce(&usvg::Tree) -> R,
    ) -> R {
        let (doc, svg) = build_doc(svg_attrs, sheet, embedded, nested, path_attrs);
        let tree = doc.nodes[svg]
            .element_data()
            .unwrap()
            .svg_data()
            .expect("parsed svg");
        inspect(tree)
    }

    fn build_doc(
        svg_attrs: &[(&str, &str)],
        sheet: &str,
        embedded: &str,
        nested: Option<&[(&str, &str)]>,
        path_attrs: &[(&str, &str)],
    ) -> (BaseDocument, crate::NodeId) {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        if !sheet.is_empty() {
            doc.add_user_agent_stylesheet(UserAgentStylesheet::parse(sheet));
        }
        let attr = |k: &str, v: &str| Attribute {
            name: QualName::new(None, ns!(), LocalName::from(k)),
            value: v.to_owned(),
        };
        let svg_name = |t: &str| QualName::new(None, ns!(svg), LocalName::from(t));
        let root_id = doc.root_node().id;
        let mut m = doc.mutate();
        let html = m.create_element(qual_name!("html"), vec![]);
        let body = m.create_element(qual_name!("body"), vec![]);
        let div = m.create_element(qual_name!("div"), vec![attr("style", "font-size: 20px")]);
        let mut attrs = vec![
            attr("viewBox", "0 0 100 100"),
            attr("width", "100px"),
            attr("height", "100px"),
        ];
        attrs.extend(svg_attrs.iter().map(|(k, v)| attr(k, v)));
        let svg = m.create_element(svg_name("svg"), attrs);
        let path = m.create_element(
            svg_name("path"),
            vec![
                attr("d", "M0 50 L100 50"),
                attr("stroke", "black"),
                attr("stroke-width", "0.1em"),
            ]
            .into_iter()
            .chain(path_attrs.iter().map(|(k, v)| attr(k, v)))
            .collect(),
        );
        let text = m.create_element(svg_name("text"), vec![attr("y", "40")]);
        let glyphs = m.create_text_node("Ag");
        m.append_children(text, &[glyphs]);
        match nested {
            Some(nested_attrs) => {
                let mut attrs = vec![attr("width", "100"), attr("height", "100")];
                attrs.extend(nested_attrs.iter().map(|(k, v)| attr(k, v)));
                let inner = m.create_element(svg_name("svg"), attrs);
                m.append_children(inner, &[path, text]);
                m.append_children(svg, &[inner]);
            }
            None => m.append_children(svg, &[path, text]),
        }
        if !embedded.is_empty() {
            let style = m.create_element(svg_name("style"), vec![]);
            let css = m.create_text_node(embedded);
            m.append_children(style, &[css]);
            let first = m.child_ids(svg)[0];
            m.insert_nodes_before(first, &[style]);
        }
        m.append_children(div, &[svg]);
        m.append_children(body, &[div]);
        m.append_children(html, &[body]);
        m.append_children(root_id, &[html]);
        drop(m);
        doc.resolve(0.0);
        (doc, svg)
    }

    /// The font weight, italic-ness and size usvg resolved for the `<text>`.
    /// usvg keeps a text node only when its font database has a face for
    /// it, so this parses the source Blitz serializes with a database that
    /// holds Blitz's bullet font as every generic family.
    fn text_font(
        svg_attrs: &[(&str, &str)],
        embedded: &str,
        nested: Option<&[(&str, &str)]>,
    ) -> (u16, bool, f32) {
        let (doc, svg) = build_doc(svg_attrs, "", embedded, nested, &[]);
        let node = &doc.nodes[svg];
        let size = node
            .primary_styles()
            .unwrap()
            .get_font()
            .font_size
            .used_size
            .0
            .px();
        let source = node.svg_source(size);
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(crate::BULLET_FONT.to_vec());
        let family = db.faces().next().unwrap().families[0].0.clone();
        db.set_serif_family(&family);
        db.set_sans_serif_family(&family);
        let options = usvg::Options {
            fontdb: std::sync::Arc::new(db),
            font_size: size,
            font_family: family,
            ..Default::default()
        };
        let tree = usvg::Tree::from_str(&source, &options).expect("parsed svg");
        {
            let tree = &tree;
            fn find(group: &usvg::Group) -> Option<(u16, bool, f32)> {
                group.children().iter().find_map(|node| match node {
                    usvg::Node::Text(t) => t.chunks().first()?.spans().first().map(|span| {
                        (
                            span.font().weight(),
                            span.font().style() != usvg::FontStyle::Normal,
                            span.font_size().get(),
                        )
                    }),
                    usvg::Node::Group(g) => find(g),
                    _ => None,
                })
            }
            find(tree.root()).expect("a text node")
        }
    }

    #[test]
    fn rules_that_do_not_touch_the_root_size_are_copied_as_written() {
        let stroke = |label: &str, embedded: &str| {
            with_tree(&[], "", embedded, None, &[("data-label", label)], |tree| {
                tree.root().children().iter().find_map(|node| match node {
                    usvg::Node::Path(p) => p.stroke().map(|s| s.width().get()),
                    _ => None,
                })
            })
        };
        // Each selector must match exactly when it does in the untouched
        // sheet. A root font rule in the same sheet forces the rewrite;
        // unmatched, the 0.1em stroke is 2 without it and 4 with it.
        let root_rule = "svg { font: 2em serif; } ";
        for (label, rule, px) in [
            ("it's", r#"[data-label="it's"] { stroke-width: 9px; }"#, 9.0),
            ("a\"b", r#"[data-label="a\"b"] { stroke-width: 7px; }"#, 7.0),
            ("a\\b", r#"[data-label='a\\b'] { stroke-width: 6px; }"#, 6.0),
            ("x", r#"path[data-label='x'], g { stroke-width: 5px }"#, 5.0),
            ("y", "/* c */ path:first-child { stroke-width: 3px; }", 3.0),
        ] {
            let untouched = stroke(label, rule);
            let rewritten = stroke(label, &format!("{root_rule}{rule}"));
            assert_eq!(
                untouched == Some(px),
                rewritten == Some(px),
                "{rule}: {untouched:?} untouched, {rewritten:?} rewritten"
            );
        }
        // usvg matches the first one, so the rewrite must too.
        assert_eq!(
            stroke(
                "it's",
                &format!(r#"{root_rule}[data-label="it's"] {{ stroke-width: 9px; }}"#)
            ),
            Some(9.0)
        );
    }

    #[test]
    fn bracketed_and_functional_selectors_keep_their_boundaries() {
        let stroke = |svg_attrs: &[(&str, &str)], path_label: &str, embedded: &str| {
            with_tree(
                svg_attrs,
                "",
                embedded,
                None,
                &[("data-label", path_label)],
                |tree| {
                    tree.root().children().iter().find_map(|node| match node {
                        usvg::Node::Path(p) => p.stroke().map(|s| s.width().get()),
                        _ => None,
                    })
                },
            )
        };
        let root = [("data-label", "it's")];
        // An attribute selector right before the brace, with and without a
        // space: the root's computed 40px applies once, so 0.1em is 4.
        for embedded in [
            r#"svg[data-label="it's"]{font-size:2em!important}"#,
            r#"svg[data-label="it's"] {font-size:2em!important}"#,
        ] {
            assert_eq!(stroke(&root, "-", embedded), Some(4.0), "{embedded}");
        }
        // An attribute selector before a comma: the path rule survives
        // whole, so its 9px stroke applies.
        for embedded in [
            r#"path[data-label="it's"], svg {font-size:2em!important;stroke-width:9px}"#,
            r#"path[data-label="it's"],svg{font-size:2em!important;stroke-width:9px}"#,
        ] {
            assert_eq!(stroke(&[], "it's", embedded), Some(9.0), "{embedded}");
        }
        // A functional selector simplecss cannot parse, before a comma, is
        // dropped as usvg drops it; the root selector after it still gets
        // split, and nothing else matches the path.
        let embedded = r#"path:nth-child(1), svg[data-label="it's"]{font-size:2em!important}"#;
        assert_eq!(stroke(&root, "-", embedded), Some(4.0));
    }

    #[test]
    fn split_lists_keep_the_selectors_simplecss_keeps_in_source_order() {
        let stroke = |embedded: &str| {
            with_tree(
                &[("id", "r"), ("class", "c")],
                "",
                embedded,
                None,
                &[],
                |tree| {
                    tree.root().children().iter().find_map(|node| match node {
                        usvg::Node::Path(p) => p.stroke().map(|s| s.width().get()),
                        _ => None,
                    })
                },
            )
        };
        // The same sheet in plain usvg, without Blitz: whether the path
        // gets the rule's 9px stroke is the reference.
        let plain = |embedded: &str| {
            let source = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" id="r" class="c" viewBox="0 0 100 100" width="100" height="100"><style>{embedded}</style><path d="M0 50 L100 50" stroke="black" stroke-width="0.1em"/></svg>"#
            );
            let tree = usvg::Tree::from_str(&source, &usvg::Options::default()).unwrap();
            tree.root().children().iter().find_map(|node| match node {
                usvg::Node::Path(p) => p.stroke().map(|s| s.width().get()),
                _ => None,
            })
        };
        let body = "{font-size:40px!important;stroke-width:9px!important}";
        for list in [
            "#r,path",
            "path,#r",
            "#r, path, svg, .c",
            ".c,svg,path,#r",
            "path, path, #r",
            "#r, #r, path",
            // Selectors simplecss cannot parse, before and after others.
            "path:nth-child(1), #r, path",
            "#r, path:nth-child(1), path",
            "#r, path:nth-child(1) g, path",
            "#r, path > , path",
            // Unicode spaces are not CSS whitespace: simplecss rejects a
            // selector they end or start, and must see them where they are.
            "#r, path\u{a0}",
            "#r, path\u{2003}",
            "#r,\u{a0}path",
            "\u{2003}path, #r",
            // CSS whitespace around selectors is fine. (Form feed is CSS
            // whitespace too, but XML forbids it, so no SVG can hold it.)
            "#r,\tpath\n",
            "#r ,\t path ",
            "\n#r,\r\npath",
        ] {
            let embedded = format!("{list} {body}");
            let reference = plain(&embedded) == Some(9.0);
            assert_eq!(stroke(&embedded) == Some(9.0), reference, "{list}");
        }
        // The orders and specificities from the report keep the path rule.
        assert_eq!(stroke(&format!("#r,path {body}")), Some(9.0));
        assert_eq!(stroke(&format!("path,#r {body}")), Some(9.0));
        // A no-break space after `path` drops it, as in plain usvg: the
        // path inherits the root's 40px, so 0.1em is 4. CSS whitespace does
        // not.
        assert_eq!(stroke(&format!("#r, path\u{a0} {body}")), Some(4.0));
        assert_eq!(stroke(&format!("#r, path\u{2003} {body}")), Some(4.0));
        assert_eq!(stroke(&format!("#r,\tpath\n {body}")), Some(9.0));
    }

    #[test]
    fn root_font_rules_keep_every_property_but_the_size() {
        // The root's text keeps the shorthand's style and weight; the size
        // is the computed 40px.
        let embedded = "svg { font: italic bold 2em serif; }";
        assert_eq!(text_font(&[], embedded, None), (700, true, 40.0));
        // A later longhand still wins over the shorthand, for the root and
        // for a nested svg, whose size the rule still sets: 2em of 40px.
        let embedded = "svg { font: italic bold 2em serif; font-weight: normal; }";
        assert_eq!(text_font(&[], embedded, None), (400, true, 40.0));
        assert_eq!(text_font(&[], embedded, Some(&[])), (400, true, 80.0));
        // Important longhands keep their importance.
        let embedded = "svg { font-weight: 300 !important; font: bold 2em serif; }";
        assert_eq!(text_font(&[], embedded, None), (300, false, 40.0));
    }

    #[test]
    fn em_lengths_resolve_against_the_computed_root_size_once() {
        // Inherited 20px: 0.1em is 2.
        assert_eq!(stroke_width(&[], ""), 2.0);
        // The root's own relative size applies once: 2em of 20px is 40px.
        assert_eq!(stroke_width(&[("style", "font-size: 2em")], ""), 4.0);
        assert_eq!(stroke_width(&[("style", "font: 2em sans-serif")], ""), 4.0);
        // A presentation attribute.
        assert_eq!(stroke_width(&[("font-size", "40")], ""), 4.0);
        // A stylesheet rule usvg cannot see still reaches it.
        assert_eq!(stroke_width(&[], "svg { font-size: 30px; }"), 3.0);
        // !important in a stylesheet beats the inline style.
        assert_eq!(
            stroke_width(
                &[("style", "font-size: 2em")],
                "svg { font-size: 30px !important; }"
            ),
            3.0
        );
        // The inline style beats a presentation attribute.
        assert_eq!(
            stroke_width(&[("font-size", "60"), ("style", "font-size: 30px")], ""),
            3.0
        );
    }

    #[test]
    fn important_and_shorthand_inline_sizes_apply_once() {
        // Computed 40px; usvg must not apply 2em again on top.
        assert_eq!(
            stroke_width(&[("style", "font-size: 2em !important")], ""),
            4.0
        );
        assert_eq!(
            stroke_width(
                &[("style", "FONT : 2em/1 sans-serif ! IMPORTANT ; color: red")],
                ""
            ),
            4.0
        );
        assert_eq!(stroke_width(&[("style", "font-size:200%")], ""), 4.0);
    }

    #[test]
    fn embedded_rules_cannot_reapply_a_root_size() {
        // An !important rule for the root inside the svg's own <style>
        // must not apply 2em again on top of the computed size.
        let embedded = "svg { font-size: 2em !important; }";
        // Stylo applies the <style> to the root too (40px), as Chromium does.
        assert_eq!(stroke_width_with(&[], "", embedded), 4.0);
        assert_eq!(
            stroke_width_with(&[("style", "font-size: 2em")], "", embedded),
            4.0
        );
        assert_eq!(
            stroke_width_with(
                &[("style", "font: 2em sans-serif !important")],
                "",
                embedded
            ),
            4.0
        );
        // Embedded rules still style descendants: 0.3em of the root's 20px,
        // and a path's own font size is its own.
        assert_eq!(
            stroke_width_with(&[], "", "path { stroke-width: 0.3em; }"),
            6.0
        );
        assert_eq!(
            stroke_width_with(&[], "", "path { font-size: 30px !important; }"),
            3.0
        );
    }

    #[test]
    fn universal_important_rules_reach_descendants_not_the_computed_root() {
        // A <style> inside the svg is a document style sheet, in Chromium
        // and in Blitz, so `*` doubles every element from <html> down,
        // beating the div's inline 20px: html 32, body 64, div 128, svg 256
        // and path 512px, so 0.1em is 51.2. usvg must apply 2em once more
        // only to the path, not to the root's computed 256px.
        let embedded = "* { font-size: 2em !important; }";
        assert_eq!(stroke_width_with(&[], "", embedded), 51.2);
        // Root-only control: `svg` matches only the root here, the path
        // inherits 40px.
        assert_eq!(
            stroke_width_with(&[], "", "svg { font-size: 2em !important; }"),
            4.0
        );
        // A rule that does not match the root is left alone.
        assert_eq!(stroke_width_with(&[], "", "path { font-size: 2em; }"), 4.0);
    }

    #[test]
    fn nested_svgs_keep_their_own_cascade() {
        // `svg` matches the root and the nested svg: root 40px, nested 2em
        // of that = 80px, so 8.
        let embedded = "svg { font-size: 2em !important; }";
        assert_eq!(stroke_width_nested(&[], "", embedded, Some(&[])), 8.0);
        // Without rules the nested svg inherits the root's computed size.
        assert_eq!(
            stroke_width_nested(&[("style", "font-size: 2em")], "", "", Some(&[])),
            4.0
        );
        // An explicit size on the nested svg is its own.
        assert_eq!(
            stroke_width_nested(&[], "", "", Some(&[("style", "font-size: 30px")])),
            3.0
        );
        // A relative one builds on the root's computed 40px.
        assert_eq!(
            stroke_width_nested(
                &[("style", "font-size: 2em")],
                "",
                "",
                Some(&[("font-size", "50%")])
            ),
            2.0
        );
    }

    #[test]
    fn font_size_attributes_use_the_css_grammar() {
        // Keywords: `larger` is 1.2 times the parent in Stylo and Chromium.
        assert!((stroke_width(&[("font-size", "larger")], "") - 2.4).abs() < 1e-4);
        assert_eq!(stroke_width(&[("font-size", "calc(2 * 20px)")], ""), 4.0);
        assert_eq!(stroke_width(&[("font-size", "2em")], ""), 4.0);
        assert_eq!(stroke_width(&[("font-size", "150%")], ""), 3.0);
        // Unitless numbers are SVG user units.
        assert_eq!(stroke_width(&[("font-size", " 30 ")], ""), 3.0);
        // Invalid values are ignored: the inherited 20px stays.
        for bad in ["-10", "10px; color: red", "red", "", "10px 20px"] {
            assert_eq!(stroke_width(&[("font-size", bad)], ""), 2.0, "{bad:?}");
        }
        // Presentation attributes sit above user-agent rules (the test
        // sheet's origin) and below author CSS such as the inline style,
        // which the inline cases above cover.
        assert_eq!(
            stroke_width(&[("font-size", "60")], "svg { font-size: 30px; }"),
            6.0
        );
    }
}
