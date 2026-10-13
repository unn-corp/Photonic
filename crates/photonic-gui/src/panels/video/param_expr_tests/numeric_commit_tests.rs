use super::*;

struct Harness {
    ctx: egui::Context,
    value: f64,
    changes: usize,
    time: f64,
}
impl Harness {
    fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            value: 16.0,
            changes: 0,
            time: 0.0,
        }
    }
    fn frame(&mut self, events: Vec<egui::Event>, focus: bool, owner: u64) {
        self.time += 0.02;
        let _ = self.ctx.run(
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
                        if focus {
                            let id = ui.next_auto_id();
                            ui.memory_mut(|m| m.request_focus(id));
                        }
                        let mut proposed = self.value;
                        if float_drag(
                            ui,
                            &mut proposed,
                            8.0,
                            Some((1.0, 256.0)),
                            &vars_from_params([], Some(320.0), Some(240.0)),
                            0.01,
                        ) {
                            self.value = proposed;
                            self.changes += 1;
                        }
                        let _ = ui.button("Other control");
                    });
                });
            },
        );
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
    fn begin(&mut self, owner: u64) {
        self.frame(vec![], true, owner);
        self.frame(
            vec![Self::key(
                egui::Key::A,
                egui::Modifiers {
                    ctrl: true,
                    command: true,
                    ..egui::Modifiers::NONE
                },
            )],
            false,
            owner,
        );
    }
}

#[test]
fn numeric_text_commits_once_on_enter_and_separate_edits_stay_separate() {
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("3".into())], false, 1);
    assert_eq!(
        (h.value, h.changes),
        (16.0, 0),
        "numeric prefix must stay an uncommitted draft"
    );
    h.frame(vec![egui::Event::Text("2".into())], false, 1);
    assert_eq!((h.value, h.changes), (16.0, 0));
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        false,
        1,
    );
    h.frame(vec![], false, 1);
    h.frame(vec![], false, 1);
    assert_eq!((h.value, h.changes), (32.0, 1));
    h.begin(1);
    h.frame(vec![egui::Event::Text("%w/10+2".into())], false, 1);
    assert_eq!((h.value, h.changes), (32.0, 1));
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        false,
        1,
    );
    h.frame(vec![], false, 1);
    assert_eq!((h.value, h.changes), (34.0, 2));
}

#[test]
fn numeric_text_escape_invalid_and_owner_switch_do_not_commit() {
    for text in ["47", "999", "1/0"] {
        let mut h = Harness::new();
        h.begin(1);
        h.frame(vec![egui::Event::Text(text.into())], false, 1);
        assert_eq!((h.value, h.changes), (16.0, 0));
        let key = if text == "47" {
            egui::Key::Escape
        } else {
            egui::Key::Enter
        };
        h.frame(vec![Harness::key(key, egui::Modifiers::NONE)], false, 1);
        h.frame(vec![], false, 1);
        h.frame(vec![], false, 1);
        assert_eq!((h.value, h.changes), (16.0, 0), "{text}");
    }
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("47".into())], false, 1);
    h.frame(
        vec![Harness::key(egui::Key::Enter, egui::Modifiers::NONE)],
        false,
        2,
    );
    h.frame(vec![], false, 2);
    assert_eq!((h.value, h.changes), (16.0, 0));
}

#[test]
fn numeric_text_focus_loss_commits_once() {
    let mut h = Harness::new();
    h.begin(1);
    h.frame(vec![egui::Event::Text("64".into())], false, 1);
    assert_eq!((h.value, h.changes), (16.0, 0));
    h.frame(
        vec![Harness::key(egui::Key::Tab, egui::Modifiers::NONE)],
        false,
        1,
    );
    h.frame(vec![], false, 1);
    h.frame(vec![], false, 1);
    assert_eq!((h.value, h.changes), (64.0, 1));
}
#[test]
fn numeric_drag_stays_live_and_middle_click_resets() {
    let mut h = Harness::new();
    h.frame(vec![], false, 1);
    h.frame(vec![], false, 1);
    let start = egui::pos2(24.0, 18.0);
    let button = |pos, button, pressed| egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.frame(
        vec![
            egui::Event::PointerMoved(start),
            button(start, egui::PointerButton::Primary, true),
        ],
        false,
        1,
    );
    h.frame(
        vec![egui::Event::PointerMoved(start + egui::vec2(80.0, 0.0))],
        false,
        1,
    );
    h.frame(
        vec![egui::Event::PointerMoved(start + egui::vec2(100.0, 0.0))],
        false,
        1,
    );
    assert!(
        h.value > 16.0 && h.changes > 0,
        "drag must update before pointer release: {} {}",
        h.value,
        h.changes
    );
    h.frame(
        vec![button(
            start + egui::vec2(100.0, 0.0),
            egui::PointerButton::Primary,
            false,
        )],
        false,
        1,
    );
    h.frame(
        vec![
            egui::Event::PointerMoved(start),
            button(start, egui::PointerButton::Middle, true),
        ],
        false,
        1,
    );
    h.frame(
        vec![button(start, egui::PointerButton::Middle, false)],
        false,
        1,
    );
    assert_eq!(h.value, 8.0);
}
