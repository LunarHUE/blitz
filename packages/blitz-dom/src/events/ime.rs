use blitz_traits::events::{BlitzImeEvent, DomEvent};

use markup5ever::local_name;

use crate::BaseDocument;

pub(crate) fn handle_ime_event<F: FnMut(DomEvent)>(
    doc: &mut BaseDocument,
    event: BlitzImeEvent,
    dispatch_event: F,
) {
    if let Some(node_id) = doc.focus_node_id {
        let node = &mut doc.nodes[node_id];
        // IME text goes straight into the editor with no cancelable event
        // first, so a disabled input, which script can still focus, must be
        // skipped here. Clearing a composition is still applied, so one
        // started before the input was disabled doesn't stay on screen.
        let disabled = node
            .element_data()
            .is_some_and(|el| el.attr(local_name!("disabled")).is_some());
        let clears = match &event {
            BlitzImeEvent::Disabled => true,
            BlitzImeEvent::Preedit(text, _) => text.is_empty(),
            _ => false,
        };
        if disabled && !clears {
            return;
        }
        let text_input_data = node
            .data
            .downcast_element_mut()
            .and_then(|el| el.text_input_data_mut());
        if let Some(input_data) = text_input_data {
            let generated_event = input_data.apply_ime_event(
                &mut doc.font_ctx.lock().unwrap(),
                &mut doc.layout_ctx,
                event,
            );

            if let Some(generated_event) = generated_event {
                doc.apply_generated_text_input_event(node_id, generated_event, dispatch_event);
            }

            #[cfg(feature = "tracing")]
            tracing::debug!(node_id = ?node_id, "Sent ime event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Attribute, BaseDocument, DocumentConfig, qual_name};
    use blitz_traits::node_id::NodeId;
    use blitz_traits::shell::{ColorScheme, Viewport};

    fn attr(name: markup5ever::QualName, value: &str) -> Attribute {
        Attribute {
            name,
            value: value.into(),
        }
    }

    /// `<input value="fixed" disabled>` and `<input value="fixed">`.
    fn make_doc() -> (BaseDocument, NodeId, NodeId) {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root_id = doc.root_node().id;
        let mut m = doc.mutate();
        let html = m.create_element(qual_name!("html"), vec![]);
        let body = m.create_element(qual_name!("body"), vec![]);
        let off = m.create_element(
            qual_name!("input"),
            vec![
                attr(qual_name!("type"), "text"),
                attr(qual_name!("value"), "fixed"),
                attr(qual_name!("disabled"), ""),
            ],
        );
        let on = m.create_element(
            qual_name!("input"),
            vec![
                attr(qual_name!("type"), "text"),
                attr(qual_name!("value"), "fixed"),
            ],
        );
        m.append_children(body, &[off, on]);
        m.append_children(html, &[body]);
        m.append_children(root_id, &[html]);
        drop(m);
        doc.resolve(0.0);
        (doc, off, on)
    }

    fn text(doc: &BaseDocument, id: NodeId) -> String {
        doc.nodes[id]
            .element_data()
            .unwrap()
            .text_input_data()
            .unwrap()
            .editor
            .raw_text()
            .to_string()
    }

    /// Sends `event` to the focused input and counts the DOM events it
    /// generates, such as `input`.
    fn ime(doc: &mut BaseDocument, event: BlitzImeEvent) -> usize {
        let mut sent = 0;
        handle_ime_event(doc, event, |_| sent += 1);
        sent
    }

    #[test]
    fn a_disabled_input_ignores_ime_text_even_when_focused() {
        let (mut doc, off, _) = make_doc();
        // Focus forced onto the disabled input, as script can.
        assert!(doc.set_focus_to(off));
        assert_eq!(ime(&mut doc, BlitzImeEvent::Preedit("か".into(), None)), 0);
        assert_eq!(text(&doc, off), "fixed");
        assert_eq!(ime(&mut doc, BlitzImeEvent::Commit("漢".into())), 0);
        assert_eq!(text(&doc, off), "fixed");
    }

    #[test]
    fn an_enabled_input_still_takes_ime_text() {
        let (mut doc, _, on) = make_doc();
        assert!(doc.set_focus_to(on));
        ime(&mut doc, BlitzImeEvent::Preedit("か".into(), None));
        assert!(text(&doc, on).contains('か'));
        ime(&mut doc, BlitzImeEvent::Preedit(String::new(), None));
        assert_eq!(ime(&mut doc, BlitzImeEvent::Commit("漢".into())), 1);
        assert!(text(&doc, on).contains('漢'));
        assert!(!text(&doc, on).contains('か'));
    }

    #[test]
    fn a_composition_can_be_cleared_after_the_input_is_disabled() {
        let (mut doc, _, on) = make_doc();
        assert!(doc.set_focus_to(on));
        ime(&mut doc, BlitzImeEvent::Preedit("か".into(), None));
        assert!(text(&doc, on).contains('か'));
        doc.mutate().set_attribute(on, qual_name!("disabled"), "");
        // Clearing the preedit still applies; new text does not.
        ime(&mut doc, BlitzImeEvent::Preedit(String::new(), None));
        assert_eq!(text(&doc, on), "fixed");
        ime(&mut doc, BlitzImeEvent::Commit("漢".into()));
        assert_eq!(text(&doc, on), "fixed");
        ime(&mut doc, BlitzImeEvent::Preedit("か".into(), None));
        ime(&mut doc, BlitzImeEvent::Disabled);
        assert_eq!(text(&doc, on), "fixed");
    }

    #[test]
    fn ime_disabled_clears_an_active_composition_on_a_disabled_input() {
        let (mut doc, _, on) = make_doc();
        assert!(doc.set_focus_to(on));
        ime(&mut doc, BlitzImeEvent::Preedit("か".into(), None));
        doc.mutate().set_attribute(on, qual_name!("disabled"), "");
        assert!(text(&doc, on).contains('か'));
        // No empty preedit first: IME Disabled alone ends the composition.
        ime(&mut doc, BlitzImeEvent::Disabled);
        assert_eq!(text(&doc, on), "fixed");
    }
}
