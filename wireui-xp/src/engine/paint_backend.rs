// Paint-backend seam, CPU-only. The engine paints through PaintScene, which
// records a display list rasterized by cpu_raster (tiny-skia). The GPU
// (vello/wgpu) arm from the mainline engine is removed entirely; Windows XP
// has no usable modern GPU API and the client forces the CPU layer anyway.

use kurbo::{self, Affine, PathEl, Rect, Shape};
use peniko::{self, BlendMode, BrushRef, Color, Fill, Font};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

pub fn set_backend_cpu(_cpu: bool) {}

pub fn backend_is_cpu() -> bool {
    true
}

pub struct PaintScene {
    pub(crate) inner: Inner,
}

pub(crate) enum Inner {
    Cpu(DisplayList),
}

#[derive(Clone, Default)]
pub struct DisplayList {
    pub(crate) cmds: Vec<Cmd>,
}

#[derive(Clone)]
pub(crate) enum Cmd {
    Fill {
        rule: Fill,
        transform: Affine,
        brush: CpuBrush,
        path: tiny_skia::Path,
    },
    Stroke {
        stroke: kurbo::Stroke,
        transform: Affine,
        brush: CpuBrush,
        path: tiny_skia::Path,
    },
    // alpha 1.0 = pure clip; < 1.0 = an opacity group. clip None = degenerate
    // clip shape (everything inside is clipped out).
    PushLayer {
        alpha: f32,
        transform: Affine,
        clip: Option<tiny_skia::Path>,
    },
    PopLayer,
    Image {
        data: peniko::Blob<u8>,
        width: u32,
        height: u32,
        transform: Affine,
    },
    GlyphRun {
        font: Font,
        font_size: f32,
        transform: Affine,
        color: Color,
        glyphs: Vec<Glyph>,
    },
    BlurredRect {
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    },
    Append {
        list: DisplayList,
        transform: Affine,
    },
}

#[derive(Clone)]
pub(crate) enum CpuBrush {
    Solid(Color),
    LinearGradient {
        start: kurbo::Point,
        end: kurbo::Point,
        stops: Vec<(f32, Color)>,
    },
}

fn to_cpu_brush(b: BrushRef) -> CpuBrush {
    match b {
        BrushRef::Solid(c) => CpuBrush::Solid(c),
        BrushRef::Gradient(g) => match g.kind {
            peniko::GradientKind::Linear { start, end } => CpuBrush::LinearGradient {
                start,
                end,
                stops: g.stops.iter().map(|s| (s.offset, s.color)).collect(),
            },
            // Radial/sweep are unused by the engine; first stop keeps it visible.
            _ => CpuBrush::Solid(
                g.stops.first().map(|s| s.color).unwrap_or(Color::BLACK),
            ),
        },
        BrushRef::Image(_) => CpuBrush::Solid(Color::TRANSPARENT),
    }
}

// kurbo shape -> tiny-skia path, converted once at record time. 0.1 tolerance
// matches the .to_path(0.1) calls the vello path used for the same shapes.
pub(crate) fn shape_to_path(shape: &impl Shape) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    for el in shape.path_elements(0.1) {
        match el {
            PathEl::MoveTo(p) => pb.move_to(p.x as f32, p.y as f32),
            PathEl::LineTo(p) => pb.line_to(p.x as f32, p.y as f32),
            PathEl::QuadTo(p1, p2) => {
                pb.quad_to(p1.x as f32, p1.y as f32, p2.x as f32, p2.y as f32)
            }
            PathEl::CurveTo(p1, p2, p3) => pb.cubic_to(
                p1.x as f32,
                p1.y as f32,
                p2.x as f32,
                p2.y as f32,
                p3.x as f32,
                p3.y as f32,
            ),
            PathEl::ClosePath => pb.close(),
        }
    }
    pb.finish()
}

impl Default for PaintScene {
    fn default() -> Self {
        Self::new()
    }
}

impl PaintScene {
    pub fn new() -> Self {
        PaintScene {
            inner: Inner::Cpu(DisplayList::default()),
        }
    }

