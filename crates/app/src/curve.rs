use iced::{
    Color, Point, Rectangle, Renderer, Theme, alignment, mouse,
    widget::canvas::{
        self, Frame, Geometry, LineCap, LineDash, LineJoin, Path, Stroke, Text,
        gradient,
    },
};
use iced_t3::{color, font};

const TOP: f32 = 26.0;
const BOTTOM: f32 = 8.0;
const DASH: LineDash<'static> = LineDash {
    segments: &[3.0, 5.0],
    offset: 0,
};
/// A band this close to 0 dB counts as flat and gets a quiet marker.
const FLAT_DB: f32 = 0.05;

/// A smooth line through the ten band gains, not a measured frequency response.
/// Each point is centred in a tenth of the width, directly above its fader.
pub struct Curve(pub [f32; 10], pub bool);

/// Slope at each band, in dB per band, by Fritsch-Carlson monotone interpolation.
/// Unlike a plain spline, the line never rises above or dips below the gains
/// either side of it, so the graph can't show gain that isn't set.
fn slopes(gains: &[f32; 10]) -> [f32; 10] {
    let mut slopes = [0.0; 10];
    slopes[0] = gains[1] - gains[0];
    slopes[9] = gains[9] - gains[8];
    for i in 1..9 {
        let (before, after) =
            (gains[i] - gains[i - 1], gains[i + 1] - gains[i]);
        if before * after > 0.0 {
            slopes[i] = 2.0 * before * after / (before + after);
        }
    }
    slopes
}

/// `tint` at `alpha` over the panel behind the graph, as an opaque color.
fn over_panel(tint: Color, alpha: f32) -> Color {
    let blend = |panel: f32, tint: f32| panel + (tint - panel) * alpha;
    Color::from_rgb(
        blend(color::RAISED.r, tint.r),
        blend(color::RAISED.g, tint.g),
        blend(color::RAISED.b, tint.b),
    )
}

