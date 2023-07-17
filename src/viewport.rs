//! the viewport implements common canvas functions - e.g. panning, zooming
//! CanvasSpace <-> ViewportSpace <-> SchematicSpace
//! CanvasSpace is the UI canvas coordinate
//! ViewportSpace is the schematic coordinate in f32
//! SchematicSpace is the schematic coordinate in i16
//! separated from schematic controls - wouldn't want panning or zooming to cancel placing a device, etc.

use crate::transforms::{
    CSBox, CSPoint, CSVec, CVTransform, Point, SSPoint, VCTransform, VSBox, VSPoint, VSVec,
};
use crate::IcedStruct;
use iced::widget::canvas::path::Builder;
use iced::widget::canvas::{
    self, event, stroke, Cache, Cursor, Event, Frame, Geometry, LineCap, LineDash, Path, Stroke,
    Text,
};
use iced::{mouse, Color, Length, Rectangle, Size, Theme};

/// trait for element which can be drawn on canvas
pub trait Drawable {
    fn draw_persistent(&self, vct: VCTransform, vcscale: f32, frame: &mut Frame);
    fn draw_selected(&self, vct: VCTransform, vcscale: f32, frame: &mut Frame);
    fn draw_preview(&self, vct: VCTransform, vcscale: f32, frame: &mut Frame);
}

#[derive(Clone, Debug, Default)]
pub enum State {
    #[default]
    None,
    Panning(CSPoint),
    NewView(VSPoint, VSPoint),
}

#[derive(Clone, Copy, Debug)]
pub enum Msg {
    None,
    NewView(VCTransform, f32, CSPoint),
    CursorMoved(CSPoint),
}

/// message type that is the union of content and viewport messages - allows content and viewport to process events simultaneously
#[derive(Clone, Copy, Debug)]
pub struct CompositeMsg<M>
where
    M: ContentMsg,
{
    /// content msg
    pub content_msg: M,
    /// viewport message processed from canvas event
    pub viewport_msg: Msg,
}

pub trait Content<Msg>: Default {
    /// returns the mouse interaction to display on canvas based on content state
    fn mouse_interaction(&self) -> mouse::Interaction;
    /// mutate self based on ContentMsg. Returns whether to clear passive cache
    fn update(&mut self, msg: Msg) -> bool;
    /// draw geometry onto active frame
    fn draw_active(&self, vct: VCTransform, scale: f32, frame: &mut Frame);
    /// draw geometry onto passive frame
    fn draw_passive(&self, vct: VCTransform, scale: f32, frame: &mut Frame);
    /// returns the bounding box of all elements in content
    fn bounds(&self) -> VSBox;
    /// called when the user presses esc. Clear selection, reset state, etc. Returns whether or not to clear passive cache
    fn rst(&mut self) -> bool {
        false
    }
    /// called when the user cycles single selection (c key). Returns whether or not to clear passive cache
    fn cycle(&mut self, curpos_ssp: SSPoint) -> bool {
        false
    }
    /// wip - area select - only if left click on empty (ssp, vsp?). Returns whether or not to clear passive cache
    fn area_select(&mut self) -> bool {
        false
    }
}

pub trait ContentMsg {
    fn canvas_event_msg(event: Event, curpos_vsp: VSPoint) -> Self;
}

pub struct Viewport<C, M>
where
    C: Content<M>,
    M: ContentMsg,
{
    /// Contents displayed through this viewport
    pub content: C,
    /// phantom data to mark ContentMsg type
    content_msg: std::marker::PhantomData<M>,
    /// iced canvas graphical cache, cleared every frame
    pub active_cache: Cache,
    /// iced canvas graphical cache, cleared following some schematic actions
    pub passive_cache: Cache,
    /// iced canvas graphical cache, almost never cleared
    pub background_cache: Cache,

    /// viewport to canvas transform
    vct: VCTransform,
    /// the zoom scale, i.e. vct: VCTransform .determinant().abs().sqrt()
    zoom_scale: f32,

    /// the cursor positions in the different spaces
    curpos: (CSPoint, VSPoint, SSPoint),

    /// zoom in limit
    max_zoom: f32,
    /// zoom out limit
    min_zoom: f32,
    /// ssp always rounds to i16. This scale allows snapping to fixed f32 intervals if not 1.0
    /// effectively the transform from schematic space to viewport space
    scale: f32,
}

