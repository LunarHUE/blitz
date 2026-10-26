use crate::events::focus::generate_focus_events;
use crate::{BaseDocument, node::GeneratedTextInputEvent, util::ACTION_MOD};
use blitz_traits::node_id::NodeId;
use blitz_traits::{
    SmolStr,
    events::{BlitzInputEvent, BlitzKeyEvent, DomEvent, DomEventData},
};
use keyboard_types::{Key, Modifiers, NamedKey};
use markup5ever::local_name;

pub(super) enum KeyboardOrTextInputEvent {
    KeyPress(BlitzKeyEvent),
    AppleStandardKeyBinding(SmolStr),
}

pub(crate) fn handle_key_or_input_event<F: FnMut(DomEvent)>(
    doc: &mut BaseDocument,
    target: NodeId,
    event: KeyboardOrTextInputEvent,
    mut dispatch_event: F,
) {
    if let KeyboardOrTextInputEvent::KeyPress(event) = &event {
        if event.key == Key::Named(NamedKey::Tab) {
            let backwards = event.modifiers.contains(Modifiers::SHIFT);
            let old_focus = doc.focus_node_id;
            generate_focus_events(
                doc,
                &mut |doc| {
                    if backwards {
                        doc.focus_prev_node();
                    } else {
                        doc.focus_next_node();
                    }
                },
                &mut dispatch_event,
            );
            if doc.focus_node_id != old_focus {
                doc.shell_provider.request_redraw();
            }
            return;
        }

        // Handle copy (Ctrl+C/Cmd+C) for text selection when no text input is focused
        if event.state.is_pressed() {
            let action_mod = event.modifiers.contains(ACTION_MOD);
            if action_mod {
                if let Key::Character(c) = &event.key {
                    if c.to_lowercase() == "c" {
                        // Check if we have a text selection (and no focused text input)
                        let has_focused_text_input = doc.focus_node_id.is_some_and(|id| {
                            doc.get_node(id)
                                .and_then(|n| n.element_data())
                                .is_some_and(|e| e.text_input_data().is_some())
                        });

                        if !has_focused_text_input {
                            if let Some(text) = doc.get_selected_text() {
                                let _ = doc.shell_provider.set_clipboard_text(text);
                                return;
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(node_id) = doc.focus_node_id {
        if target != node_id {
            return;
        }

        let node = &mut doc.nodes[node_id];
        let Some(element_data) = node.element_data_mut() else {
            return;
        };
        // Read on every event, so toggling either takes effect at once. A
        // disabled input can still be focused by script, and editing key
        // bindings never pass through a cancellable keydown, so it refuses
        // edits here just as a readonly one does.
        let read_only = element_data.attr(local_name!("readonly")).is_some()
            || element_data.attr(local_name!("disabled")).is_some();

        if let Some(input_data) = element_data.text_input_data_mut() {
            let generated_event = match event {
                KeyboardOrTextInputEvent::KeyPress(blitz_key_event) => input_data
                    .apply_keypress_event(
                        &mut doc.font_ctx.lock().unwrap(),
                        &mut doc.layout_ctx,
                        &*doc.shell_provider,
                        blitz_key_event,
                        read_only,
                    ),
                KeyboardOrTextInputEvent::AppleStandardKeyBinding(command) => input_data
                    .apply_apple_standard_keybinding(
                        &mut doc.font_ctx.lock().unwrap(),
                        &mut doc.layout_ctx,
                        &*doc.shell_provider,
                        &command,
                        read_only,
                    ),
            };

            if let Some(generated_event) = generated_event {
                doc.apply_generated_text_input_event(node_id, generated_event, dispatch_event);
            }
        }
    }
}

impl BaseDocument {
    pub(crate) fn apply_generated_text_input_event<F: FnMut(DomEvent)>(
        &mut self,
        node_id: NodeId,
        event: GeneratedTextInputEvent,
        mut dispatch_event: F,
    ) {
        let node = &mut self.nodes[node_id];
        let element_data = node
            .element_data_mut()
            .expect("apply_generated_text_input_event called on a node that is not an element");
        let input_data = element_data
            .text_input_data_mut()
            .expect("apply_generated_text_input_event called on a node that is not a text input");

        match event {
            GeneratedTextInputEvent::Input => {
                let value = input_data.editor.raw_text().to_string();
                dispatch_event(DomEvent::new(
                    node_id,
                    DomEventData::Input(BlitzInputEvent { value }),
                ));
                self.shell_provider.request_redraw();
            }
            GeneratedTextInputEvent::Select | GeneratedTextInputEvent::PreEditChange => {
                self.shell_provider.request_redraw();
            }
            GeneratedTextInputEvent::Submit => {
                // TODO: Generate submit event that can be handled by script
                implicit_form_submission(self, node_id);
            }
        }
    }
}

/// https://html.spec.whatwg.org/multipage/form-control-infrastructure.html#field-that-blocks-implicit-submission
fn implicit_form_submission(doc: &BaseDocument, text_target: NodeId) {
    let Some(form_owner_id) = doc.controls_to_form.get(&text_target) else {
        return;
    };
    if doc
        .controls_to_form
        .iter()
        .filter(|(_control_id, form_id)| *form_id == form_owner_id)
        .filter_map(|(control_id, _)| doc.nodes[*control_id].element_data())
        .filter(|element_data| {
            element_data.attr(local_name!("type")).is_some_and(|t| {
                matches!(
                    t,
                    "text"
                        | "search"
                        | "email"
                        | "url"
                        | "tel"
                        | "password"
                        | "date"
                        | "month"
                        | "week"
                        | "time"
                        | "datetime-local"
                        | "number"
                )
            })
        })
        .count()
        > 1
    {
        return;
    }

    doc.submit_form(*form_owner_id, *form_owner_id);
}

#[cfg(test)]
mod read_only_tests {
    use std::sync::{Arc, Mutex};

    use blitz_traits::events::{DomEventData, KeyState};
    use blitz_traits::shell::{ClipboardError, ColorScheme, ShellProvider, Viewport};
    use keyboard_types::{Code, Location};

    use super::*;
    use crate::events::ime::handle_ime_event;
    use crate::{Attribute, DocumentConfig, qual_name};
    use blitz_traits::events::BlitzImeEvent;

    /// A clipboard the tests can read and fill.
    #[derive(Default)]
    struct Clipboard(Mutex<String>);

    impl ShellProvider for Clipboard {
        fn get_clipboard_text(&self) -> Result<String, ClipboardError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn set_clipboard_text(&self, text: String) -> Result<(), ClipboardError> {
            *self.0.lock().unwrap() = text;
            Ok(())
        }
    }

    struct Doc {
        doc: BaseDocument,
        clip: Arc<Clipboard>,
        /// `<input value="fixed" readonly>`
        input: NodeId,
        /// `<textarea readonly>`
        area: NodeId,
        /// `<textarea>`
        open: NodeId,
    }

    fn attr(name: markup5ever::QualName, value: &str) -> Attribute {
        Attribute {
            name,
            value: value.into(),
        }
    }

    fn make() -> Doc {
        let clip = Arc::new(Clipboard::default());
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            shell_provider: Some(clip.clone()),
            ..Default::default()
        });
        let root = doc.root_node().id;
        let mut m = doc.mutate();
        let html = m.create_element(qual_name!("html"), vec![]);
        let body = m.create_element(qual_name!("body"), vec![]);
        let input = m.create_element(
            qual_name!("input"),
            vec![
                attr(qual_name!("type"), "text"),
                attr(qual_name!("value"), "fixed"),
                attr(qual_name!("readonly"), ""),
            ],
        );
        let area = m.create_element(
            qual_name!("textarea"),
            vec![attr(qual_name!("readonly"), "")],
        );
        let open = m.create_element(qual_name!("textarea"), vec![]);
        m.append_children(body, &[input, area, open]);
        m.append_children(html, &[body]);
        m.append_children(root, &[html]);
        drop(m);
        doc.resolve(0.0);
        Doc {
            doc,
            clip,
            input,
            area,
            open,
        }
    }

    impl Doc {
        fn text(&self, id: NodeId) -> String {
            self.doc.nodes[id]
                .element_data()
                .unwrap()
                .text_input_data()
                .unwrap()
                .editor
                .raw_text()
                .to_string()
        }

        fn selected(&self, id: NodeId) -> Option<String> {
            self.doc.nodes[id]
                .element_data()
                .unwrap()
                .text_input_data()
                .unwrap()
                .editor
                .selected_text()
                .map(str::to_string)
        }

        /// Sends a keydown to the focused input; returns the `input`
        /// events it generated.
        fn key(&mut self, key: Key, mods: Modifiers) -> usize {
            let target = self.doc.focus_node_id.unwrap();
            let event = BlitzKeyEvent {
                key,
                code: Code::Unidentified,
                modifiers: mods,
                location: Location::Standard,
                is_auto_repeating: false,
                is_composing: false,
                state: KeyState::Pressed,
                text: None,
            };
            let mut inputs = 0;
            handle_key_or_input_event(
                &mut self.doc,
                target,
                KeyboardOrTextInputEvent::KeyPress(event),
                |e| inputs += usize::from(matches!(e.data, DomEventData::Input(_))),
            );
            inputs
        }

        fn char(&mut self, c: &str) -> usize {
            self.key(Key::Character(c.into()), Modifiers::empty())
        }

        fn action(&mut self, c: &str) -> usize {
            self.key(Key::Character(c.into()), ACTION_MOD)
        }

        fn named(&mut self, k: NamedKey) -> usize {
            self.key(Key::Named(k), Modifiers::empty())
        }

        fn apple(&mut self, command: &str) -> usize {
            let target = self.doc.focus_node_id.unwrap();
            let mut inputs = 0;
            handle_key_or_input_event(
                &mut self.doc,
                target,
                KeyboardOrTextInputEvent::AppleStandardKeyBinding(command.into()),
                |e| inputs += usize::from(matches!(e.data, DomEventData::Input(_))),
            );
            inputs
        }

        fn ime(&mut self, event: BlitzImeEvent) {
            handle_ime_event(&mut self.doc, event, |_| {});
        }
    }

    #[test]
    fn a_textarea_starts_from_its_value_attribute() {
        let mut doc = BaseDocument::new(DocumentConfig {
            viewport: Some(Viewport::new(400, 300, 1.0, ColorScheme::Light)),
            ..Default::default()
        });
        let root = doc.root_node().id;
        let mut m = doc.mutate();
        let html = m.create_element(qual_name!("html"), vec![]);
        let body = m.create_element(qual_name!("body"), vec![]);
        let valued = m.create_element(
            qual_name!("textarea"),
            vec![attr(qual_name!("value"), "from value")],
        );
        let plain = m.create_element(qual_name!("textarea"), vec![]);
        let child = m.create_text_node("from children");
        m.append_children(plain, &[child]);
        m.append_children(body, &[valued, plain]);
        m.append_children(html, &[body]);
        m.append_children(root, &[html]);
        drop(m);
        doc.resolve(0.0);
        let text = |id: NodeId| {
            doc.nodes[id]
                .element_data()
                .unwrap()
                .text_input_data()
                .unwrap()
                .editor
                .raw_text()
                .to_string()
        };
        assert_eq!(text(valued), "from value");
        assert_eq!(text(plain), "from children");
    }

    #[test]
    fn a_focused_disabled_input_or_textarea_refuses_every_edit() {
        let mut d = make();
        let (input, area) = (d.input, d.open);
        // Give the textarea text, with the caret at its start.
        assert!(d.doc.set_focus_to(area));
        d.char("a");
        d.char("b");
        d.named(NamedKey::Home);
        assert_eq!(d.text(area), "ab");
        // `input` loses readonly and gains disabled; `area` gets disabled.
        d.doc
            .mutate()
            .clear_attribute(input, qual_name!("readonly"));
        for id in [input, area] {
            d.doc.mutate().set_attribute(id, qual_name!("disabled"), "");
        }
        for id in [input, area] {
            // Script can still focus a disabled control.
            assert!(d.doc.set_focus_to(id) || d.doc.focus_node_id == Some(id));
            let before = d.text(id);
            assert_eq!(d.char("z"), 0);
            assert_eq!(d.named(NamedKey::Backspace), 0);
            assert_eq!(d.named(NamedKey::Delete), 0);
            assert_eq!(d.apple("deleteForward:"), 0);
            assert_eq!(d.apple("deleteBackward:"), 0);
            assert_eq!(d.apple("insertText:"), 0);
            assert_eq!(d.text(id), before);
        }
        // Enabled again at runtime: it edits.
        d.doc
            .mutate()
            .clear_attribute(input, qual_name!("disabled"));
        assert!(d.doc.set_focus_to(input));
        d.named(NamedKey::End);
        assert_eq!(d.char("z"), 1);
        assert_eq!(d.text(input), "fixedz");
        // Disabled and readonly together refuse edits too.
        d.doc
            .mutate()
            .set_attribute(area, qual_name!("readonly"), "");
        assert!(d.doc.set_focus_to(area));
        assert_eq!(d.apple("deleteForward:"), 0);
        assert_eq!(d.text(area), "ab");
    }

    #[test]
    fn readonly_ignores_every_edit() {
        let mut d = make();
        assert!(d.doc.set_focus_to(d.input));
        *d.clip.0.lock().unwrap() = "pasted".into();
        assert_eq!(d.char("a"), 0);
        assert_eq!(d.named(NamedKey::Backspace), 0);
        assert_eq!(d.named(NamedKey::Delete), 0);
        assert_eq!(d.action("v"), 0);
        d.action("a");
        assert_eq!(d.action("x"), 0, "cut on readonly only copies");
        assert_eq!(*d.clip.0.lock().unwrap(), "fixed");
        assert_eq!(d.text(d.input), "fixed");
        for command in [
            "deleteBackward:",
            "insertText:",
            "yank:",
            "transpose:",
            "deleteWordForward:",
        ] {
            assert_eq!(d.apple(command), 0, "{command}");
        }
        d.ime(BlitzImeEvent::Preedit("か".into(), None));
        d.ime(BlitzImeEvent::Commit("漢".into()));
        assert_eq!(d.text(d.input), "fixed");
    }

    #[test]
    fn readonly_keeps_caret_selection_and_copy() {
        let mut d = make();
        assert!(d.doc.set_focus_to(d.input));
        d.action("a");
        assert_eq!(d.selected(d.input).as_deref(), Some("fixed"));
        assert_eq!(d.action("c"), 0, "Copy is not an edit");
        assert_eq!(*d.clip.0.lock().unwrap(), "fixed");
        d.named(NamedKey::End);
        d.key(Key::Named(NamedKey::ArrowLeft), Modifiers::SHIFT);
        d.key(Key::Named(NamedKey::ArrowLeft), Modifiers::SHIFT);
        assert_eq!(d.selected(d.input).as_deref(), Some("ed"));
        d.apple("moveToBeginningOfLine:");
        d.apple("moveRightAndModifySelection:");
        assert_eq!(d.selected(d.input).as_deref(), Some("f"));
    }

    #[test]
    fn cut_with_nothing_selected_reports_no_input() {
        let mut d = make();
        *d.clip.0.lock().unwrap() = "before".into();
        assert!(d.doc.set_focus_to(d.input));
        d.named(NamedKey::End);
        assert_eq!(d.action("x"), 0, "readonly, collapsed caret");
        assert!(d.doc.set_focus_to(d.open));
        d.char("a");
        assert_eq!(d.action("x"), 0, "editable, collapsed caret");
        assert_eq!(d.text(d.open), "a");
        assert_eq!(*d.clip.0.lock().unwrap(), "before");
        // With a selection, an editable field still cuts.
        d.action("a");
        assert_eq!(d.action("x"), 1);
        assert_eq!(d.text(d.open), "");
        assert_eq!(*d.clip.0.lock().unwrap(), "a");
    }

    #[test]
    fn copy_reports_no_input_on_an_editable_field_either() {
        let mut d = make();
        assert!(d.doc.set_focus_to(d.open));
        d.char("a");
        d.action("a");
        assert_eq!(d.action("c"), 0);
        assert_eq!(*d.clip.0.lock().unwrap(), "a");
        assert_eq!(d.text(d.open), "a");
    }

    #[test]
    fn a_readonly_textarea_takes_no_newline_and_an_open_one_does() {
        let mut d = make();
        assert!(d.doc.set_focus_to(d.area));
        assert_eq!(d.named(NamedKey::Enter), 0);
        assert_eq!(d.apple("insertNewline:"), 0);
        assert_eq!(d.text(d.area), "");
        assert!(d.doc.set_focus_to(d.open));
        d.char("a");
        assert_eq!(d.named(NamedKey::Enter), 1);
        d.char("b");
        assert_eq!(d.text(d.open), "a\nb");
    }

    #[test]
    fn readonly_applies_as_soon_as_it_is_set_or_removed() {
        let mut d = make();
        let input = d.input;
        assert!(d.doc.set_focus_to(input));
        d.doc
            .mutate()
            .clear_attribute(input, qual_name!("readonly"));
        d.named(NamedKey::End);
        assert_eq!(d.char("!"), 1);
        assert_eq!(d.text(input), "fixed!");
        d.doc
            .mutate()
            .set_attribute(input, qual_name!("readonly"), "");
        assert_eq!(d.char("?"), 0);
        assert_eq!(d.text(input), "fixed!");
    }

    #[test]
    fn a_composition_clears_after_the_input_turns_readonly() {
        let mut d = make();
        let input = d.input;
        d.doc
            .mutate()
            .clear_attribute(input, qual_name!("readonly"));
        assert!(d.doc.set_focus_to(input));
        d.ime(BlitzImeEvent::Preedit("か".into(), None));
        assert!(d.text(input).contains('か'));
        d.doc
            .mutate()
            .set_attribute(input, qual_name!("readonly"), "");
        d.ime(BlitzImeEvent::Disabled);
        assert_eq!(d.text(input), "fixed");
    }
}