fn text(content: impl Into<String>, position: Point, tint: Color) -> Text {
    Text {
        content: content.into(),
        position,
        color: tint,
        size: 10.0.into(),
        font: font::REGULAR,
        ..Default::default()
    }
}

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
        let y =
            |db: f32| TOP + (12.0 - db) / 24.0 * (bounds.height - TOP - BOTTOM);
        let x = |band: f32| (band + 0.5) / 10.0 * bounds.width;
        let tint = if self.1 { color::PRIMARY } else { color::MUTED };
        let (left, right) = (x(0.0), x(9.0));

        let point = |band: usize| Point::new(x(band as f32), y(self.0[band]));
        let slopes = slopes(&self.0);
        let step = x(1.0) - x(0.0);
        // Cubic segments in pixels; a slope of one band's rise spans a third of it.
        let trace = |builder: &mut canvas::path::Builder| {
            for band in 0..9 {
                let (from, to) = (point(band), point(band + 1));
                let pixels =
                    |db: f32| db / 24.0 * (bounds.height - TOP - BOTTOM);
                builder.bezier_curve_to(
                    Point::new(
                        from.x + step / 3.0,
                        from.y - pixels(slopes[band]) / 3.0,
                    ),
                    Point::new(
                        to.x - step / 3.0,
                        to.y + pixels(slopes[band + 1]) / 3.0,
                    ),
                    to,
                );
            }
        };
        // Strongest at the extremes and gone at 0 dB, so boosts and cuts both read.
        // Opaque on purpose: with MSAA on, an RX 9060 XT draws translucent gradient
        // stops with a hard step at one alpha value. The grid goes on top instead.
        let fade = gradient::Linear::new(
            Point::new(0.0, y(12.0)),
            Point::new(0.0, y(-12.0)),
        )
        .add_stop(0.0, over_panel(tint, 0.34))
        .add_stop(0.5, over_panel(tint, 0.02))
        .add_stop(1.0, over_panel(tint, 0.34));
        frame.fill(
            &Path::new(|p| {
                p.move_to(Point::new(left, y(0.0)));
                p.line_to(point(0));
                trace(p);
                p.line_to(Point::new(right, y(0.0)));
                p.close();
            }),
            fade,
        );

        for (label, band) in [("Bass", 1.0), ("Midrange", 4.5), ("Treble", 8.0)]
        {
            frame.fill_text(Text {
                size: 11.0.into(),
                font: font::MEDIUM,
                align_x: alignment::Horizontal::Center.into(),
                ..text(label, Point::new(x(band), 0.0), color::LABEL)
            });
        }
        for band in [2.5, 6.5] {
            frame.stroke(
                &Path::line(
                    Point::new(x(band), y(12.0)),
                    Point::new(x(band), y(-12.0)),
                ),
                Stroke {
                    line_dash: DASH,
                    ..Stroke::default().with_color(color::HAIRLINE)
                },
            );
        }
        let label_x = left - 7.0;
        frame.fill_text(Text {
            align_x: alignment::Horizontal::Right.into(),
            ..text("dB", Point::new(label_x, 1.0), color::LABEL)
        });
        for (db, label) in [
            (12.0, "+12"),
            (6.0, "+6"),
            (0.0, "0"),
            (-6.0, "−6"),
            (-12.0, "−12"),
        ] {
            let line =
                Path::line(Point::new(left, y(db)), Point::new(right, y(db)));
            let stroke = Stroke::default().with_color(if db == 0.0 {
                color::MUTED.scale_alpha(0.35)
            } else {
                color::HAIRLINE
            });
            frame.stroke(
                &line,
                if db == 6.0 || db == -6.0 {
                    Stroke {
                        line_dash: DASH,
                        ..stroke
                    }
                } else {
                    stroke
                },
            );
            frame.fill_text(Text {
                align_x: alignment::Horizontal::Right.into(),
                align_y: alignment::Vertical::Center,
                ..text(label, Point::new(label_x, y(db)), color::LABEL)
            });
        }

        let curve = Path::new(|p| {
            p.move_to(point(0));
            trace(p);
        });
        let line = Stroke {
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        frame.stroke(
            &curve,
            line.with_width(6.0).with_color(tint.scale_alpha(0.16)),
        );
        frame.stroke(&curve, line.with_width(2.0).with_color(tint));

        for (band, gain) in self.0.iter().enumerate() {
            let center = point(band);
            if gain.abs() < FLAT_DB {
                frame.fill(&Path::circle(center, 2.0), tint.scale_alpha(0.6));
            } else {
                frame.fill(&Path::circle(center, 5.5), color::RAISED);
                frame.fill(&Path::circle(center, 3.5), tint);
            }
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A point on the segment after `band`, `t` of the way along, in dB.
    fn sample(gains: &[f32; 10], band: usize, t: f32) -> f32 {
        let slopes = slopes(gains);
        let (a, b) = (gains[band], gains[band + 1]);
        let (c, d) = (a + slopes[band] / 3.0, b - slopes[band + 1] / 3.0);
        let u = 1.0 - t;
        u * u * u * a
            + 3.0 * u * u * t * c
            + 3.0 * u * t * t * d
            + t * t * t * b
    }

    #[test]
    fn the_curve_never_shows_gain_beyond_the_bands_either_side() {
        let curves: [[f32; 10]; 3] = [
            [6.76, 9.0, 7.62, 3.2, 0.97, 0.29, 0.0, 0.0, 0.0, 0.0],
            [
                12.0, -12.0, 12.0, -12.0, 12.0, -12.0, 12.0, -12.0, 12.0, -12.0,
            ],
            [0.0, 0.0, 12.0, 0.0, 0.0, -12.0, -12.0, 3.0, 3.0, 3.0],
        ];
        for gains in curves {
            for band in 0..9 {
                let low = gains[band].min(gains[band + 1]) - 1e-3;
                let high = gains[band].max(gains[band + 1]) + 1e-3;
                for step in 0..=20 {
                    let db = sample(&gains, band, step as f32 / 20.0);
                    assert!((low..=high).contains(&db), "{db} at band {band}");
                }
            }
        }
    }
    #[test]
    fn a_flat_curve_stays_flat() {
        assert_eq!(slopes(&[3.0; 10]), [0.0; 10]);
    }
}
