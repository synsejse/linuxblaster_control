use crate::style;
use iced::{
    Point, Rectangle, Renderer, Theme, mouse,
    widget::canvas::{self, Frame, Geometry, Path, Stroke, Text},
};
/// Straight segments between the ten band gains, not a measured frequency response.
pub struct Curve(pub [f32; 10], pub bool);
impl<Message> canvas::Program<Message> for Curve {
    type State = ();
    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let left = 36.0;
        let right = bounds.width - 14.0;
        let y = |db: f32| 24.0 + (12.0 - db) / 24.0 * (bounds.height - 46.0);
        let x = |i: usize| left + i as f32 / 9.0 * (right - left);
        let accent = if self.1 { style::ACCENT } else { style::MUTED };
        for (label, pos) in [("BASS", 0), ("MIDRANGE", 4), ("TREBLE", 8)] {
            frame.fill_text(Text {
                content: label.into(),
                position: Point::new(x(pos), 2.0),
                color: style::MUTED,
                size: 9.0.into(),
                font: style::SEMIBOLD,
                ..Default::default()
            });
        }
        for db in [-12.0, -6.0, 0.0, 6.0, 12.0] {
            frame.stroke(
                &Path::line(Point::new(left, y(db)), Point::new(right, y(db))),
                Stroke::default().with_color(if db == 0.0 {
                    style::MUTED.scale_alpha(0.4)
                } else {
                    style::LINE.scale_alpha(0.65)
                }),
            );
            if db == 0.0 || db.abs() == 12.0 {
                frame.fill_text(Text {
                    content: if db == 0.0 {
                        "0".into()
                    } else {
                        format!("{db:+.0}")
                    },
                    position: Point::new(2.0, y(db) - 6.0),
                    color: style::MUTED,
                    size: 10.0.into(),
                    font: style::REGULAR,
                    ..Default::default()
                });
            }
        }
        for i in 0..10 {
            frame.stroke(
                &Path::line(
                    Point::new(x(i), y(12.0)),
                    Point::new(x(i), y(-12.0)),
                ),
                Stroke::default().with_color(style::LINE.scale_alpha(0.4)),
            );
        }
        let fill = Path::new(|p| {
            p.move_to(Point::new(x(0), y(0.0)));
            for (i, db) in self.0.iter().enumerate() {
                p.line_to(Point::new(x(i), y(*db)));
            }
            p.line_to(Point::new(x(9), y(0.0)));
            p.close();
        });
        frame.fill(&fill, accent.scale_alpha(0.08));
        let path = Path::new(|p| {
            for (i, db) in self.0.iter().enumerate() {
                if i == 0 {
                    p.move_to(Point::new(x(i), y(*db)));
                } else {
                    p.line_to(Point::new(x(i), y(*db)));
                }
            }
        });
        frame.stroke(
            &path,
            Stroke::default().with_width(2.0).with_color(accent),
        );
        for (i, db) in self.0.iter().enumerate() {
            let dot = Path::circle(Point::new(x(i), y(*db)), 3.0);
            frame.fill(&dot, style::INSET);
            frame.stroke(
                &dot,
                Stroke::default().with_width(1.5).with_color(accent),
            );
        }
        vec![frame.into_geometry()]
    }
}
