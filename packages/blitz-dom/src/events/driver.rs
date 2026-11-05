use crate::Document;
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, DomEvent, DomEventData, EventState, Point, PointerCoords,
    UiEvent,
};
use blitz_traits::node_id::NodeId;
use std::collections::VecDeque;

pub trait EventHandler {
    fn handle_event(
        &mut self,
        chain: &[NodeId],
        event: &mut DomEvent,
        doc: &mut dyn Document,
        event_state: &mut EventState,
    );
}

pub struct NoopEventHandler;
impl EventHandler for NoopEventHandler {
    fn handle_event(
        &mut self,
        _chain: &[NodeId],
        _event: &mut DomEvent,
        _doc: &mut dyn Document,
        _event_state: &mut EventState,
    ) {
        // Do nothing
    }
}

pub struct EventDriver<'doc, Handler: EventHandler> {
    doc: &'doc mut dyn Document,
    handler: Handler,
    queue: VecDeque<DomEvent>,
}

impl<'doc, Handler: EventHandler> EventDriver<'doc, Handler> {
    pub fn new(doc: &'doc mut dyn Document, handler: Handler) -> Self {
        EventDriver {
            doc,
            handler,
            queue: VecDeque::with_capacity(4),
        }
    }

    pub fn handle_pointer_move(&mut self, event: &BlitzPointerEvent) -> Option<NodeId> {
        let mut doc = self.doc.inner_mut();

        let prev_hover_node_id = doc.hover_node_id;
        let changed = doc.set_hover_to(event.page_x(), event.page_y());
        let hover_node_id = doc.hover_node_id;

        drop(doc);

        if !changed {
            return prev_hover_node_id;
        }

        self.dispatch_hover_change(prev_hover_node_id, hover_node_id, event);

        hover_node_id
    }

    /// End hover when the pointer leaves the document, dispatching out/leave
    /// events to the previously hovered chain
    pub fn handle_pointer_leave(&mut self, event: &BlitzPointerEvent) {
        let mut doc = self.doc.inner_mut();
        let prev_hover_node_id = doc.hover_node_id;
        doc.clear_hover();
        drop(doc);

        self.dispatch_hover_change(prev_hover_node_id, None, event);
    }

    fn dispatch_hover_change(
        &mut self,
        prev_hover_node_id: Option<NodeId>,
        hover_node_id: Option<NodeId>,
        event: &BlitzPointerEvent,
    ) {
        let doc = self.doc.inner();
        let mut old_chain = prev_hover_node_id
            .map(|id| doc.node_chain(id))
            .unwrap_or_default();
        let mut new_chain = hover_node_id
            .map(|id| doc.node_chain(id))
            .unwrap_or_default();
        old_chain.reverse();
        new_chain.reverse();

        // Find the difference in the node chain of the last hovered objected and the newest
        let old_len = old_chain.len();
        let new_len = new_chain.len();

        let first_difference_index = old_chain
            .iter()
            .zip(&new_chain)
            .position(|(old, new)| old != new)
            .unwrap_or_else(|| old_len.min(new_len));

        drop(doc);

        let is_mouse = event.is_mouse();

        if let Some(target) = prev_hover_node_id {
            self.handle_dom_event(DomEvent::new(
                target,
                DomEventData::PointerOut(event.clone()),
            ));
            if is_mouse {
                self.handle_dom_event(DomEvent::new(target, DomEventData::MouseOut(event.clone())));
            }

            // Send an mouseleave event to all old elements on the chain
            for node_id in old_chain
                .get(first_difference_index..)
                .unwrap_or(&[])
                .iter()
            {
                self.handle_dom_event(DomEvent::new(
                    *node_id,
                    DomEventData::PointerLeave(event.clone()),
                ));
                if is_mouse {
                    self.handle_dom_event(DomEvent::new(
                        *node_id,
                        DomEventData::MouseLeave(event.clone()),
                    ));
                }
            }
        }

        if let Some(target) = hover_node_id {
            self.handle_dom_event(DomEvent::new(
                target,
                DomEventData::PointerOver(event.clone()),
            ));

            if is_mouse {
                self.handle_dom_event(DomEvent::new(
                    target,
                    DomEventData::MouseOver(event.clone()),
                ));
            }

            // Send an mouseenter event to all new elements on the chain
            for node_id in new_chain
                .get(first_difference_index..)
                .unwrap_or(&[])
                .iter()
            {
                self.handle_dom_event(DomEvent::new(
                    *node_id,
                    DomEventData::PointerEnter(event.clone()),
                ));

                if is_mouse {
                    self.handle_dom_event(DomEvent::new(
                        *node_id,
                        DomEventData::MouseEnter(event.clone()),
                    ));
                }
            }
        }
    }

