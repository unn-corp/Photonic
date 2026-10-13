use super::*;
struct Harness {
    ctx: egui::Context,
    value: f64,
    changes: usize,
    time: f64,
    text_pos: egui::Pos2,
    slider_id: Option<egui::Id>,
    track_rect: egui::Rect,
    enabled: bool,
}
impl Harness {
    fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            value: 16.0,
            changes: 0,
            time: 0.0,
            text_pos: egui::Pos2::ZERO,
            slider_id: None,
            track_rect: egui::Rect::NOTHING,
            enabled: true,
        }
    }
    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    fn frame(&mut self, events: Vec<egui::Event>, owner: u64) {
        self.time += 0.02;
        let old = self.value;
        let output = self.ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 300.0),
                )),
                time: Some(self.time),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.push_id(owner, |ui| {
                        if !self.enabled {
                            ui.disable();
                        }
                        let mut proposed = self.value;
                        let response = numeric_slider(ui, &mut proposed, 0.0..=256.0, |s| {
                            s.clamping(egui::SliderClamping::Edits)
                                .fixed_decimals(2)
                                .step_by(1.0)
                        });
                        self.slider_id = Some(response.id);
                        self.track_rect = response.rect;
                        if response.changed() {
                            self.value = proposed;
                            self.changes += 1;
                        }
                        let _ = ui.button("Other control");
                    });
                });
            },
        );
        fn find(shape: &egui::Shape, wanted: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == wanted => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, wanted)),
                _ => None,
            }
        }
        if let Some(pos) = output
            .shapes
            .iter()
            .find_map(|s| find(&s.shape, &format!("{old:.2}")))
        {
            self.text_pos = pos;
        }
    }
    fn begin(&mut self, owner: u64) {
        self.frame(vec![], owner);
        self.frame(vec![], owner);
        assert_ne!(self.text_pos, egui::Pos2::ZERO);
        let pos = self.text_pos;
        self.frame(
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            owner,
        );
        self.frame(
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            owner,
        );
        self.frame(
            vec![Self::key(
                egui::Key::A,
                egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..egui::Modifiers::NONE
                },
            )],
            owner,
        );
    }
}
#[test]
fn raw_numeric_slider_text_commits_once_and_escape_cancels() {
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("3".into())], 1);
    assert_eq!((h.value, h.changes), (16.0, 0));
    h.frame(vec![egui::Event::Text("2".into())], 1);
    assert_eq!((h.value, h.changes), (16.0, 0));
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        1,
    );
    h.frame(vec![], 1);
    assert_eq!((h.value, h.changes), (32.0, 1));
    h.begin(1);
    h.frame(vec![egui::Event::Text("47".into())], 1);
    h.frame(
        vec![Harness::key(egui::Key::Escape, egui::Modifiers::NONE)],
        1,
    );
    h.frame(vec![], 1);
    assert_eq!((h.value, h.changes), (32.0, 1));
}
#[test]
fn raw_numeric_slider_focus_loss_target_isolation_and_clamp_semantics() {
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("64".into())], 1);
    assert_eq!((h.value, h.changes), (16.0, 0));
    h.frame(vec![Harness::key(egui::Key::Tab, egui::Modifiers::NONE)], 1);
    h.frame(vec![], 1);
    assert_eq!((h.value, h.changes), (64.0, 1));
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("47".into())], 1);
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        2,
    );
    h.frame(vec![], 2);
    assert_eq!((h.value, h.changes), (16.0, 0));
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("999".into())], 1);
    assert_eq!((h.value, h.changes), (16.0, 0));
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        1,
    );
    h.frame(vec![], 1);
    assert_eq!((h.value, h.changes), (256.0, 1));
}
#[test]
fn raw_numeric_slider_keyboard_and_pointer_scrubbing_remain_live() {
    let mut h = Harness::new();
    h.frame(vec![], 1);
    h.frame(vec![], 1);
    h.ctx.memory_mut(|m| m.request_focus(h.slider_id.unwrap()));
    // Let the focused egui slider install its navigation filter before the
    // first key event, just as it does on the frame receiving pointer focus.
    h.frame(vec![], 1);
    assert!(h.ctx.memory(|m| m.has_focus(h.slider_id.unwrap())));
    h.frame(
        vec![Harness::key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
        1,
    );
    assert!(
        h.value > 16.0 && h.changes > 0,
        "keyboard handle must stay live: {} {}",
        h.value,
        h.changes
    );
    for _ in 0..3 {
        let previous = h.value;
        let mut release = Harness::key(egui::Key::ArrowRight, egui::Modifiers::NONE);
        if let egui::Event::Key { pressed, .. } = &mut release {
            *pressed = false;
        }
        h.frame(vec![release], 1);
        h.frame(
            vec![Harness::key(egui::Key::ArrowRight, egui::Modifiers::NONE)],
            1,
        );
        assert!(
            h.value > previous,
            "repeated arrow nudge: previous={previous} actual={} focused={:?} slider={:?}",
            h.value,
            h.ctx.memory(|m| m.focused()),
            h.slider_id
        );
        assert!(h.ctx.memory(|m| m.has_focus(h.slider_id.unwrap())));
    }
    let mut h = Harness::new();
    h.frame(vec![], 1);
    h.frame(vec![], 1);
    let pos = egui::pos2(h.track_rect.left() + 70.0, h.track_rect.center().y);
    h.frame(
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        1,
    );
    h.frame(
        vec![egui::Event::PointerMoved(pos + egui::vec2(10.0, 0.0))],
        1,
    );
    assert!(
        h.value > 16.0 && h.changes > 0,
        "pointer handle must stay live"
    );
}
#[test]
fn raw_numeric_slider_invalid_suffix_same_frame_enter_and_external_update() {
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("32".into())], 1);
    h.frame(vec![egui::Event::Text("q".into())], 1);
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        1,
    );
    h.frame(vec![], 1);
    assert_eq!((h.value, h.changes), (16.0, 0));
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("3".into())], 1);
    h.frame(
        vec![
            egui::Event::Text("2".into()),
            Harness::key(egui::Key::Enter, egui::Modifiers::NONE),
        ],
        1,
    );
    h.frame(vec![], 1);
    assert_eq!((h.value, h.changes), (32.0, 1));
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("47".into())], 1);
    h.value = 64.0;
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        1,
    );
    h.frame(vec![], 1);
    assert_eq!((h.value, h.changes), (64.0, 0));
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("32".into())], 1);
    h.frame(vec![Harness::key(egui::Key::Tab, egui::Modifiers::NONE)], 1);
    h.frame(vec![], 1);
    assert_eq!(h.value, 32.0);
    h.begin(1);
    h.frame(vec![egui::Event::Text("48".into())], 1);
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        1,
    );
    h.frame(vec![], 1);
    assert_eq!((h.value, h.changes), (48.0, 2));
}
#[test]
fn raw_numeric_slider_disabled_scope_cancels_pending_text() {
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("47".into())], 1);
    h.enabled = false;
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        1,
    );
    h.frame(vec![], 1);
    assert_eq!((h.value, h.changes), (16.0, 0));
    h.enabled = true;
    h.frame(vec![], 1);
    h.begin(1);
    h.frame(vec![egui::Event::Text("32".into())], 1);
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        1,
    );
    h.frame(vec![], 1);
    assert_eq!((h.value, h.changes), (32.0, 1));
}
