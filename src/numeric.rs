use eframe::egui::{self, Pos2, Response, Ui, Widget};
use std::ops::RangeInclusive;

pub struct DragValue<'a> {
    value: &'a mut f64,
    speed: f64,
    range: RangeInclusive<f64>,
    suffix: String,
    decimals: Option<usize>,
}

#[derive(Clone)]
struct DragState {
    origin: Pos2,
    value: f64,
    slow: bool,
    dragged: bool,
}

impl DragState {
    fn new(origin: Pos2, value: f64, slow: bool) -> Self {
        Self {
            origin,
            value,
            slow,
            dragged: false,
        }
    }

    fn update(&mut self, position: Pos2, speed: f64) -> f64 {
        let delta = position - self.origin;
        let distance = f64::from(delta.x - delta.y);
        let offset = exponential_offset(distance, speed) * if self.slow { 0.1 } else { 1.0 };
        self.dragged = true;
        (self.value + offset).clamp(-f64::MAX, f64::MAX)
    }
}

fn exponential_offset(distance: f64, speed: f64) -> f64 {
    let rate = std::f64::consts::LN_2 / 50.0;
    let offset = speed / rate * (distance.abs() * rate).exp_m1();
    distance.signum() * offset.min(f64::MAX)
}

impl<'a> DragValue<'a> {
    pub fn new(value: &'a mut f64) -> Self {
        Self {
            value,
            speed: 1.0,
            range: f64::NEG_INFINITY..=f64::INFINITY,
            suffix: String::new(),
            decimals: None,
        }
    }

    pub fn speed(mut self, speed: f64) -> Self {
        self.speed = speed;
        self
    }

    pub fn range(mut self, range: RangeInclusive<f64>) -> Self {
        self.range = range;
        self
    }

    pub fn suffix(mut self, suffix: impl Into<String>) -> Self {
        self.suffix = suffix.into();
        self
    }

    pub fn fixed_decimals(mut self, decimals: usize) -> Self {
        self.decimals = Some(decimals);
        self
    }
}