    pub fn handle_ui_event(&mut self, event: UiEvent) {
        // Input can arrive after an update changed the DOM and before the
        // next frame laid it out: lay out first, so hit testing sees the
        // document as it is. Pointer input sets hover itself below, with
        // leave and enter events, so that layout mustn't move hover first;
        // other input (a key, the wheel) takes hover as a frame leaves it.
        let sets_hover = matches!(
            event,
            UiEvent::PointerMove(_)
                | UiEvent::PointerDown(_)
                | UiEvent::PointerUp(_)
                | UiEvent::PointerCancel(_)
                | UiEvent::PointerLeave(_)
        );
        self.doc.inner_mut().resolve_for_hit_testing(!sets_hover);
        let doc = self.doc.inner();

        // A non-hovering pointer (finger) ends hover once it is lifted or cancelled
        let mut hover_end_event = None;
        // The press a physical release ends, taken before any handler runs.
        let mut released = None;
        let mut hover_node_id = doc.hover_node_id;
        let focussed_node_id = doc.focus_node_id;
        let prev_hover = doc.hover_state();
        drop(doc);

        // A document the pointer has left has no say over the cursor
        let is_leave = matches!(event, UiEvent::PointerLeave(_));

        // Update document input state (hover, focus, active, etc)
        match &event {
            UiEvent::PointerMove(event) => {
                hover_node_id = self.handle_pointer_move(event);
            }
            UiEvent::PointerDown(event) => {
                hover_node_id = self.handle_pointer_move(event);
                let mut doc = self.doc.inner_mut();
                doc.active_node();
                doc.set_mousedown_node_id(hover_node_id);
                doc.click_origins
                    .retain(|(id, button, _)| (*id, *button) != (event.id, event.button));
                if let Some(down) = hover_node_id {
                    doc.click_origins.push((event.id, event.button, down));
                }
            }
            UiEvent::PointerUp(event) => {
                hover_node_id = self.handle_pointer_move(event);
                let mut doc = self.doc.inner_mut();
                doc.unactive_node();
                // The press ends here whether or not the release is cancelled.
                let key = (event.id, event.button);
                released = doc
                    .click_origins
                    .iter()
                    .position(|(id, button, _)| (*id, *button) == key)
                    .map(|i| doc.click_origins.remove(i).2);

                if event.is_primary && matches!(event.id, BlitzPointerId::Finger(_)) {
                    hover_end_event = Some(event.clone());
                }
            }
            UiEvent::PointerCancel(event) => {
                hover_node_id = self.handle_pointer_move(event);
                let mut doc = self.doc.inner_mut();
                doc.unactive_node();
                doc.click_origins.retain(|(id, _, _)| *id != event.id);

                if event.is_primary && matches!(event.id, BlitzPointerId::Finger(_)) {
                    hover_end_event = Some(event.clone());
                }
            }
            UiEvent::PointerLeave(event) => {
                self.handle_pointer_leave(event);
                hover_node_id = None;
            }
            _ => {}
        };

        let target = match event {
            UiEvent::PointerMove(_) => hover_node_id,
            UiEvent::PointerUp(_) => hover_node_id,
            UiEvent::PointerDown(_) => hover_node_id,
            UiEvent::PointerCancel(_) => hover_node_id,
            UiEvent::PointerLeave(_) => hover_node_id,
            UiEvent::Wheel(_) => hover_node_id,
            UiEvent::KeyUp(_) => focussed_node_id,
            UiEvent::KeyDown(_) => focussed_node_id,
            UiEvent::Ime(_) => focussed_node_id,
            UiEvent::AppleStandardKeybinding(_) => focussed_node_id,
        };
        // Fall back to the root element. A document without a root element (e.g. an
        // empty iframe sub-document) has no event target, so there is nothing to do.
        let Some(target) = target.or_else(|| self.doc.inner().try_root_element().map(|el| el.id))
        else {
            return;
        };

        match event {
            UiEvent::PointerMove(data) => {
                self.handle_pointer_event(
                    target,
                    data,
                    DomEventData::PointerMove,
                    Some(DomEventData::MouseMove),
                    DomEventData::TouchMove,
                );
            }
            UiEvent::PointerUp(data) => {
                // The token is this dispatch's alone: a release dispatched from
                // one of its handlers has none and can't use or end this press.
                let release = released.map(|node| {
                    let mut doc = self.doc.inner_mut();
                    doc.next_release += 1;
                    let token = doc.next_release;
                    doc.pending_releases.push((token, node));
                    token
                });
                self.handle_pointer_event_releasing(
                    target,
                    data,
                    DomEventData::PointerUp,
                    Some(DomEventData::MouseUp),
                    DomEventData::TouchEnd,
                    release,
                );
                // Clicked from or cancelled, the press is over. An unwind
                // skips this, leaving an entry no dispatch will ask for again.
                if let Some(token) = release {
                    self.doc
                        .inner_mut()
                        .pending_releases
                        .retain(|(t, _)| *t != token);
                }
            }
            UiEvent::PointerDown(data) => {
                self.handle_pointer_event(
                    target,
                    data,
                    DomEventData::PointerDown,
                    Some(DomEventData::MouseDown),
                    DomEventData::TouchStart,
                );
            }
            UiEvent::PointerCancel(data) => {
                // `pointercancel` has no mouse-compatibility event, but does
                // generate a `touchcancel` for touch-like inputs.
                self.handle_pointer_event(
                    target,
                    data,
                    DomEventData::PointerCancel,
                    None::<fn(BlitzPointerEvent) -> DomEventData>,
                    DomEventData::TouchCancel,
                );
            }
            // Out/leave events were dispatched by `handle_pointer_leave`
            UiEvent::PointerLeave(_) => {}
            UiEvent::Wheel(data) => {
                self.handle_dom_event(DomEvent::new(target, DomEventData::Wheel(data)))
            }
            UiEvent::KeyUp(data) => {
                self.handle_dom_event(DomEvent::new(target, DomEventData::KeyUp(data)))
            }
            UiEvent::KeyDown(data) => {
                self.handle_dom_event(DomEvent::new(target, DomEventData::KeyDown(data)))
            }
            UiEvent::Ime(data) => {
                self.handle_dom_event(DomEvent::new(target, DomEventData::Ime(data)))
            }
            UiEvent::AppleStandardKeybinding(data) => {
                let mut dom_event =
                    DomEvent::new(target, DomEventData::AppleStandardKeybinding(data));
                self.run_default_action(&mut dom_event);
                self.process_queue();
            }
        };

        // Update document input state (hover, focus, active, etc)
        if let Some(event) = hover_end_event {
            self.handle_pointer_leave(&event);
        }

        // Update the cursor once the event (including any forwarding to a
        // sub-document) has been fully handled
        let doc = self.doc.inner();
        if !is_leave && doc.hover_state() != prev_hover {
            doc.update_cursor();
        }
    }

