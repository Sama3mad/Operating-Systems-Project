use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::Color;

use std::collections::VecDeque;

// ─── Bandwidth chart ──────────────────────────────────────────────────────────

pub struct BandwidthProgram<'a> {
    pub in_history: &'a VecDeque<f32>,
    pub out_history: &'a VecDeque<f32>,
}

impl<'a> canvas::Program<crate::data::Message> for BandwidthProgram<'a> {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &iced::Renderer,
        _theme: &iced::Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());

        let w = bounds.width;
        let h = bounds.height;
        let pad = 4.0f32;

        // Background
        frame.fill_rectangle(
            iced::Point::ORIGIN,
            bounds.size(),
            Color::from_rgb(0.05, 0.07, 0.10),
        );

        // Grid lines
        for i in 1..4 {
            let y = pad + (h - 2.0 * pad) * (i as f32 / 4.0);
            let grid = Path::line(
                iced::Point::new(pad, y),
                iced::Point::new(w - pad, y),
            );
            frame.stroke(
                &grid,
                Stroke::default()
                    .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.06))
                    .with_width(1.0),
            );
        }

        let max_val = self
            .in_history
            .iter()
            .chain(self.out_history.iter())
            .cloned()
            .fold(100.0f32, f32::max)
            * 1.2;

        let mut plot = |history: &VecDeque<f32>, color: Color| {
            if history.len() < 2 {
                return;
            }
            let n = history.len();
            let points: Vec<iced::Point> = history
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    let x = pad + (w - 2.0 * pad) * (i as f32 / (crate::data::HISTORY_LEN - 1) as f32);
                    let y = h - pad - (h - 2.0 * pad) * (v / max_val);
                    iced::Point::new(x, y)
                })
                .collect();

            let mut path_builder = canvas::path::Builder::new();
            path_builder.move_to(points[0]);
            for p in &points[1..] {
                path_builder.line_to(*p);
            }
            let line = path_builder.build();
            frame.stroke(&line, Stroke::default().with_color(color).with_width(2.0));

            // Filled area
            let mut fill_builder = canvas::path::Builder::new();
            fill_builder.move_to(iced::Point::new(points[0].x, h - pad));
            fill_builder.line_to(points[0]);
            for p in &points[1..] {
                fill_builder.line_to(*p);
            }
            fill_builder.line_to(iced::Point::new(points[n - 1].x, h - pad));
            fill_builder.close();
            let fill_area = fill_builder.build();
            let fill_color = Color::new(color.r, color.g, color.b, 0.15);
            frame.fill(&fill_area, fill_color);
        };

        plot(self.in_history, Color::from_rgb(0.2, 0.8, 1.0));   // cyan = in
        plot(self.out_history, Color::from_rgb(1.0, 0.75, 0.2));  // amber = out

        vec![frame.into_geometry()]
    }
}