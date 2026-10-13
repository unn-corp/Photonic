use super::*;

/// Reserve right-hand controls before laying out bounded document/status text.
pub fn draw_toolbar(
    ui: &mut Ui,
    doc_name: &str,
    zoom: f64,
    file_status: Option<&str>,
    logo: Option<&egui::TextureHandle>,
) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // Quit menu — explicit, discoverable shutdown (the window's close
        // button works too; this also offers killing every instance).
        ui.menu_button(format!("Quit {}", ph::CARET_DOWN), |ui| {
            if ui.button("Quit Photonic").clicked() {
                ui.close_menu();
                crate::quit::quit_self();
            }
            ui.separator();
            if ui
                .button("Quit ALL Photonic processes")
                .on_hover_text(
                    "Closes every Photonic window and any headless MCP server. \
                         Unsaved changes in other windows will be lost.",
                )
                .clicked()
            {
                ui.close_menu();
                crate::quit::quit_all();
            }
        });
        ui.separator();
        ui.label(format!("{:.0}%", zoom * 100.0));
        ui.label("Zoom:");

        let available = ui.available_width().max(0.0);
        ui.allocate_ui_with_layout(
            egui::vec2(available, ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                if available >= 240.0 {
                    if let Some(tex) = logo {
                        ui.add(
                            egui::Image::new(egui::load::SizedTexture::new(
                                tex.id(),
                                egui::vec2(18.0, 18.0),
                            ))
                            .maintain_aspect_ratio(true),
                        );
                        ui.label(
                            RichText::new("Photonic")
                                .strong()
                                .color(Color32::from_rgb(110, 86, 207)),
                        );
                    } else {
                        ui.label(
                            RichText::new(format!("{} Photonic", ph::HEXAGON))
                                .strong()
                                .color(Color32::from_rgb(110, 86, 207)),
                        );
                    }
                    ui.separator();
                }
                let width = ui.available_width().max(0.0);
                let height = ui.spacing().interact_size.y;
                if let Some(status) = file_status {
                    if width >= 180.0 {
                        let title_width = width * 0.45;
                        ui.add_sized([title_width, height], egui::Label::new(doc_name).truncate());
                        let remaining = ui.available_width().max(0.0);
                        ui.add_sized(
                            [remaining, height],
                            egui::Label::new(RichText::new(status).weak().italics()).truncate(),
                        );
                    } else {
                        ui.add_sized(
                            [width, height],
                            egui::Label::new(
                                RichText::new(format!("{status} · {doc_name}"))
                                    .weak()
                                    .italics(),
                            )
                            .truncate(),
                        );
                    }
                } else {
                    ui.add_sized([width, height], egui::Label::new(doc_name).truncate());
                }
            },
        );
    });
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    fn text_rects(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Rect)> {
        fn collect(s: &egui::epaint::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match s {
                egui::epaint::Shape::Text(t) => out.push((
                    t.galley.text().to_owned(),
                    egui::Rect::from_min_size(t.pos, t.galley.size()),
                )),
                egui::epaint::Shape::Vec(v) => {
                    for s in v {
                        collect(s, out)
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for s in shapes {
            collect(&s.shape, &mut out);
        }
        out
    }
    #[test]
    fn toolbar_long_text_stays_inside_reserved_controls() {
        let name =
            "A deliberately very long document title with enough words to overflow the toolbar";
        for width in [220.0, 280.0, 420.0, 700.0] {
            for status in [
                None,
                Some("Saved a very long document file successfully at the selected location"),
            ] {
                let ctx = egui::Context::default();
                let frame = || {
                    ctx.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, 160.0),
                            )),
                            ..Default::default()
                        },
                        |ctx| {
                            egui::CentralPanel::default()
                                .show(ctx, |ui| draw_toolbar(ui, name, 7.31, status, None));
                        },
                    )
                };
                frame();
                let out = frame();
                let texts = text_rects(&out.shapes);
                let quit = texts
                    .iter()
                    .find(|(s, _)| s.starts_with("Quit "))
                    .expect("Quit remains visible");
                let zoom = texts
                    .iter()
                    .find(|(s, _)| s == "Zoom:")
                    .expect("Zoom remains visible");
                assert!(quit.1.right() <= width && zoom.1.left() >= 0.0);
                for (s, rect) in &texts {
                    if s.contains(name) || status.is_some_and(|status| s.starts_with(status)) {
                        assert!(
                            rect.left() >= 0.0 && rect.right() <= zoom.1.left(),
                            "{width}/{s}: {rect:?} vs {:?}",
                            zoom.1
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn toolbar_quit_menu_remains_clickable_and_escape_dismisses() {
        let ctx = egui::Context::default();
        let frame = |events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(280.0, 200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        draw_toolbar(
                            ui,
                            "Long document title for menu verification",
                            1.0,
                            None,
                            None,
                        )
                    });
                },
            )
        };
        frame(vec![]);
        let out = frame(vec![]);
        let p = text_rects(&out.shapes)
            .into_iter()
            .find(|(s, _)| s.starts_with("Quit "))
            .unwrap()
            .1
            .center();
        frame(vec![
            egui::Event::PointerMoved(p),
            egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ]);
        frame(vec![egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }]);
        let open = frame(vec![]);
        assert!(text_rects(&open.shapes)
            .iter()
            .any(|(s, _)| s == "Quit Photonic"));
        frame(vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }]);
        let closed = frame(vec![]);
        assert!(!text_rects(&closed.shapes)
            .iter()
            .any(|(s, _)| s == "Quit Photonic"));
    }
    #[test]
    fn toolbar_elided_title_has_one_full_text_tooltip() {
        let ctx = egui::Context::default();
        let name =
            "A deliberately long title whose complete text must appear in exactly one tooltip";
        let frame = |time, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(280.0, 160.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .show(ctx, |ui| draw_toolbar(ui, name, 7.31, None, None));
                },
            )
        };
        frame(0.0, vec![]);
        let out = frame(0.1, vec![]);
        let p = text_rects(&out.shapes)
            .into_iter()
            .find(|(s, _)| s == name)
            .unwrap()
            .1
            .center();
        frame(0.2, vec![egui::Event::PointerMoved(p)]);
        frame(2.0, vec![]);
        let hovered = frame(3.0, vec![]);
        assert_eq!(
            text_rects(&hovered.shapes)
                .iter()
                .filter(|(s, _)| s == name)
                .count(),
            2,
            "one elided label plus one tooltip"
        );
    }
}
