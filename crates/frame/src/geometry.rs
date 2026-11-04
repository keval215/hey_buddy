/// Opaque display identifier (platform display id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DisplayId(pub u32);

/// Integer rectangle. Used for both pixels and points; the unit is given by context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// Exclusive right edge (saturating at i64 range; computed in i64 to avoid overflow).
    fn right(&self) -> i64 {
        self.x as i64 + self.w as i64
    }

    fn bottom(&self) -> i64 {
        self.y as i64 + self.h as i64
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Intersection, or `None` when the rects do not overlap.
    pub fn intersect(&self, other: &Rect) -> Option<Rect> {
        let x0 = (self.x as i64).max(other.x as i64);
        let y0 = (self.y as i64).max(other.y as i64);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Rect {
            x: x0 as i32,
            y: y0 as i32,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        })
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.intersect(other).is_some()
    }

    /// Clamp into `bounds`. Returns `None` if nothing remains.
    pub fn clamp_to(&self, bounds: &Rect) -> Option<Rect> {
        self.intersect(bounds)
    }

    /// Grow by `pad` on every side (negative `pad` shrinks, floor at empty).
    pub fn padded(&self, pad: i32) -> Rect {
        let x = self.x as i64 - pad as i64;
        let y = self.y as i64 - pad as i64;
        let w = (self.w as i64 + 2 * pad as i64).max(0);
        let h = (self.h as i64 + 2 * pad as i64).max(0);
        Rect {
            x: x.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            y: y.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            w: w.min(u32::MAX as i64) as u32,
            h: h.min(u32::MAX as i64) as u32,
        }
    }

    /// Scale by a factor (points -> pixels), rounding outward so the result always covers
    /// the original area.
    pub fn scaled(&self, factor: f32) -> Rect {
        let x0 = (self.x as f64 * factor as f64).floor();
        let y0 = (self.y as f64 * factor as f64).floor();
        let x1 = (self.right() as f64 * factor as f64).ceil();
        let y1 = (self.bottom() as f64 * factor as f64).ceil();
        Rect {
            x: x0 as i32,
            y: y0 as i32,
            w: (x1 - x0).max(0.0) as u32,
            h: (y1 - y0).max(0.0) as u32,
        }
    }
}

/// A display. `origin` and `size` are in global points; `scale` is pixels per point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayInfo {
    pub id: DisplayId,
    pub origin: (i32, i32),
    pub size: (u32, u32),
    pub scale: f32,
}

impl DisplayInfo {
    /// The display bounds in global points.
    pub fn bounds(&self) -> Rect {
        Rect::new(self.origin.0, self.origin.1, self.size.0, self.size.1)
    }

    /// Expected pixel size of a full-display capture.
    pub fn pixel_size(&self) -> (u32, u32) {
        (
            (self.size.0 as f32 * self.scale).round() as u32,
            (self.size.1 as f32 * self.scale).round() as u32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_and_clamp() {
        let a = Rect::new(0, 0, 10, 10);
        assert_eq!(
            a.intersect(&Rect::new(5, 5, 10, 10)),
            Some(Rect::new(5, 5, 5, 5))
        );
        assert_eq!(a.intersect(&Rect::new(10, 0, 5, 5)), None);
        assert_eq!(
            Rect::new(-5, -5, 10, 10).clamp_to(&a),
            Some(Rect::new(0, 0, 5, 5))
        );
        assert!(Rect::new(0, 0, 0, 3).is_empty());
    }

    #[test]
    fn padding() {
        assert_eq!(Rect::new(10, 10, 4, 4).padded(4), Rect::new(6, 6, 12, 12));
        assert_eq!(Rect::new(10, 10, 4, 4).padded(-3).w, 0);
        let big = Rect::new(i32::MIN, i32::MAX, u32::MAX, u32::MAX).padded(i32::MAX);
        assert_eq!(big.w, u32::MAX);
    }

    #[test]
    fn scale_rounds_outward() {
        assert_eq!(Rect::new(1, 1, 3, 3).scaled(1.5), Rect::new(1, 1, 5, 5));
        assert_eq!(Rect::new(1, 1, 3, 3).scaled(2.0), Rect::new(2, 2, 6, 6));
    }

    #[test]
    fn display_helpers() {
        let d = DisplayInfo {
            id: DisplayId(1),
            origin: (0, 0),
            size: (100, 50),
            scale: 2.0,
        };
        assert_eq!(d.pixel_size(), (200, 100));
        assert_eq!(d.bounds(), Rect::new(0, 0, 100, 50));
    }
}