    pub fn handle_dom_event(&mut self, event: DomEvent) {
        self.queue.push_back(event);
        self.process_queue();
    }

    fn handle_pointer_event(
        &mut self,
        target: NodeId,
        data: BlitzPointerEvent,
        make_ptr_data: impl FnOnce(BlitzPointerEvent) -> DomEventData,
        make_mouse_data: Option<impl FnOnce(BlitzPointerEvent) -> DomEventData>,
        make_touch_data: impl FnOnce(BlitzPointerEvent) -> DomEventData,
    ) {
        self.handle_pointer_event_releasing(
            target,
            data,
            make_ptr_data,
            make_mouse_data,
            make_touch_data,
            None,
        );
    }

    /// [`Self::handle_pointer_event`], with the token of the press a physical
    /// release ends, for its default action alone.
    fn handle_pointer_event_releasing(
        &mut self,
        target: NodeId,
        data: BlitzPointerEvent,
        make_ptr_data: impl FnOnce(BlitzPointerEvent) -> DomEventData,
        make_mouse_data: Option<impl FnOnce(BlitzPointerEvent) -> DomEventData>,
        make_touch_data: impl FnOnce(BlitzPointerEvent) -> DomEventData,
        release: Option<u64>,
    ) {
        let mut ptr_event = DomEvent::new(target, make_ptr_data(data.clone()));
        let mut event_state = EventState::default();
        event_state = self.run_handler_event(&mut ptr_event, event_state);

        // Generate the corresponding compatibility event (mouse events for the
        // mouse, touch events for fingers and pen/stylus input) and expose it to
        // script. The default action is always run on the pointer event so that
        // the shell layer and default actions remain pointer-based.
        //
        // `pointercancel` has no mouse equivalent, so `make_mouse_data` is `None`
        // in that case and no mouse event is generated.
        if !event_state.is_cancelled() {
            if data.is_mouse() {
                if let Some(make_mouse_data) = make_mouse_data {
                    let mut mouse_event = DomEvent::new(target, make_mouse_data(data));
                    event_state = self.run_handler_event(&mut mouse_event, event_state);
                }
            } else if data.is_finger() || data.is_pen() {
                let mut touch_event = DomEvent::new(target, make_touch_data(data));
                event_state = self.run_handler_event(&mut touch_event, event_state);
            }
        }

        if !event_state.is_cancelled() {
            let mut doc = self.doc.inner_mut();
            doc.handle_release_dom_event(&mut ptr_event, release, |new_evt| {
                self.queue.push_back(new_evt)
            });
        }
        self.process_queue();
    }

