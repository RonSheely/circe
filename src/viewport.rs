//! the viewport handles visual transforms from the schematic to canvas and vice-versa
//! CanvasSpace <-> ViewportSpace <-> SchematicSpace
//! CanvasSpace is the UI canvas coordinate
//! ViewportSpace is the schematic coordinate in f32
//! SchematicSpace is the schematic coordinate in i16

use crate::transforms::{
    CSBox, CSPoint, CSVec, CVTransform, Point, SSPoint, VCTransform, VSBox, VSPoint, VSVec,
};
use iced::widget::canvas::path::Builder;
use iced::widget::canvas::{stroke, Event, Frame, LineCap, LineDash, Path, Stroke, Text};
use iced::Color;

#[derive(Clone, Debug)]
pub enum ViewportState {
    Panning(CSPoint),
    NewView(VSPoint, VSPoint),
    None,
}

impl Default for ViewportState {
    fn default() -> Self {
        ViewportState::None
    }
}

pub struct Viewport {
    pub state: ViewportState,
    transform: VCTransform,
    scale: f32,

    curpos: (CSPoint, VSPoint, SSPoint),

    max_scale: f32,
    min_scale: f32,
    pub snap_scale: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            state: Default::default(),
            transform: VCTransform::default()
                .pre_scale(10., 10.)
                .then_scale(1., -1.),
            scale: 10.0, // scale from canvas to viewport, sqrt of transform determinant. Save value to save computing power

            curpos: (CSPoint::origin(), VSPoint::origin(), SSPoint::origin()),

            /// most zoomed in - every 1.0 unit is 100.0 pixels
            max_scale: 100.0,
            /// most zoomed out - every 1.0 unit is 1.0 pixels
            min_scale: 1.0,

            /// schematic, designer should always snap to nearest integer.  
            /// snap scale just scales viewport grid such that snapping appears to operate on some other granularity
            snap_scale: 1.0,
        }
    }
}

