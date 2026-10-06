use egui::Key;
use egui_phosphor::regular as icon;
use pf_core::fill::GradientShape;
use pf_core::paint::BrushParams;
use pf_core::selection::Combine;
use pf_core::text::TextSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    Move,
    RectSelect,
    EllipseSelect,
    Lasso,
    QuickSelect,
    MagicWand,
    Brush,
    Pencil,
    Eraser,
    Smudge,
    Clone,
    Gradient,
    Bucket,
    Text,
    Eyedropper,
    Hand,
    Zoom,
}

impl Tool {
    /// Toolbar order; `None` is a gap between groups.
    pub const STRIP: [Option<Tool>; 22] = [
        Some(Tool::Move),
        None,
        Some(Tool::RectSelect),
        Some(Tool::EllipseSelect),
        Some(Tool::Lasso),
        Some(Tool::QuickSelect),
        Some(Tool::MagicWand),
        None,
        Some(Tool::Brush),
        Some(Tool::Pencil),
        Some(Tool::Eraser),
        Some(Tool::Gradient),
        Some(Tool::Bucket),
        None,
        Some(Tool::Clone),
        Some(Tool::Smudge),
        None,
        Some(Tool::Text),
        None,
        Some(Tool::Eyedropper),
        Some(Tool::Hand),
        Some(Tool::Zoom),
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Move => "Arrange",
            Tool::RectSelect => "Rectangular Selection",
            Tool::EllipseSelect => "Elliptical Selection",
            Tool::Lasso => "Free Selection",
            Tool::QuickSelect => "Quick Selection",
            Tool::MagicWand => "Magic Wand",
            Tool::Brush => "Paint",
            Tool::Pencil => "Pencil",
            Tool::Eraser => "Erase",
            Tool::Smudge => "Smudge",
            Tool::Clone => "Clone Stamp",
            Tool::Gradient => "Gradient",
            Tool::Bucket => "Color Fill",
            Tool::Text => "Type",
            Tool::Eyedropper => "Color Picker",
            Tool::Hand => "Hand",
            Tool::Zoom => "Zoom",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Tool::Move => icon::CURSOR,
            Tool::RectSelect => icon::SELECTION,
            Tool::EllipseSelect => icon::CIRCLE_DASHED,
            Tool::Lasso => icon::LASSO,
            Tool::QuickSelect => icon::SELECTION_PLUS,
            Tool::MagicWand => icon::MAGIC_WAND,
            Tool::Brush => icon::PAINT_BRUSH,
            Tool::Pencil => icon::PENCIL_SIMPLE,
            Tool::Eraser => icon::ERASER,
            Tool::Smudge => icon::HAND_POINTING,
            Tool::Clone => icon::STAMP,
            Tool::Gradient => icon::GRADIENT,
            Tool::Bucket => icon::PAINT_BUCKET,
            Tool::Text => icon::TEXT_T,
            Tool::Eyedropper => icon::EYEDROPPER,
            Tool::Hand => icon::HAND,
            Tool::Zoom => icon::MAGNIFYING_GLASS,
        }
    }

    pub fn key(self) -> Key {
        match self {
            Tool::Move => Key::V,
            Tool::RectSelect => Key::M,
            Tool::EllipseSelect => Key::O,
            Tool::Lasso => Key::L,
            Tool::QuickSelect => Key::Q,
            Tool::MagicWand => Key::W,
            Tool::Brush => Key::B,
            Tool::Pencil => Key::N,
            Tool::Eraser => Key::E,
            Tool::Smudge => Key::R,
            Tool::Clone => Key::S,
            Tool::Gradient => Key::G,
            Tool::Bucket => Key::K,
            Tool::Text => Key::T,
            Tool::Eyedropper => Key::I,
            Tool::Hand => Key::H,
            Tool::Zoom => Key::Z,
        }
    }

    pub fn is_selection(self) -> bool {
        matches!(self, Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso | Tool::QuickSelect | Tool::MagicWand)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GradientFill {
    ForegroundToBackground,
    ForegroundToTransparent,
}

/// Per-tool options shown in the inspector.
pub struct Settings {
    pub fg: [u8; 4],
    pub bg: [u8; 4],
    pub brush: BrushParams,
    pub pencil: BrushParams,
    pub eraser: BrushParams,
    pub smudge: BrushParams,
    pub smudge_strength: f32,
    pub clone: BrushParams,
    pub gradient_shape: GradientShape,
    pub gradient_fill: GradientFill,
    pub gradient_opacity: f32,
    pub sel_mode: Combine,
    /// Style for new text; `text` and `path` are ignored.
    pub text: TextSpec,
    pub tolerance: u8,
    pub contiguous: bool,
    pub sample_all_layers: bool,
    pub quick_size: f32,
    pub fill_opacity: f32,
    pub auto_select: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let b = |size, hardness| BrushParams { size, hardness, opacity: 1.0, flow: 1.0, spacing: 0.08 };
        Self {
            fg: [0, 0, 0, 255],
            bg: [255, 255, 255, 255],
            brush: BrushParams { flow: 0.6, ..b(40.0, 0.6) },
            pencil: b(3.0, 1.0),
            eraser: b(50.0, 0.8),
            smudge: b(40.0, 0.3),
            smudge_strength: 0.75,
            clone: b(60.0, 0.5),
            gradient_shape: GradientShape::Linear,
            gradient_fill: GradientFill::ForegroundToBackground,
            gradient_opacity: 1.0,
            sel_mode: Combine::Replace,
            text: TextSpec::default(),
            tolerance: 32,
            contiguous: true,
            sample_all_layers: true,
            quick_size: 30.0,
            fill_opacity: 1.0,
            auto_select: true,
        }
    }
}

impl Settings {
    /// The brush parameters of a paint tool.
    pub fn params_mut(&mut self, tool: Tool) -> Option<&mut BrushParams> {
        match tool {
            Tool::Brush => Some(&mut self.brush),
            Tool::Pencil => Some(&mut self.pencil),
            Tool::Eraser => Some(&mut self.eraser),
            Tool::Smudge => Some(&mut self.smudge),
            Tool::Clone => Some(&mut self.clone),
            _ => None,
        }
    }

    /// Diameter of the on-canvas cursor ring for `tool`, if it has one.
    pub fn cursor_size(&mut self, tool: Tool) -> Option<f32> {
        if tool == Tool::QuickSelect {
            return Some(self.quick_size);
        }
        self.params_mut(tool).map(|p| p.size)
    }
}
