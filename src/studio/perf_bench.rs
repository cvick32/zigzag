//! Frame-time benchmarks for the gestures that must feel smooth: typing
//! on the canvas and dragging objects, on a traced logo (hundreds of
//! shapes, tens of thousands of stitches). Each figure is a whole frame:
//! the UI pass plus egui's tessellation of what it painted, which is
//! where directly drawn stitches cost.
//!
//! `cargo test --release --features gui --bin zigzag-studio perf_bench -- --ignored --nocapture`

#![cfg(test)]

use crate::app::Studio;
use eframe::egui::{self, Vec2};
use egui_kittest::Harness;
use zigzag::geom::P;

fn studio_with_logo() -> Harness<'static, Studio> {
    let png = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/agent-badge-preview.png"
    ));
    let mut h = Harness::builder()
        .with_size(Vec2::new(1600.0, 1000.0))
        .build_ui_state(
            |ui, s: &mut Studio| s.frame(ui),
            Studio::with_file(None, None),
        );
    // As the app does it: a slow plan moves planning to the worker.
    h.state_mut().planner.timed = true;
    let doc = zigzag::document::Document::open(&png).unwrap();
    h.state_mut().set_document(doc);
    h.run_steps(5);
    h
}

/// One frame, tessellated: milliseconds.
fn frame_ms(
    h: &mut Harness<'static, Studio>,
    before: impl FnOnce(&mut Harness<'static, Studio>),
) -> f64 {
    before(h);
    let t = std::time::Instant::now();
    h.step();
    let shapes = h.output().shapes.clone();
    let _ = h.ctx.tessellate(shapes, 1.0);
    t.elapsed().as_secs_f64() * 1000.0
}

fn report(what: &str, ms: &[f64]) {
    let mean = ms.iter().sum::<f64>() / ms.len() as f64;
    let max = ms.iter().cloned().fold(0.0, f64::max);
    println!("{what:<34} mean {mean:6.2} ms   max {max:6.2} ms");
}

#[test]
#[ignore]
fn typing_and_dragging_on_a_traced_logo() {
    let mut h = studio_with_logo();
    println!(
        "{} shapes, {} stitches",
        h.state().doc.shapes.len(),
        h.state().design.stitch_count()
    );
    let rect = h.state().canvas_rect;

    // Idle, nothing happening.
    let idle: Vec<f64> = (0..10).map(|_| frame_ms(&mut h, |_| {})).collect();
    report("idle", &idle);

    // Type tool: click on the canvas, type a word.
    h.key_press(egui::Key::T);
    h.run_steps(1);
    let at = h.state().view.to_screen(rect, P::new(0.0, 0.0));
    h.hover_at(at);
    h.run_steps(1);
    h.drag_at(at);
    h.run_steps(1);
    h.drop_at(at);
    h.run_steps(2);
    let mut keys = Vec::new();
    for ch in "CAMPFIRE".chars() {
        // The keystroke's frame plus the one after it (the replan lands
        // on the next frame).
        let a = frame_ms(&mut h, |h| h.event(egui::Event::Text(ch.to_string())));
        let b = frame_ms(&mut h, |_| {});
        keys.push(a + b);
    }
    report("keystroke (2 frames)", &keys);
    let idle_typing: Vec<f64> = (0..10).map(|_| frame_ms(&mut h, |_| {})).collect();
    report("idle while typing", &idle_typing);
    // Mouse moving over the canvas while typing (hover repaints).
    let hover_typing: Vec<f64> = (0..10)
        .map(|k| {
            frame_ms(&mut h, |h| {
                h.hover_at(at + Vec2::new(40.0 + k as f32 * 5.0, 60.0))
            })
        })
        .collect();
    report("hover while typing", &hover_typing);
    if std::env::var_os("TL_PROFILE").is_some() {
        eprintln!("-- profiled: a keystroke, the frame after, then idle while typing");
        let _ = frame_ms(&mut h, |h| h.event(egui::Event::Text("S".into())));
        let _ = frame_ms(&mut h, |_| {});
        let _ = frame_ms(&mut h, |_| {});
        let _ = frame_ms(&mut h, |_| {});
    }
    h.key_press(egui::Key::Escape);
    h.run_steps(2);

    // Selection tool: drag the biggest shape.
    h.key_press(egui::Key::V);
    h.run_steps(1);
    let big = (0..h.state().doc.shapes.len())
        .max_by_key(|&i| {
            h.state()
                .doc
                .edit_path(i)
                .map(|p| p[0].anchors.len())
                .unwrap_or(0)
        })
        .unwrap();
    h.state_mut().set_selection(&[big]);
    h.run_steps(2);
    let c = h.state().view.to_screen(
        rect,
        crate::app::shape_bounds(&h.state().doc.shapes[big]).center(),
    );
    h.hover_at(c);
    h.run_steps(1);
    h.drag_at(c);
    h.run_steps(1);
    let drag: Vec<f64> = (1..=20)
        .map(|k| {
            frame_ms(&mut h, |h| {
                h.hover_at(c + Vec2::new(k as f32 * 3.0, k as f32))
            })
        })
        .collect();
    report("drag biggest shape", &drag);
    h.drop_at(c + Vec2::new(60.0, 20.0));
    h.run_steps(3);

    // Drag everything.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(2);
    let n = h.state().sel_indices().len();
    let c = h
        .state()
        .view
        .to_screen(rect, h.state().selection_bounds().center());
    h.hover_at(c);
    h.run_steps(1);
    h.drag_at(c);
    h.run_steps(1);
    let drag_all: Vec<f64> = (1..=20)
        .map(|k| frame_ms(&mut h, |h| h.hover_at(c + Vec2::new(k as f32 * 3.0, 0.0))))
        .collect();
    report(&format!("drag all ({n} selected)"), &drag_all);
    if std::env::var_os("TL_PROFILE").is_some() {
        eprintln!("-- profiled: dragging everything");
        let _ = frame_ms(&mut h, |h| h.hover_at(c + Vec2::new(70.0, 0.0)));
        let _ = frame_ms(&mut h, |h| h.hover_at(c + Vec2::new(80.0, 0.0)));
    }
    h.drop_at(c + Vec2::new(60.0, 0.0));
    h.run_steps(3);

    // Pan.
    h.key_press(egui::Key::H);
    h.run_steps(1);
    let p0 = rect.center();
    h.hover_at(p0);
    h.run_steps(1);
    h.drag_at(p0);
    h.run_steps(1);
    let pan: Vec<f64> = (1..=10)
        .map(|k| frame_ms(&mut h, |h| h.hover_at(p0 + Vec2::new(k as f32 * 4.0, 0.0))))
        .collect();
    report("pan", &pan);
    h.drop_at(p0 + Vec2::new(40.0, 0.0));
}
