/// Layer blend modes (W3C compositing spec semantics).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Darken,
    Multiply,
    ColorBurn,
    Lighten,
    Screen,
    ColorDodge,
    Add,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub const ALL: [BlendMode; 17] = [
        BlendMode::Normal,
        BlendMode::Darken,
        BlendMode::Multiply,
        BlendMode::ColorBurn,
        BlendMode::Lighten,
        BlendMode::Screen,
        BlendMode::ColorDodge,
        BlendMode::Add,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Darken => "Darken",
            BlendMode::Multiply => "Multiply",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::Lighten => "Lighten",
            BlendMode::Screen => "Screen",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::Add => "Add",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }

    /// OpenRaster `composite-op` attribute value.
    pub fn ora_name(self) -> &'static str {
        match self {
            BlendMode::Normal => "svg:src-over",
            BlendMode::Darken => "svg:darken",
            BlendMode::Multiply => "svg:multiply",
            BlendMode::ColorBurn => "svg:color-burn",
            BlendMode::Lighten => "svg:lighten",
            BlendMode::Screen => "svg:screen",
            BlendMode::ColorDodge => "svg:color-dodge",
            BlendMode::Add => "svg:plus",
            BlendMode::Overlay => "svg:overlay",
            BlendMode::SoftLight => "svg:soft-light",
            BlendMode::HardLight => "svg:hard-light",
            BlendMode::Difference => "svg:difference",
            BlendMode::Exclusion => "svg:exclusion",
            BlendMode::Hue => "svg:hue",
            BlendMode::Saturation => "svg:saturation",
            BlendMode::Color => "svg:color",
            BlendMode::Luminosity => "svg:luminosity",
        }
    }

    pub fn from_ora_name(s: &str) -> BlendMode {
        BlendMode::ALL.into_iter().find(|m| m.ora_name() == s).unwrap_or_default()
    }

    /// Blend source colour `s` onto backdrop `b` (both straight RGB in 0..1).
    #[inline]
    pub fn blend(self, b: [f32; 3], s: [f32; 3]) -> [f32; 3] {
        let per = |f: fn(f32, f32) -> f32| [f(b[0], s[0]), f(b[1], s[1]), f(b[2], s[2])];
        match self {
            BlendMode::Normal => s,
            BlendMode::Darken => per(f32::min),
            BlendMode::Multiply => per(|b, s| b * s),
            BlendMode::ColorBurn => per(|b, s| {
                if b >= 1.0 {
                    1.0
                } else if s <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - b) / s).min(1.0)
                }
            }),
            BlendMode::Lighten => per(f32::max),
            BlendMode::Screen => per(|b, s| b + s - b * s),
            BlendMode::ColorDodge => per(|b, s| {
                if b <= 0.0 {
                    0.0
                } else if s >= 1.0 {
                    1.0
                } else {
                    (b / (1.0 - s)).min(1.0)
                }
            }),
            BlendMode::Add => per(|b, s| (b + s).min(1.0)),
            BlendMode::Overlay => per(|b, s| hard_light(s, b)),
            BlendMode::SoftLight => per(|b, s| {
                if s <= 0.5 {
                    b - (1.0 - 2.0 * s) * b * (1.0 - b)
                } else {
                    let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
                    b + (2.0 * s - 1.0) * (d - b)
                }
            }),
            BlendMode::HardLight => per(hard_light),
            BlendMode::Difference => per(|b, s| (b - s).abs()),
            BlendMode::Exclusion => per(|b, s| b + s - 2.0 * b * s),
            BlendMode::Hue => set_lum(set_sat(s, sat(b)), lum(b)),
            BlendMode::Saturation => set_lum(set_sat(b, sat(s)), lum(b)),
            BlendMode::Color => set_lum(s, lum(b)),
            BlendMode::Luminosity => set_lum(b, lum(s)),
        }
    }
}

#[inline]
fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 { b * 2.0 * s } else { let s2 = 2.0 * s - 1.0; b + s2 - b * s2 }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(mut c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        for v in &mut c {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut c {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mx = c[0].max(c[1]).max(c[2]);
    let mn = c[0].min(c[1]).min(c[2]);
    if mx <= mn {
        return [0.0; 3];
    }
    let k = s / (mx - mn);
    [(c[0] - mn) * k, (c[1] - mn) * k, (c[2] - mn) * k]
}
