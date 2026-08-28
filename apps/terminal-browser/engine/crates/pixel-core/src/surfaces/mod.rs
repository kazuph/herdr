

use std::cell::RefCell;
use std::collections::HashMap;

mod convert;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn sized(w: u32, h: u32) -> Self {
        Self { x: 0, y: 0, w, h }
    }

    pub fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn union(self, other: Rect) -> Rect {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect {
            x,
            y,
            w: (self.x + self.w).max(other.x + other.w) - x,
            h: (self.y + self.h).max(other.y + other.h) - y,
        }
    }

    pub fn clamped(self, width: u32, height: u32) -> Rect {
        let x = self.x.min(width);
        let y = self.y.min(height);
        Rect {
            x,
            y,
            w: self.w.min(width - x),
            h: self.h.min(height - y),
        }
    }
}

pub struct Surface {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

thread_local! {
    static SURFACES: RefCell<HashMap<u32, Surface>> = RefCell::new(HashMap::new());
}


pub fn write(
    id: u32,
    width: u32,
    height: u32,
    damage: Option<Rect>,
    bgra: &[u8],
    stride: usize,
) -> Rect {
    SURFACES.with_borrow_mut(|surfaces| {
        let surface = surfaces.entry(id).or_insert(Surface {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        });
        let resized = surface.width != width || surface.height != height;
        if resized {
            surface.width = width;
            surface.height = height;
            surface
                .pixels
                .resize(width as usize * height as usize * 4, 0);
        }
        let (region, compare) = match damage {
            Some(damage) if !resized => (damage.clamped(width, height), false),
            _ => (Rect::sized(width, height), !resized),
        };
        if region.is_empty() {
            return region;
        }
        let changed = convert::region(&mut surface.pixels, width, bgra, stride, region, compare);
        if !changed && !resized {
            crate::profiler::count("surface.unchanged", 1);
            return Rect::default();
        }
        region
    })
}

pub fn remove(id: u32) {
    SURFACES.with_borrow_mut(|surfaces| {
        surfaces.remove(&id);
    });
}

pub fn with<R>(id: u32, read: impl FnOnce(&Surface) -> R) -> Option<R> {
    SURFACES.with_borrow(|surfaces| surfaces.get(&id).map(read))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bgra(pixels: &[[u8; 4]]) -> Vec<u8> {
        pixels.iter().flatten().copied().collect()
    }

    #[test]
    fn union_of_disjoint_rects_covers_both() {
        let a = Rect { x: 2, y: 3, w: 1, h: 1 };
        let b = Rect { x: 8, y: 1, w: 2, h: 4 };
        assert_eq!(a.union(b), Rect { x: 2, y: 1, w: 8, h: 4 });
        assert_eq!(a.union(Rect::default()), a);
        assert_eq!(Rect::default().union(b), b);
    }

    #[test]
    fn a_rect_running_past_the_surface_is_clipped_to_it() {
        let rect = Rect { x: 6, y: 0, w: 10, h: 10 };
        assert_eq!(rect.clamped(8, 4), Rect { x: 6, y: 0, w: 2, h: 4 });
    }

    #[test]
    fn the_first_frame_writes_the_whole_surface() {
        let source = bgra(&[[1, 2, 3, 4], [5, 6, 7, 8]]);
        let damage = Rect { x: 0, y: 0, w: 1, h: 1 };
        assert_eq!(
            write(1, 2, 1, Some(damage), &source, 8),
            Rect::sized(2, 1)
        );
        with(1, |s| assert_eq!(s.pixels, [3, 2, 1, 4, 7, 6, 5, 8])).unwrap();
        remove(1);
    }

    #[test]
    fn later_frames_only_touch_the_damaged_pixels() {
        write(2, 2, 1, None, &bgra(&[[1, 2, 3, 4], [5, 6, 7, 8]]), 8);
        let second = bgra(&[[9, 9, 9, 9], [10, 20, 30, 40]]);
        let damage = Rect { x: 1, y: 0, w: 1, h: 1 };
        assert_eq!(write(2, 2, 1, Some(damage), &second, 8), damage);
        with(2, |s| assert_eq!(s.pixels, [3, 2, 1, 4, 30, 20, 10, 40])).unwrap();
        remove(2);
    }

    #[test]
    fn a_resize_ignores_damage_because_there_is_nothing_to_keep() {
        write(3, 1, 1, None, &bgra(&[[1, 2, 3, 4]]), 4);
        let grown = bgra(&[[1, 2, 3, 4], [5, 6, 7, 8]]);
        let damage = Rect { x: 0, y: 0, w: 1, h: 1 };
        assert_eq!(
            write(3, 2, 1, Some(damage), &grown, 8),
            Rect::sized(2, 1)
        );
        with(3, |s| assert_eq!(s.pixels, [3, 2, 1, 4, 7, 6, 5, 8])).unwrap();
        remove(3);
    }

    #[test]
    fn an_identical_frame_without_damage_reports_nothing_changed() {
        let source = bgra(&[[1, 2, 3, 255], [5, 6, 7, 255]]);
        write(4, 2, 1, None, &source, 8);
        assert_eq!(write(4, 2, 1, None, &source, 8), Rect::default());
        remove(4);
    }

    #[test]
    fn a_present_with_damage_is_trusted_and_skips_the_compare() {
        let source = bgra(&[[1, 2, 3, 255], [5, 6, 7, 255]]);
        write(5, 2, 1, None, &source, 8);
        let damage = Rect { x: 0, y: 0, w: 2, h: 1 };
        assert_eq!(write(5, 2, 1, Some(damage), &source, 8), damage);
        remove(5);
    }

}
