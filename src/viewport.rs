//! Display-oriented geometry, independent of the GUI and native framebuffer rotation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Picture {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Viewport {
    pub actual_size: bool,
    pub pan: (f32, f32),
}
impl Viewport {
    pub fn picture(&mut self, view: (f32, f32), image: (u16, u16)) -> Picture {
        let (vw, vh) = (view.0.max(0.0), view.1.max(0.0));
        let (iw, ih) = (f32::from(image.0), f32::from(image.1));
        if vw == 0.0 || vh == 0.0 || iw == 0.0 || ih == 0.0 {
            return Picture::default();
        }
        let scale = if self.actual_size {
            1.0
        } else {
            (vw / iw).min(vh / ih)
        };
        let (width, height) = (iw * scale, ih * scale);
        self.pan.0 = self.pan.0.clamp(0.0, (width - vw).max(0.0));
        self.pan.1 = self.pan.1.clamp(0.0, (height - vh).max(0.0));
        Picture {
            left: if width <= vw {
                (vw - width) / 2.0
            } else {
                -self.pan.0
            },
            top: if height <= vh {
                (vh - height) / 2.0
            } else {
                -self.pan.1
            },
            width,
            height,
        }
    }
}
impl Picture {
    /// Initial presses outside the picture are rejected; drag/up positions clamp.
    /// `image` is upright size. Do not inverse-rotate the resulting coordinates.
    pub fn pointer(self, point: (f32, f32), image: (u16, u16), clamp: bool) -> Option<(u16, u16)> {
        if self.width <= 0.0
            || self.height <= 0.0
            || image.0 == 0
            || image.1 == 0
            || !point.0.is_finite()
            || !point.1.is_finite()
        {
            return None;
        }
        let x = (point.0 - self.left) / self.width * f32::from(image.0);
        let y = (point.1 - self.top) / self.height * f32::from(image.1);
        if !clamp && (x < 0.0 || y < 0.0 || x >= f32::from(image.0) || y >= f32::from(image.1)) {
            return None;
        }
        Some((
            x.clamp(0.0, f32::from(image.0 - 1)) as u16,
            y.clamp(0.0, f32::from(image.1 - 1)) as u16,
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fit_letterbox_and_clamped_upright_coordinates() {
        let p = Viewport::default().picture((400.0, 300.0), (200, 100));
        assert_eq!(
            p,
            Picture {
                left: 0.0,
                top: 50.0,
                width: 400.0,
                height: 200.0
            }
        );
        assert_eq!(p.pointer((0.0, 0.0), (200, 100), false), None);
        assert_eq!(
            p.pointer((201.9, 152.9), (200, 100), false),
            Some((100, 51))
        );
        assert_eq!(p.pointer((900.0, -10.0), (200, 100), true), Some((199, 0)));
    }
    #[test]
    fn actual_size_pan_clamps_and_centers_axes_that_fit() {
        let mut v = Viewport {
            actual_size: true,
            pan: (500.0, 500.0),
        };
        assert_eq!(
            v.picture((300.0, 200.0), (500, 100)),
            Picture {
                left: -200.0,
                top: 50.0,
                width: 500.0,
                height: 100.0
            }
        );
        assert_eq!(v.pan, (200.0, 0.0));
        assert_eq!(v.picture((600.0, 200.0), (500, 100)).left, 50.0);
        assert_eq!(v.pan, (0.0, 0.0));
    }
}