impl Viewport {
    /// mutate viewport based on event
    pub fn events_handler(
        &mut self,
        event: iced::widget::canvas::Event,
        curpos_csp: CSPoint,
        bounds: iced::Rectangle,
    ) -> (Option<crate::Msg>, bool, bool) {
        self.curpos_update(curpos_csp);

        let mut msg = None;
        let mut clear_passive = false;
        let mut processed = true;
        let mut state = self.state.clone();
        match (&mut state, event) {
            // zooming
            (_, Event::Mouse(iced::mouse::Event::WheelScrolled { delta })) => {
                match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. }
                    | iced::mouse::ScrollDelta::Pixels { y, .. } => {
                        let scale = 1.0 + y.clamp(-5.0, 5.0) / 5.;
                        self.zoom(scale);
                    }
                }
                msg = Some(crate::Msg::NewZoom(self.vc_scale()));
                clear_passive = true;
            }
            // panning
            (
                ViewportState::None,
                Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Middle)),
            ) => {
                state = ViewportState::Panning(curpos_csp);
            }
            (
                ViewportState::Panning(csp_prev),
                Event::Mouse(iced::mouse::Event::CursorMoved { .. }),
            ) => {
                self.pan(self.cv_transform().transform_vector(curpos_csp - *csp_prev));
                *csp_prev = curpos_csp;
                clear_passive = true;
            }
            (
                ViewportState::Panning(_),
                Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Middle)),
            ) => {
                state = ViewportState::None;
            }
            // newview
            (
                ViewportState::None,
                Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Right)),
            ) => {
                let vsp = self.cv_transform().transform_point(curpos_csp);
                state = ViewportState::NewView(vsp, vsp);
            }
            (
                ViewportState::NewView(vsp0, vsp1),
                Event::Mouse(iced::mouse::Event::CursorMoved { .. }),
            ) => {
                let vsp_now = self.cv_transform().transform_point(curpos_csp);
                if (vsp_now - *vsp0).length() > 10. {
                    *vsp1 = vsp_now;
                } else {
                    *vsp1 = *vsp0;
                }
            }
            (
                ViewportState::NewView(_vsp0, _vsp1),
                Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key_code,
                    modifiers,
                }),
            ) => {
                if let (iced::keyboard::KeyCode::Escape, 0) = (key_code, modifiers.bits()) {
                    state = ViewportState::None;
                }
            }
            (
                ViewportState::NewView(vsp0, vsp1),
                Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Right)),
            ) => {
                if vsp1 != vsp0 {
                    self.display_bounds(
                        CSBox::from_points([
                            CSPoint::origin(),
                            CSPoint::new(bounds.width, bounds.height),
                        ]),
                        VSBox::from_points([vsp0, vsp1]),
                    );
                }
                msg = Some(crate::Msg::NewZoom(self.vc_scale()));
                state = ViewportState::None;
                clear_passive = true;
            }
            _ => {
                processed = false;
            }
        }
        self.state = state;
        (msg, clear_passive, processed)
    }

    /// returns the cursor position in canvas space
    pub fn curpos_csp(&self) -> CSPoint {
        self.curpos.0
    }

    /// returns the cursor position in viewport space
    pub fn curpos_vsp(&self) -> VSPoint {
        self.curpos.1
    }

    /// returns the cursor position in schematic space
    pub fn curpos_ssp(&self) -> SSPoint {
        self.curpos.2
    }

    /// returns transform and scale such that VSBox (viewport/schematic bounds) fit inside CSBox (canvas bounds)
    fn bounds_transform(&self, csb: CSBox, vsb: VSBox) -> (VCTransform, f32) {
        let mut vct = VCTransform::identity();

        let s = (csb.height() / vsb.height())
            .min(csb.width() / vsb.width())
            .clamp(self.min_scale, self.max_scale); // scale from vsb to fit inside csb
        vct = vct.then_scale(s, -s);

        let v = csb.center() - vct.transform_point(vsb.center()); // vector from vsb to csb
        vct = vct.then_translate(v);

        (vct, s)
    }

    /// change transform such that VSBox (viewport/schematic bounds) fit inside CSBox (canvas bounds)
    pub fn display_bounds(&mut self, csb: CSBox, vsb: VSBox) {
        (self.transform, self.scale) = self.bounds_transform(csb, vsb);
        // recalculate cursor in viewport, or it will be wrong until cursor is moved
        self.curpos_update(self.curpos.0);
    }

    /// pan by vector v
    pub fn pan(&mut self, v: VSVec) {
        self.transform = self.transform.pre_translate(v);
    }

    /// return the canvas to viewport space transform
    pub fn cv_transform(&self) -> CVTransform {
        self.transform.inverse().unwrap()
    }

    /// return the viewport to canvas space transform
    pub fn vc_transform(&self) -> VCTransform {
        self.transform
    }

    /// returns the scale factor in the viewwport to canvas transform
    /// this value is stored to avoid calling sqrt() each time
    pub fn vc_scale(&self) -> f32 {
        self.scale
    }

    /// returns the scale factor in the viewwport to canvas transform
    /// this value is stored to avoid calling sqrt() each time
    pub fn cv_scale(&self) -> f32 {
        1. / self.scale
    }

    /// update the cursor position
    pub fn curpos_update(&mut self, csp1: CSPoint) {
        let vsp1 = self.cv_transform().transform_point(csp1);
        let ssp1: SSPoint = vsp1.round().cast().cast_unit();
        self.curpos = (csp1, vsp1, ssp1);
    }

    /// change the viewport zoom by scale
    pub fn zoom(&mut self, scale: f32) {
        let (csp, vsp, _) = self.curpos;
        let scaled_transform = self.transform.then_scale(scale, scale);

        let mut new_transform; // transform with applied scale and translated to maintain p_viewport position
        let scaled_determinant = scaled_transform.determinant().abs();
        if scaled_determinant < self.min_scale * self.min_scale {
            // minimum scale
            let clamped_scale = self.min_scale / self.vc_scale();
            new_transform = self.transform.then_scale(clamped_scale, clamped_scale);
        } else if scaled_determinant <= self.max_scale * self.max_scale {
            // adjust scale
            new_transform = scaled_transform;
        } else {
            // maximum scale
            let clamped_scale = self.max_scale / self.vc_scale();
            new_transform = self.transform.then_scale(clamped_scale, clamped_scale);
        }
        let csp1 = new_transform.transform_point(vsp); // translate based on cursor location
        let translation = csp - csp1;
        new_transform = new_transform.then_translate(translation);

        self.transform = new_transform;
        self.scale = self.transform.determinant().abs().sqrt();
    }

    /// draw the cursor onto canvas
    pub fn draw_cursor(&self, frame: &mut Frame) {
        let cursor_stroke = || -> Stroke {
            Stroke {
                width: 1.0,
                style: stroke::Style::Solid(Color::from_rgb(1.0, 0.9, 0.0)),
                line_cap: LineCap::Round,
                ..Stroke::default()
            }
        };
        let curdim = 5.0;
        let csp = self
            .vc_transform()
            .transform_point(self.curpos.2.cast().cast_unit());
        let csp_topleft = csp - CSVec::from([curdim / 2.; 2]);
        let s = iced::Size::from([curdim, curdim]);
        let c = Path::rectangle(iced::Point::from([csp_topleft.x, csp_topleft.y]), s);
        frame.stroke(&c, cursor_stroke());
    }

    /// draw the schematic grid onto canvas
    pub fn draw_grid(&self, frame: &mut Frame, bb_canvas: CSBox) {
        let a = Text {
            content: String::from("origin"),
            position: Point::from(self.vc_transform().transform_point(VSPoint::origin())).into(),
            color: Color::from_rgba(1.0, 1.0, 1.0, 1.0),
            size: self.vc_scale(),
            ..Default::default()
        };
        frame.fill_text(a);

        fn draw_grid_w_spacing(
            spacing: f32,
            bb_canvas: CSBox,
            vct: VCTransform,
            cvt: CVTransform,
            frame: &mut Frame,
            stroke: Stroke,
        ) {
            let bb_viewport = cvt.outer_transformed_box(&bb_canvas);
            let v = ((bb_viewport.min / spacing).round() * spacing) - bb_viewport.min;
            let bb_viewport = bb_viewport.translate(v);

            let v = bb_viewport.max - bb_viewport.min;
            for col in 0..=(v.x.ceil() / spacing) as u32 {
                let csp0 = bb_viewport.min + VSVec::from([col as f32 * spacing, 0.0]);
                let csp1 = bb_viewport.min + VSVec::from([col as f32 * spacing, v.y.ceil()]);
                let c = Path::line(
                    Point::from(vct.transform_point(csp0)).into(),
                    Point::from(vct.transform_point(csp1)).into(),
                );
                frame.stroke(&c, stroke.clone());
            }
        }
        let coarse_grid_threshold: f32 = 2.0 * self.snap_scale;
        let fine_grid_threshold: f32 = 6.0 * self.snap_scale;

        if self.vc_scale() > coarse_grid_threshold {
            // draw coarse grid
            let spacing = 16.0 / self.snap_scale;

            let grid_stroke = Stroke {
                width: (0.5 * self.vc_scale()).clamp(0.5, 3.0),
                style: stroke::Style::Solid(Color::from_rgba(1.0, 1.0, 1.0, 0.5)),
                line_cap: LineCap::Round,
                line_dash: LineDash {
                    segments: &[0.0, spacing * self.vc_scale()],
                    offset: 0,
                },
                ..Stroke::default()
            };

            draw_grid_w_spacing(
                spacing,
                bb_canvas,
                self.vc_transform(),
                self.cv_transform(),
                frame,
                grid_stroke,
            );

            if self.vc_scale() > fine_grid_threshold {
                // draw fine grid if sufficiently zoomed in
                let spacing = 2.0 / self.snap_scale;

                let grid_stroke = Stroke {
                    width: 1.0,
                    style: stroke::Style::Solid(Color::from_rgba(1.0, 1.0, 1.0, 0.5)),
                    line_cap: LineCap::Round,
                    line_dash: LineDash {
                        segments: &[0.0, spacing * self.vc_scale()],
                        offset: 0,
                    },
                    ..Stroke::default()
                };

                draw_grid_w_spacing(
                    spacing,
                    bb_canvas,
                    self.vc_transform(),
                    self.cv_transform(),
                    frame,
                    grid_stroke,
                );
            }
        }
        let ref_stroke = Stroke {
            width: (0.1 * self.vc_scale()).clamp(0.1, 3.0),
            style: stroke::Style::Solid(Color::from_rgba(1.0, 1.0, 1.0, 0.5)),
            line_cap: LineCap::Round,
            ..Stroke::default()
        };

        let mut path_builder = Builder::new();
        path_builder.move_to(
            Point::from(self.vc_transform().transform_point(VSPoint::new(0.0, 1.0))).into(),
        );
        path_builder.line_to(
            Point::from(self.vc_transform().transform_point(VSPoint::new(0.0, -1.0))).into(),
        );
        path_builder.move_to(
            Point::from(self.vc_transform().transform_point(VSPoint::new(1.0, 0.0))).into(),
        );
        path_builder.line_to(
            Point::from(self.vc_transform().transform_point(VSPoint::new(-1.0, 0.0))).into(),
        );
        let p = self.vc_transform().transform_point(VSPoint::origin());
        let r = self.vc_scale() * 0.5;
        path_builder.circle(Point::from(p).into(), r);
        frame.stroke(&path_builder.build(), ref_stroke);
    }
}