    fn process_queue(&mut self) {
        while let Some(mut event) = self.queue.pop_front() {
            let event_state = self.run_handler_event(&mut event, EventState::default());
            if !event_state.is_cancelled() {
                self.run_default_action(&mut event);
            }
        }
    }

    fn adjust_element_coords(
        &self,
        target: NodeId,
        coords: &PointerCoords,
        element: &mut Point<f32>,
    ) {
        if let Some(rect) = self.doc.inner().get_client_bounding_rect(target) {
            element.x = coords.client_x - rect.x as f32;
            element.y = coords.client_y - rect.y as f32;
        }
    }

    fn run_handler_event(
        &mut self,
        event: &mut DomEvent,
        initial_event_state: EventState,
    ) -> EventState {
        let chain = if event.bubbles {
            let doc = self.doc.inner();
            doc.node_chain(event.target)
        } else {
            vec![event.target]
        };

        match &mut event.data {
            DomEventData::PointerMove(data)
            | DomEventData::PointerDown(data)
            | DomEventData::PointerUp(data)
            | DomEventData::PointerCancel(data)
            | DomEventData::PointerEnter(data)
            | DomEventData::PointerLeave(data)
            | DomEventData::PointerOver(data)
            | DomEventData::PointerOut(data)
            | DomEventData::MouseMove(data)
            | DomEventData::MouseDown(data)
            | DomEventData::MouseUp(data)
            | DomEventData::MouseEnter(data)
            | DomEventData::MouseLeave(data)
            | DomEventData::MouseOver(data)
            | DomEventData::MouseOut(data)
            | DomEventData::TouchStart(data)
            | DomEventData::TouchEnd(data)
            | DomEventData::TouchMove(data)
            | DomEventData::TouchCancel(data)
            | DomEventData::Click(data)
            | DomEventData::ContextMenu(data)
            | DomEventData::DoubleClick(data) => {
                self.adjust_element_coords(event.target, &data.coords, &mut data.element)
            }
            DomEventData::Wheel(data) => {
                self.adjust_element_coords(event.target, &data.coords, &mut data.element)
            }
            _ => {}
        }

        let mut event_state = initial_event_state;
        self.handler
            .handle_event(&chain, event, self.doc, &mut event_state);

        event_state
    }

    fn run_default_action(&mut self, event: &mut DomEvent) {
        let mut doc = self.doc.inner_mut();
        doc.handle_dom_event(event, |new_evt| self.queue.push_back(new_evt));
    }
}
