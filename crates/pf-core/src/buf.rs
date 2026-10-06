use crate::geom::IRect;

/// A simple interleaved 8-bit raster with `C` channels per pixel.
#[derive(Clone, PartialEq)]
pub struct Buf<const C: usize> {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

/// RGBA, straight (non-premultiplied) alpha, sRGB encoded.
pub type Pixmap = Buf<4>;
/// Single channel coverage: 0 = hidden / unselected, 255 = fully visible / selected.
pub type Mask = Buf<1>;

impl<const C: usize> Buf<C> {
    pub fn new(w: u32, h: u32) -> Self {
        Self { w, h, data: vec![0; w as usize * h as usize * C] }
    }

    pub fn filled(w: u32, h: u32, px: [u8; C]) -> Self {
        let mut data = Vec::with_capacity(w as usize * h as usize * C);
        for _ in 0..w as usize * h as usize {
            data.extend_from_slice(&px);
        }
        Self { w, h, data }
    }

    pub fn from_raw(w: u32, h: u32, data: Vec<u8>) -> Self {
        assert_eq!(data.len(), w as usize * h as usize * C);
        Self { w, h, data }
    }

    pub fn rect(&self) -> IRect {
        IRect::xywh(0, 0, self.w, self.h)
    }

    #[inline]
    pub fn idx(&self, x: i32, y: i32) -> usize {
        (y as usize * self.w as usize + x as usize) * C
    }

    /// Unchecked-by-contract read; caller guarantees the coordinate is in bounds.
    #[inline]
    pub fn px(&self, x: i32, y: i32) -> [u8; C] {
        let i = self.idx(x, y);
        self.data[i..i + C].try_into().unwrap()
    }

    #[inline]
    pub fn get(&self, x: i32, y: i32) -> Option<[u8; C]> {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            None
        } else {
            Some(self.px(x, y))
        }
    }

    #[inline]
    pub fn set(&mut self, x: i32, y: i32, px: [u8; C]) {
        let i = self.idx(x, y);
        self.data[i..i + C].copy_from_slice(&px);
    }

    /// Copy out the bytes of `r`, which must lie inside the buffer.
    pub fn crop_bytes(&self, r: IRect) -> Vec<u8> {
        debug_assert!(self.rect().contains_rect(r));
        let rw = r.width() as usize * C;
        let mut out = Vec::with_capacity(rw * r.height() as usize);
        for y in r.y0..r.y1 {
            let i = self.idx(r.x0, y);
            out.extend_from_slice(&self.data[i..i + rw]);
        }
        out
    }

    /// Inverse of [`Self::crop_bytes`].
    pub fn write_bytes(&mut self, r: IRect, bytes: &[u8]) {
        debug_assert!(self.rect().contains_rect(r));
        let rw = r.width() as usize * C;
        if rw == 0 {
            return;
        }
        for (row, y) in bytes.chunks_exact(rw).zip(r.y0..r.y1) {
            let i = self.idx(r.x0, y);
            self.data[i..i + rw].copy_from_slice(row);
        }
    }

    /// A new buffer covering `r` (expressed in this buffer's coordinates, and
    /// allowed to extend past it); areas outside the old buffer get `fill`.
    pub fn reframed(&self, r: IRect, fill: [u8; C]) -> Self {
        let mut out = Self::filled(r.width() as u32, r.height() as u32, fill);
        let ov = r.intersect(self.rect());
        if !ov.is_empty() {
            let bytes = self.crop_bytes(ov);
            out.write_bytes(ov.translate(-r.x0, -r.y0), &bytes);
        }
        out
    }

    pub fn flip_h(&mut self) {
        let w = self.w as usize;
        for row in self.data.chunks_exact_mut(w * C) {
            for x in 0..w / 2 {
                for c in 0..C {
                    row.swap(x * C + c, (w - 1 - x) * C + c);
                }
            }
        }
    }

    pub fn flip_v(&mut self) {
        let rw = self.w as usize * C;
        let h = self.h as usize;
        for y in 0..h / 2 {
            let (a, b) = self.data.split_at_mut((h - 1 - y) * rw);
            a[y * rw..(y + 1) * rw].swap_with_slice(&mut b[..rw]);
        }
    }
}

/// Composite colour `c` with extra coverage `a` (0..1) over straight-alpha `s`.
#[inline]
pub fn over(s: [u8; 4], c: [u8; 4], a: f32) -> [u8; 4] {
    let ca = a * c[3] as f32 * (1.0 / 255.0);
    if ca <= 0.0 {
        return s;
    }
    let sa = s[3] as f32 * (1.0 / 255.0);
    let oa = ca + sa * (1.0 - ca);
    if oa <= 0.0 {
        return [0; 4];
    }
    let k = sa * (1.0 - ca);
    let f = |cc: u8, sc: u8| ((cc as f32 * ca + sc as f32 * k) / oa + 0.5) as u8;
    [f(c[0], s[0]), f(c[1], s[1]), f(c[2], s[2]), (oa * 255.0 + 0.5) as u8]
}

/// Rec. 601 luma of an sRGB colour, used when painting on masks.
#[inline]
pub fn luma(c: [u8; 4]) -> u8 {
    ((c[0] as u32 * 299 + c[1] as u32 * 587 + c[2] as u32 * 114 + 500) / 1000) as u8
}