impl Widget for DragValue<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let Self {
            value,
            speed,
            range,
            suffix,
            decimals,
        } = self;
        let before = *value;
        let id = ui.next_auto_id();
        let editing = ui.memory(|memory| memory.has_focus(id));
        let state_id = id.with("exponential-drag");
        let mut state = ui.data_mut(|data| data.get_temp::<DragState>(state_id));
        let (position, origin, pressed, released, down, slow, aim_radius) = ui.input(|input| {
            (
                input.pointer.interact_pos(),
                input.pointer.press_origin(),
                input.pointer.any_pressed(),
                input.pointer.any_released(),
                input.pointer.any_down(),
                input.modifiers.shift_only(),
                f64::from(input.aim_radius()),
            )
        });
        let dragging = ui.is_enabled()
            && !editing
            && (ui.ctx().is_being_dragged(id)
                || (released && state.as_ref().is_some_and(|state| state.dragged)));
        if dragging {
            if state.is_none() {
                if let Some(origin) = origin {
                    state = Some(DragState::new(origin, before, slow));
                }
            }
            if let (Some(state), Some(position)) = (&mut state, position) {
                let target = state.update(position, speed);
                let precision = decimals.unwrap_or_else(|| {
                    (aim_radius / (speed * if state.slow { 0.1 } else { 1.0 }))
                        .log10()
                        .ceil()
                        .clamp(0.0, 15.0) as usize
                });
                let target = if target == state.value {
                    target
                } else {
                    egui::emath::round_to_decimals(target, precision)
                };
                *value = target
                    .clamp(-f64::MAX, f64::MAX)
                    .clamp(*range.start(), *range.end());
            }
        }
        let mut native = egui::DragValue::from_get_set(|next| {
            if !dragging {
                if let Some(next) = next {
                    *value = next;
                }
            }
            *value
        })
        .speed(speed)
        .range(range)
        .suffix(suffix);
        if let Some(decimals) = decimals {
            native = native.fixed_decimals(decimals);
        }
        let mut response = native.ui(ui);
        if pressed
            && !editing
            && response.is_pointer_button_down_on()
            && !state.as_ref().is_some_and(|state| state.dragged)
        {
            if let Some(origin) = origin {
                state = Some(DragState::new(origin, *value, slow));
            }
        }
        if released || !down {
            ui.data_mut(|data| data.remove::<DragState>(state_id));
        } else if let Some(state) = state {
            ui.data_mut(|data| data.insert_temp(state_id, state));
        }
        if *value != before {
            response.mark_changed();
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2, Event, Modifiers, PointerButton, Rect};

    fn draw(ctx: &egui::Context, value: &mut f64, events: Vec<Event>) -> Response {
        let mut response = None;
        crate::test_ui(
            ctx,
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 400.0))),
                events,
                ..Default::default()
            },
            |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    response = Some(
                        ui.add(
                            DragValue::new(value)
                                .speed(0.1)
                                .range(0.0..=200.0)
                                .fixed_decimals(3),
                        ),
                    );
                });
            },
        );
        response.unwrap()
    }

    fn pointer(position: Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    #[test]
    fn exponential_sensitivity_doubles_every_fifty_pixels_in_both_directions() {
        for speed in [0.01, 0.1, 1.0] {
            let first = exponential_offset(50.0, speed);
            let second = exponential_offset(100.0, speed) - first;
            assert!((second / first - 2.0).abs() < 1e-12);
            assert_eq!(
                exponential_offset(-100.0, speed),
                -exponential_offset(100.0, speed)
            );
            assert!((exponential_offset(0.001, speed) / (speed * 0.001) - 1.0).abs() < 0.00001);
            assert!(exponential_offset(1e6, speed).is_finite());
        }
    }

    #[test]
    fn dragging_is_position_based_reversible_and_bounded_without_idle_changes() {
        let ctx = egui::Context::default();
        let mut value = 100.123456;
        let origin = draw(&ctx, &mut value, vec![]).rect.center();
        draw(
            &ctx,
            &mut value,
            vec![Event::PointerMoved(origin), pointer(origin, true)],
        );
        for distance in [8.0, 50.0, 100.0, 0.0, -50.0, 1000.0, -1000.0, 0.0] {
            let before = value;
            let response = draw(
                &ctx,
                &mut value,
                vec![Event::PointerMoved(origin + vec2(distance, 0.0))],
            );
            let expected = if distance == 0.0 {
                100.123456
            } else {
                egui::emath::round_to_decimals(
                    100.123456 + exponential_offset(f64::from(distance), 0.1),
                    3,
                )
                .clamp(0.0, 200.0)
            };
            assert_eq!(value, expected, "distance {distance}");
            assert!(response.dragged());
            assert_eq!(response.changed(), value != before);
            assert!(!draw(&ctx, &mut value, vec![]).changed());
            assert_eq!(value, expected);
        }
        assert_eq!(value, 100.123456);
        draw(&ctx, &mut value, vec![pointer(origin, false)]);
    }

    #[test]
    fn dragging_does_not_depend_on_frame_count_and_applies_the_release_position() {
        let mut results = Vec::new();
        for steps in [1, 10] {
            let ctx = egui::Context::default();
            let mut value = 100.0;
            let origin = draw(&ctx, &mut value, vec![]).rect.center();
            draw(
                &ctx,
                &mut value,
                vec![Event::PointerMoved(origin), pointer(origin, true)],
            );
            for step in 1..=steps {
                draw(
                    &ctx,
                    &mut value,
                    vec![Event::PointerMoved(
                        origin + vec2(50.0 * step as f32 / steps as f32, 0.0),
                    )],
                );
            }
            let position = origin + vec2(100.0, 0.0);
            draw(&ctx, &mut value, vec![pointer(position, false)]);
            results.push(value);
        }
        assert_eq!(results[0], results[1]);
        assert_eq!(
            results[0],
            egui::emath::round_to_decimals(100.0 + exponential_offset(100.0, 0.1), 3)
        );
    }

    #[test]
    fn shift_precision_is_position_based_and_returns_to_the_starting_value() {
        let mut normal = DragState::new(Pos2::ZERO, 10.0, false);
        let mut precise = DragState::new(Pos2::ZERO, 10.0, true);
        let position = pos2(100.0, 0.0);
        let change = normal.update(position, 0.1) - 10.0;
        assert!((precise.update(position, 0.1) - 10.0 - change / 10.0).abs() < 1e-12);
        assert_eq!(normal.update(Pos2::ZERO, 0.1), 10.0);
        assert_eq!(precise.update(Pos2::ZERO, 0.1), 10.0);
        assert!(normal.update(pos2(1e6, 0.0), 0.1).is_finite());
    }

    #[test]
    fn clicking_to_type_preserves_normal_entry_and_range_limits() {
        let ctx = egui::Context::default();
        let mut value = 100.0;
        for (text, expected) in [("123.456", 123.456), ("500", 200.0)] {
            let position = draw(&ctx, &mut value, vec![]).rect.center();
            draw(
                &ctx,
                &mut value,
                vec![Event::PointerMoved(position), pointer(position, true)],
            );
            draw(&ctx, &mut value, vec![pointer(position, false)]);
            draw(&ctx, &mut value, vec![Event::Text(text.into())]);
            draw(
                &ctx,
                &mut value,
                vec![Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                }],
            );
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn selecting_text_does_not_change_the_numeric_value() {
        let ctx = egui::Context::default();
        let mut value = 100.0;
        let position = draw(&ctx, &mut value, vec![]).rect.center();
        draw(
            &ctx,
            &mut value,
            vec![Event::PointerMoved(position), pointer(position, true)],
        );
        draw(&ctx, &mut value, vec![pointer(position, false)]);
        draw(&ctx, &mut value, vec![]);
        draw(&ctx, &mut value, vec![pointer(position, true)]);
        draw(
            &ctx,
            &mut value,
            vec![Event::PointerMoved(position + vec2(80.0, 0.0))],
        );
        assert_eq!(value, 100.0);
        draw(&ctx, &mut value, vec![pointer(position, false)]);
        assert_eq!(value, 100.0);
    }
}