    pub fn as_cpu(&self) -> Option<&DisplayList> {
        match &self.inner {
            Inner::Cpu(l) => Some(l),
        }
    }

    pub fn fill<'b>(
        &mut self,
        style: Fill,
        transform: Affine,
        brush: impl Into<BrushRef<'b>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        let brush = brush.into();
        match &mut self.inner {
            Inner::Cpu(l) => {
                debug_assert!(brush_transform.is_none());
                if let Some(path) = shape_to_path(shape) {
                    l.cmds.push(Cmd::Fill {
                        rule: style,
                        transform,
                        brush: to_cpu_brush(brush),
                        path,
                    });
                }
            }
        }
    }

    pub fn stroke<'b>(
        &mut self,
        stroke: &kurbo::Stroke,
        transform: Affine,
        brush: impl Into<BrushRef<'b>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        let brush = brush.into();
        match &mut self.inner {
            Inner::Cpu(l) => {
                debug_assert!(brush_transform.is_none());
                if let Some(path) = shape_to_path(shape) {
                    l.cmds.push(Cmd::Stroke {
                        stroke: stroke.clone(),
                        transform,
                        brush: to_cpu_brush(brush),
                        path,
                    });
                }
            }
        }
    }

    pub fn push_layer(
        &mut self,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        clip: &impl Shape,
    ) {
        let _ = blend.into();
        match &mut self.inner {
            Inner::Cpu(l) => l.cmds.push(Cmd::PushLayer {
                alpha,
                transform,
                clip: shape_to_path(clip),
            }),
        }
    }

    pub fn pop_layer(&mut self) {
        match &mut self.inner {
            Inner::Cpu(l) => l.cmds.push(Cmd::PopLayer),
        }
    }

    pub fn draw_image(&mut self, image: &peniko::Image, transform: Affine) {
        match &mut self.inner {
            Inner::Cpu(l) => l.cmds.push(Cmd::Image {
                data: image.data.clone(),
                width: image.width,
                height: image.height,
                transform,
            }),
        }
    }

    pub fn draw_blurred_rounded_rect(
        &mut self,
        transform: Affine,
        rect: Rect,
        color: Color,
        radius: f64,
        std_dev: f64,
    ) {
        match &mut self.inner {
            Inner::Cpu(l) => l.cmds.push(Cmd::BlurredRect {
                transform,
                rect,
                color,
                radius,
                std_dev,
            }),
        }
    }

    pub fn append(&mut self, other: &PaintScene, transform: Option<Affine>) {
        match (&mut self.inner, &other.inner) {
            (Inner::Cpu(l), Inner::Cpu(o)) => l.cmds.push(Cmd::Append {
                list: o.clone(),
                transform: transform.unwrap_or(Affine::IDENTITY),
            }),
        }
    }

    pub fn draw_glyphs(&mut self, font: &Font) -> DrawGlyphsBuilder<'_> {
        DrawGlyphsBuilder {
            scene: self,
            font: font.clone(),
            font_size: 16.0,
            transform: Affine::IDENTITY,
            brush: Color::BLACK,
        }
    }
}

pub struct DrawGlyphsBuilder<'a> {
    scene: &'a mut PaintScene,
    font: Font,
    font_size: f32,
    transform: Affine,
    brush: Color,
}

impl<'a> DrawGlyphsBuilder<'a> {
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    pub fn transform(mut self, transform: Affine) -> Self {
        self.transform = transform;
        self
    }

    pub fn brush<'b>(mut self, brush: impl Into<BrushRef<'b>>) -> Self {
        if let BrushRef::Solid(c) = brush.into() {
            self.brush = c;
        }
        self
    }

    pub fn draw(self, style: Fill, glyphs: impl Iterator<Item = Glyph>) {
        let _ = style;
        match &mut self.scene.inner {
            Inner::Cpu(l) => {
                let glyphs: Vec<Glyph> = glyphs.collect();
                if !glyphs.is_empty() {
                    l.cmds.push(Cmd::GlyphRun {
                        font: self.font,
                        font_size: self.font_size,
                        transform: self.transform,
                        color: self.brush,
                        glyphs,
                    });
                }
            }
        }
    }
}