impl<C, M> canvas::Program<CompositeMsg<M>> for Viewport<C, M>
where
    C: Content<M>,
    M: ContentMsg,
{
    type State = State;

    fn update(
        &self,
        state: &mut State,
        event: Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> (event::Status, Option<CompositeMsg<M>>) {
        let opt_curpos: Option<CSPoint> =
            cursor.position_in(&bounds).map(|p| Point::from(p).into());
        let bounds_csb = CSBox::from_points([
            CSPoint::new(bounds.x, bounds.y),
            CSPoint::new(bounds.width, bounds.height),
        ]);

        let mut msgs = None;
        self.active_cache.clear();

        if let Some(curpos) = opt_curpos {
            msgs = Some(self.events_handler(state, event, bounds_csb, curpos));
        }

        (event::Status::Captured, msgs)
    }

    fn draw(
        &self,
        state: &State,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let active = self.active_cache.draw(bounds.size(), |frame| {
            self.content
                .draw_active(self.vc_transform(), self.vc_scale(), frame);

            if let State::NewView(vsp0, vsp1) = state {
                let csp0 = self.vc_transform().transform_point(*vsp0);
                let csp1 = self.vc_transform().transform_point(*vsp1);
                let selsize = Size {
                    width: csp1.x - csp0.x,
                    height: csp1.y - csp0.y,
                };
                let f = canvas::Fill {
                    style: canvas::Style::Solid(if selsize.height > 0. {
                        Color::from_rgba(1., 0., 0., 0.1)
                    } else {
                        Color::from_rgba(0., 0., 1., 0.1)
                    }),
                    ..canvas::Fill::default()
                };
                frame.fill_rectangle(Point::from(csp0).into(), selsize, f);
            }
        });

        let passive = self.passive_cache.draw(bounds.size(), |frame| {
            self.draw_grid(
                frame,
                CSBox::new(
                    CSPoint::origin(),
                    CSPoint::from([bounds.width, bounds.height]),
                ),
            );
            self.draw_origin_marker(frame);
            self.content
                .draw_passive(self.vc_transform(), self.vc_scale(), frame);
        });

        let background = self.background_cache.draw(bounds.size(), |frame| {
            let f = canvas::Fill {
                style: canvas::Style::Solid(Color::from_rgb(0.2, 0.2, 0.2)),
                ..canvas::Fill::default()
            };
            frame.fill_rectangle(iced::Point::ORIGIN, bounds.size(), f);
        });

        vec![background, passive, active]
    }

    fn mouse_interaction(
        &self,
        viewport_st: &State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(&bounds) {
            match &viewport_st {
                State::Panning(_) => mouse::Interaction::Grabbing,
                State::None => self.content.mouse_interaction(),
                _ => mouse::Interaction::default(),
            }
        } else {
            mouse::Interaction::default()
        }
    }
}

impl<C, M> IcedStruct<CompositeMsg<M>> for Viewport<C, M>
where
    C: Content<M>,
    M: ContentMsg,
{
    fn update(&mut self, msgs: CompositeMsg<M>) {
        match msgs.viewport_msg {
            Msg::NewView(vct, zoom_scale, curpos_csp) => {
                self.vct = vct;
                self.zoom_scale = zoom_scale;
                // update cursor position, otherwise it is displayed according to old vct until cursor is moved again
                self.curpos_update(curpos_csp);
                self.passive_cache.clear();
            }
            Msg::CursorMoved(curpos_csp) => {
                self.curpos_update(curpos_csp);
            }
            Msg::None => {}
        }
        if self.content.update(msgs.content_msg) {
            self.passive_cache.clear();
        }
    }

    fn view(&self) -> iced::Element<CompositeMsg<M>> {
        iced::widget::canvas(self)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

impl<C, M> Viewport<C, M>
where
    C: Content<M>,
    M: ContentMsg,
{
    pub fn new(scale: f32, min_zoom: f32, max_zoom: f32, vct: VCTransform) -> Self {
        Viewport {
            scale,
            min_zoom,
            max_zoom,
            vct,
            content: C::default(),
            active_cache: Default::default(),
            passive_cache: Default::default(),
            background_cache: Default::default(),
            zoom_scale: vct.determinant().abs().sqrt(),
            curpos: Default::default(),
            content_msg: std::marker::PhantomData,
        }
    }

    /// generate message based on canvas event
    pub fn events_handler(
        &self,
        state: &mut State,
        event: iced::widget::canvas::Event,
        bounds_csb: CSBox,
        curpos_csp: CSPoint,
    ) -> CompositeMsg<M> {
        let mut viewport_msg = Msg::None;
        let mut stcp = state.clone();
        match (&mut stcp, event) {
            // cursor move
            (State::None, Event::Mouse(iced::mouse::Event::CursorMoved { .. })) => {
                viewport_msg = Msg::CursorMoved(curpos_csp);
            }
            // zooming
            (_, Event::Mouse(iced::mouse::Event::WheelScrolled { delta })) => match delta {
                iced::mouse::ScrollDelta::Lines { y, .. }
                | iced::mouse::ScrollDelta::Pixels { y, .. } => {
                    let zoom_scale = 1.0 + y.clamp(-5.0, 5.0) / 5.;
                    viewport_msg = self.zoom(zoom_scale, curpos_csp);
                }
            },
            // panning
            (
                State::None,
                Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Middle)),
            ) => {
                stcp = State::Panning(curpos_csp);
            }
            (State::Panning(csp_prev), Event::Mouse(iced::mouse::Event::CursorMoved { .. })) => {
                viewport_msg = self.pan(curpos_csp, *csp_prev);
                *csp_prev = curpos_csp;
            }
            (
                State::Panning(_),
                Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Middle)),
            ) => {
                stcp = State::None;
            }
            // newview
            (
                State::None,
                Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Right)),
            ) => {
                let vsp = self.cv_transform().transform_point(curpos_csp);
                stcp = State::NewView(vsp, vsp);
            }
            (State::NewView(vsp0, vsp1), Event::Mouse(iced::mouse::Event::CursorMoved { .. })) => {
                let vsp_now = self.cv_transform().transform_point(curpos_csp);
                if (vsp_now - *vsp0).length() > 10. {
                    *vsp1 = vsp_now;
                } else {
                    *vsp1 = *vsp0;
                }
                viewport_msg = Msg::CursorMoved(curpos_csp);
            }
            (
                State::NewView(_vsp0, _vsp1),
                Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key_code,
                    modifiers,
                }),
            ) => {
                if let (iced::keyboard::KeyCode::Escape, 0) = (key_code, modifiers.bits()) {
                    stcp = State::None;
                }
            }
            (
                State::NewView(vsp0, vsp1),
                Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Right)),
            ) => {
                if vsp1 != vsp0 {
                    viewport_msg = self.display_bounds(
                        bounds_csb,
                        VSBox::from_points([vsp0, vsp1]),
                        curpos_csp,
                    );
                }
                stcp = State::None;
            }
            // fit view to content
            (
                State::None,
                Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key_code: iced::keyboard::KeyCode::F,
                    modifiers: _,
                }),
            ) => {
                let vsb = self.content.bounds().inflate(5.0, 5.0);
                let csp = self.curpos_csp();
                viewport_msg = self.display_bounds(bounds_csb, vsb, csp);
            }
            // // esc key / reset
            // (
            //     _,
            //     Event::Keyboard(iced::keyboard::Event::KeyPressed {
            //         key_code: iced::keyboard::KeyCode::Escape,
            //         modifiers: _,
            //     }),
            // ) => viewport_msg = Msg::ContentRst,
            // (
            //     // to be migrated to viewport - no state
            //     _,
            //     Event::Keyboard(iced::keyboard::Event::KeyPressed {
            //         key_code: iced::keyboard::KeyCode::C,
            //         modifiers: _,
            //     }),
            // ) => viewport_msg = Msg::ContentSingleTentativeCycle(self.curpos_ssp()),
            _ => {}
        }
        *state = stcp;

        let content_msg = M::canvas_event_msg(event, self.curpos_vsp());
        CompositeMsg {
            content_msg,
            viewport_msg,
        }
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

    /// returns the cursor position in schematic space
    pub fn curpos_vsp_scaled(&self) -> VSPoint {
        self.curpos.1 * self.scale
    }

    /// returns transform and scale such that VSBox (viewport/schematic bounds) fit inside CSBox (canvas bounds)
    fn bounds_transform(&self, csb: CSBox, vsb: VSBox) -> (VCTransform, f32) {
        let mut vct = VCTransform::identity();

        let s = (csb.height() / vsb.height())
            .min(csb.width() / vsb.width())
            .clamp(self.min_zoom, self.max_zoom); // scale from vsb to fit inside csb
        vct = vct.then_scale(s, -s);
        // vector from vsb center to csb center
        let v = csb.center() - vct.transform_point(vsb.center());
        vct = vct.then_translate(v);

        (vct, s)
    }

    /// change transform such that VSBox (viewport/schematic bounds) fit inside CSBox (canvas bounds)
    pub fn display_bounds(&self, csb: CSBox, vsb: VSBox, csp: CSPoint) -> Msg {
        let (vct, zoom_scale) = self.bounds_transform(csb, vsb);
        Msg::NewView(vct, zoom_scale, csp)
    }

    /// pan by vector v
    pub fn pan(&self, csp_now: CSPoint, csp_prev: CSPoint) -> Msg {
        let v = self.cv_transform().transform_vector(csp_now - csp_prev);
        let vct = self.vct.pre_translate(v);
        Msg::NewView(vct, self.zoom_scale, csp_now)
    }

    /// return the canvas to viewport space transform
    pub fn cv_transform(&self) -> CVTransform {
        self.vct.inverse().unwrap()
    }

    /// return the viewport to canvas space transform
    pub fn vc_transform(&self) -> VCTransform {
        self.vct
    }

    /// returns the scale factor in the viewwport to canvas transform
    /// this value is stored to avoid calling sqrt() each time
    pub fn vc_scale(&self) -> f32 {
        self.zoom_scale
    }

    /// returns the scale factor in the viewwport to canvas transform
    /// this value is stored to avoid calling sqrt() each time
    pub fn cv_scale(&self) -> f32 {
        1. / self.zoom_scale
    }

    /// update the cursor position
    pub fn curpos_update(&mut self, csp1: CSPoint) {
        let vsp1 = self.cv_transform().transform_point(csp1);
        let ssp1: SSPoint = vsp1.round().cast().cast_unit();
        self.curpos = (csp1, vsp1, ssp1);
    }

    /// update the cursor position
    pub fn curpos(&mut self, csp1: CSPoint) -> (VSPoint, SSPoint) {
        let vsp1 = self.cv_transform().transform_point(csp1);
        let ssp1: SSPoint = vsp1.round().cast().cast_unit();
        (vsp1, ssp1)
    }

    /// change the viewport zoom by scale
    pub fn zoom(&self, zoom_scale: f32, curpos_csp: CSPoint) -> Msg {
        let (csp, vsp, _) = self.curpos;
        let scaled_transform = self.vct.then_scale(zoom_scale, zoom_scale);

        let mut new_transform; // transform with applied scale and translated to maintain p_viewport position
        let scaled_determinant = scaled_transform.determinant().abs();
        if scaled_determinant < self.min_zoom * self.min_zoom {
            // minimum scale
            let clamped_scale = self.min_zoom / self.vc_scale();
            new_transform = self.vct.then_scale(clamped_scale, clamped_scale);
        } else if scaled_determinant <= self.max_zoom * self.max_zoom {
            // adjust scale
            new_transform = scaled_transform;
        } else {
            // maximum scale
            let clamped_scale = self.max_zoom / self.vc_scale();
            new_transform = self.vct.then_scale(clamped_scale, clamped_scale);
        }
        let csp1 = new_transform.transform_point(vsp); // translate based on cursor location
        let translation = csp - csp1;
        new_transform = new_transform.then_translate(translation);

        Msg::NewView(
            new_transform,
            new_transform.determinant().abs().sqrt(),
            curpos_csp,
        )
    }

    pub fn draw_origin_marker(&self, frame: &mut Frame) {
        // draw the origin marker
        let a = Text {
            content: String::from("origin"),
            position: Point::from(self.vc_transform().transform_point(VSPoint::origin())).into(),
            color: Color::from_rgba(1.0, 1.0, 1.0, 1.0),
            size: self.vc_scale() * self.scale,
            ..Default::default()
        };
        frame.fill_text(a);
        let ref_stroke = Stroke {
            width: (0.1 * self.vc_scale() * self.scale).clamp(0.1, 3.0),
            style: stroke::Style::Solid(Color::from_rgba(1.0, 1.0, 1.0, 0.5)),
            line_cap: LineCap::Round,
            ..Stroke::default()
        };
        let mut path_builder = Builder::new();
        path_builder.move_to(
            Point::from(
                self.vc_transform()
                    .transform_point(VSPoint::new(0.0, 1.0) * self.scale),
            )
            .into(),
        );
        path_builder.line_to(
            Point::from(
                self.vc_transform()
                    .transform_point(VSPoint::new(0.0, -1.0) * self.scale),
            )
            .into(),
        );
        path_builder.move_to(
            Point::from(
                self.vc_transform()
                    .transform_point(VSPoint::new(1.0, 0.0) * self.scale),
            )
            .into(),
        );
        path_builder.line_to(
            Point::from(
                self.vc_transform()
                    .transform_point(VSPoint::new(-1.0, 0.0) * self.scale),
            )
            .into(),
        );
        let p = self.vc_transform().transform_point(VSPoint::origin());
        let r = self.vc_scale() * self.scale * 0.5;
        path_builder.circle(Point::from(p).into(), r);
        frame.stroke(&path_builder.build(), ref_stroke);
    }

    /// draw the schematic grid onto canvas
    pub fn draw_grid(&self, frame: &mut Frame, bb_canvas: CSBox) {
        fn draw_grid_w_spacing(
            spacing: f32,
            bb_canvas: CSBox,
            vct: VCTransform,
            cvt: CVTransform,
            frame: &mut Frame,
            stroke: Stroke,
        ) {
            let bb_viewport = cvt.outer_transformed_box(&bb_canvas);
            let v = ((bb_viewport.min / spacing).ceil() * spacing) - bb_viewport.min;
            let bb_viewport = bb_viewport.translate(v);

            let v = bb_viewport.max - bb_viewport.min;
            for col in 0..=(v.x / spacing).ceil() as u32 {
                let csp0 = bb_viewport.min + VSVec::from([col as f32 * spacing, 0.0]);
                let csp1 = bb_viewport.min + VSVec::from([col as f32 * spacing, v.y.ceil()]);
                let c = Path::line(
                    Point::from(vct.transform_point(csp0)).into(),
                    Point::from(vct.transform_point(csp1)).into(),
                );
                frame.stroke(&c, stroke.clone());
            }
        }
        let coarse_grid_threshold: f32 = 2.0 / self.scale;
        let fine_grid_threshold: f32 = 6.0 / self.scale;
        if self.vc_scale() > coarse_grid_threshold {
            // draw coarse grid
            let spacing = 16.0 * self.scale;

            let grid_stroke = Stroke {
                width: (0.5 * self.vc_scale() * self.scale).clamp(0.5, 3.0),
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
                let spacing = 2.0 * self.scale;

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
    }
}
