//! A few immediate-mode widgets for the editor's panels: buttons, choices, sliders.
//! Panels are laid out before the map handles the mouse, so a click lands on one or the other.

use macroquad::prelude::*;

pub const BG: Color = Color::new(0.09, 0.1, 0.12, 0.95);
const IDLE: Color = Color::new(0.2, 0.22, 0.26, 1.0);
const HOVER: Color = Color::new(0.28, 0.31, 0.36, 1.0);
const ON: Color = Color::new(0.85, 0.65, 0.1, 1.0);
pub const TEXT: f32 = 16.0;

pub struct Ui {
    pub mouse: Vec2,
    pressed: bool,
    down: bool,
    /// The slider being dragged (by label), which keeps the mouse even off the panel.
    drag: Option<String>,
}

impl Ui {
    pub fn new() -> Ui {
        Ui {
            mouse: Vec2::ZERO,
            pressed: false,
            down: false,
            drag: None,
        }
    }

    /// Once per frame, before anything asks.
    pub fn begin(&mut self) {
        self.mouse = mouse_position().into();
        self.pressed = is_mouse_button_pressed(MouseButton::Left);
        self.down = is_mouse_button_down(MouseButton::Left);
        if !self.down {
            self.drag = None;
        }
    }

    pub fn busy(&self) -> bool {
        self.drag.is_some()
    }

    pub fn button(&mut self, r: Rect, label: &str, on: bool) -> bool {
        let hover = r.contains(self.mouse);
        let fill = match (on, hover) {
            (true, _) => ON,
            (_, true) => HOVER,
            _ => IDLE,
        };
        draw_rectangle(r.x, r.y, r.w, r.h, fill);
        let d = measure_text(label, None, TEXT as u16, 1.0);
        let ink = if on { BLACK } else { WHITE };
        text_sized(
            label,
            r.x + (r.w - d.width) / 2.0,
            r.y + (r.h + d.offset_y) / 2.0 - 1.0,
            TEXT,
            ink,
        );
        hover && self.pressed
    }

    /// A row of buttons, one of which is chosen.
    pub fn choice<T: Copy + PartialEq>(&mut self, r: Rect, items: &[(T, &str)], v: &mut T) {
        let w = r.w / items.len() as f32;
        for (i, &(item, label)) in items.iter().enumerate() {
            let b = Rect::new(r.x + i as f32 * w, r.y, w - 2.0, r.h);
            if self.button(b, label, *v == item) {
                *v = item;
            }
        }
    }

    /// A labelled slider over `min..=max`.
    pub fn slider(&mut self, r: Rect, label: &str, v: &mut i32, min: i32, max: i32) {
        if self.pressed && r.contains(self.mouse) {
            self.drag = Some(label.into());
        }
        if self.drag.as_deref() == Some(label) {
            let t = ((self.mouse.x - r.x) / r.w).clamp(0.0, 1.0);
            *v = min + (t * (max - min) as f32).round() as i32;
        }
        draw_rectangle(r.x, r.y, r.w, r.h, IDLE);
        let t = (*v - min) as f32 / (max - min).max(1) as f32;
        draw_rectangle(r.x, r.y, r.w * t, r.h, Color::new(0.45, 0.36, 0.08, 1.0));
        text_sized(
            format!("{label}: {v}"),
            r.x + 6.0,
            r.y + r.h / 2.0 + 5.0,
            TEXT,
            WHITE,
        );
    }
}

pub fn panel(r: Rect) {
    draw_rectangle(r.x, r.y, r.w, r.h, BG);
}

pub fn text(s: &str, x: f32, y: f32, c: Color) {
    text_sized(s, x, y, TEXT, c);
}

/// Text on whole device pixels: placed between them (centred labels land on halves), its thin
/// strokes blur away on a 1× screen.
pub fn text_sized(s: impl AsRef<str>, x: f32, y: f32, size: f32, c: Color) {
    let snap = |v: f32| (v * screen_dpi_scale()).round() / screen_dpi_scale();
    draw_text(s.as_ref(), snap(x), snap(y), size, c);
}
