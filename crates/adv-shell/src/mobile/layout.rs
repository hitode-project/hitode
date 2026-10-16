//! Where a mobile game draws and where the overlay may place controls
//! (AND-APP-16). Controls may use the letterbox beside the game, including
//! the safe-area margins, but never a cutout, a rounded corner or a system
//! gesture edge. All values are view points.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Insets {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

/// What hides or takes over parts of the screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Occlusion {
    /// Camera housings: notches, Dynamic Islands and punch holes.
    pub cutouts: Vec<Rect>,
    pub corner_radius: f64,
    /// Edges whose swipes the system takes, such as the home indicator.
    pub gestures: Insets,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub width: f64,
    pub height: f64,
    /// The presented game frame.
    pub game: Rect,
    pub occlusion: Occlusion,
}

impl Occlusion {
    /// The largest rectangle centred in the view that no cutout overlaps. The
    /// game is placed in it; each cutout clears the edge it sits on, and the
    /// opposite edge by the same amount, so the frame stays centred.
    pub fn content(&self, width: f64, height: f64) -> Rect {
        let (mut x, mut y) = (0.0f64, 0.0f64);
        for cutout in &self.cutouts {
            let depths = [
                (cutout.x + cutout.width, true),
                (width - cutout.x, true),
                (cutout.y + cutout.height, false),
                (height - cutout.y, false),
            ];
            let (depth, horizontal) = depths
                .into_iter()
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .unwrap();
            let depth = depth.clamp(0.0, if horizontal { width } else { height } / 2.0);
            if horizontal {
                x = x.max(depth);
            } else {
                y = y.max(depth);
            }
        }
        Rect {
            x,
            y,
            width: width - 2.0 * x,
            height: height - 2.0 * y,
        }
    }
}

/// The camera housing of an iPhone in its portrait coordinates, from how far
/// the safe area reaches in from that edge. Apple publishes no housing
/// geometry, so each kind takes the largest of its generation: a notch at
/// most 210 points wide, a Dynamic Island 126 points wide ending 49 points
/// down. Insets under 30 points are a status bar, not a housing.
pub fn iphone_housing(portrait_width: f64, depth: f64) -> Option<Rect> {
    let (width, height): (f64, f64) = match depth {
        d if d < 30.0 => return None,
        d if d < 55.0 => (210.0, 34.0),
        _ => (126.0, 49.0),
    };
    Some(Rect {
        x: (portrait_width - width) / 2.0,
        y: 0.0,
        width,
        height: height.min(depth),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    // spec: AND-APP-16
    #[test]
    fn cutouts_clear_their_edge_symmetrically() {
        let none = Occlusion::default();
        assert_eq!(none.content(844.0, 390.0), rect(0.0, 0.0, 844.0, 390.0));
        // A notch on the right of a landscape phone.
        let notch = Occlusion {
            cutouts: vec![rect(810.0, 90.0, 34.0, 210.0)],
            ..Occlusion::default()
        };
        assert_eq!(notch.content(844.0, 390.0), rect(34.0, 0.0, 776.0, 390.0));
        // A punch hole near the top left corner of a portrait phone.
        let hole = Occlusion {
            cutouts: vec![rect(20.0, 10.0, 24.0, 24.0)],
            ..Occlusion::default()
        };
        assert_eq!(hole.content(400.0, 900.0), rect(0.0, 34.0, 400.0, 832.0));
    }

    // spec: AND-APP-16
    #[test]
    fn iphone_housings_follow_the_safe_area() {
        assert_eq!(iphone_housing(375.0, 20.0), None);
        assert_eq!(
            iphone_housing(375.0, 44.0),
            Some(rect(82.5, 0.0, 210.0, 34.0))
        );
        assert_eq!(
            iphone_housing(393.0, 59.0),
            Some(rect(133.5, 0.0, 126.0, 49.0))
        );
    }
}
