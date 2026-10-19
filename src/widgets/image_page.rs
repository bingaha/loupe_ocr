// Copyright (c) 2022-2025 Sophie Herold
// Copyright (c) 2022, 2024 Christopher Davis
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! A widget that shows the image, a loading screen, or an error page
//!
//! This widget also handles showing the context menu.

use adw::subclass::prelude::*;
use glib::{clone, Properties};
use gtk::prelude::*;
use gtk::CompositeTemplate;

use super::LpErrorDetails;
use crate::decoder::DecoderError;
use crate::deps::*;
use crate::util::gettext::*;
use crate::util::ErrorType;
use crate::widgets::LpImage;

/// Cross-box drag selection state.
///
/// Single-box drags keep working natively inside their label. Once the drag
/// leaves its starting box, the overlay gesture claims the sequence (which
/// also cancels image panning) and every spanned label gets a programmatic
/// selection. Nothing is copied automatically: Ctrl+C copies the combined
/// text, exactly like normal text selection.
#[derive(Debug, Clone, Copy, Default)]
struct OcrCrossSel {
    start_box: usize,
    start_char: i32,
    cur_box: usize,
    cur_char: i32,
}

mod imp {
    use std::cell::RefCell;
    use std::time::Duration;

    use super::*;

    #[derive(Debug, Default, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::LpImagePage)]
    #[template(file = "image_page.ui")]
    pub struct LpImagePage {
        #[template_child]
        pub(super) stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(super) spinner_page: TemplateChild<gtk::Widget>,
        #[template_child]
        pub(super) spinner_revealer: TemplateChild<gtk::Revealer>,

        #[template_child]
        pub(super) error_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub(super) error_more_info: TemplateChild<gtk::Button>,

        #[template_child]
        pub(super) image_stack_page: TemplateChild<gtk::Widget>,
        #[template_child]
        pub(super) spinner: TemplateChild<gtk::Widget>,
        #[template_child]
        pub(super) scrolled_window: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub(super) ocr_overlay: TemplateChild<gtk::Overlay>,
        #[template_child]
        #[property(type = LpImage, get = Self::image)]
        pub(super) image: TemplateChild<LpImage>,
        #[template_child]
        pub(super) popover: TemplateChild<gtk::PopoverMenu>,
        #[template_child]
        pub(super) right_click_gesture: TemplateChild<gtk::GestureClick>,
        #[template_child]
        pub(super) press_gesture: TemplateChild<gtk::GestureLongPress>,

        /// Last successful OCR lines for the shown file (original image
        /// coordinates). Owned by the window, pushed here on completion.
        pub(super) ocr_lines: RefCell<Vec<crate::ocr::OcrLine>>,
        /// Transparent selectable labels, one per OCR line.
        pub(super) ocr_labels: RefCell<Vec<gtk::Label>>,
        /// Label rects in overlay coordinates, parallel to `ocr_labels`.
        /// Empty while the layer is hidden so hit-testing misses.
        pub(super) ocr_rects: RefCell<Vec<(f64, f64, f64, f64)>>,
        /// Self-drawn highlight rects for the active selection. Native label
        /// selection cannot show on several labels at once (single PRIMARY
        /// owner), so highlights are separate widgets.
        pub(super) ocr_hl: RefCell<Vec<gtk::Box>>,
        /// Active cross-box drag selection, if any.
        pub(super) ocr_cross: RefCell<Option<OcrCrossSel>>,
        /// Last painted multi-box span (from_box, from_char, to_box, to_char)
        /// in reading order. Used by Ctrl+C. None for single-box/native.
        pub(super) ocr_span: RefCell<Option<(usize, i32, usize, i32)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LpImagePage {
        const NAME: &'static str = "LpImagePage";
        type Type = super::LpImagePage;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LpImagePage {
        fn properties() -> &'static [glib::ParamSpec] {
            Self::derived_properties()
        }

        fn property(&self, id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            self.derived_property(id, pspec)
        }

        fn constructed(&self) {
            let obj = self.obj();

            self.parent_constructed();

            self.right_click_gesture.connect_pressed(clone!(
                #[weak]
                obj,
                move |gesture, _, x, y| {
                    obj.show_popover_at(x, y);
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            ));

            self.press_gesture.connect_pressed(clone!(
                #[weak]
                obj,
                move |gesture, x, y| {
                    log::debug!("Long press triggered");
                    obj.show_popover_at(x, y);
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            ));

            self.stack.connect_visible_child_notify(clone!(
                #[weak]
                obj,
                move |stack| {
                    let imp = obj.imp();
                    let spinner_visible =
                        stack.visible_child().as_ref() == Some(imp.spinner_page.upcast_ref());
                    // Hide spinner for crossfading into other content since it does not look great
                    imp.spinner.set_visible(spinner_visible);
                }
            ));

            obj.image().connect_is_loaded_notify(clone!(
                #[weak]
                obj,
                move |_| {
                    if obj.image().is_loaded() {
                        log::debug!("Showing image");
                        obj.imp()
                            .stack
                            .set_visible_child(&*obj.imp().image_stack_page);
                    }
                }
            ));

            obj.image().connect_error_notify(clone!(
                #[weak]
                obj,
                move |_| {
                    glib::spawn_future_local(async move { obj.show_error().await });
                }
            ));

            // Keep the OCR text layer aligned with the rendered image.
            // Zoom, rotation and mirroring change the mapping, while the
            // adjustments' "changed" signal covers scrolling, zooming and
            // widget resizes (see LpImage::configure_adjustments).
            obj.image().connect_zoom_notify(clone!(
                #[weak]
                obj,
                move |_| obj.relayout_ocr_lines()
            ));
            obj.image().connect_rotation_notify(clone!(
                #[weak]
                obj,
                move |_| obj.relayout_ocr_lines()
            ));
            obj.image().connect_mirrored_notify(clone!(
                #[weak]
                obj,
                move |_| obj.relayout_ocr_lines()
            ));
            obj.image().hadj().connect_changed(clone!(
                #[weak]
                obj,
                move |_| obj.relayout_ocr_lines()
            ));
            obj.image().vadj().connect_changed(clone!(
                #[weak]
                obj,
                move |_| obj.relayout_ocr_lines()
            ));

            // Cross-box selection: a drag gesture on the overlay, CAPTURE
            // phase so it evaluates before the labels' native gestures.
            // Whoever claims first wins the sequence; claiming in begin (on
            // text hits) gives us deterministic ownership of ALL selection
            // painting, single- and multi-box alike. Empty-area presses are
            // denied so pan and native behavior there are untouched.
            let cross_drag = gtk::GestureDrag::new();
            cross_drag.set_propagation_phase(gtk::PropagationPhase::Capture);
            cross_drag.connect_drag_begin(clone!(
                #[weak]
                obj,
                move |gesture, x, y| obj.ocr_drag_begin(gesture, x, y)
            ));
            cross_drag.connect_drag_update(clone!(
                #[weak]
                obj,
                move |gesture, x, y| obj.ocr_drag_update(gesture, x, y)
            ));
            cross_drag.connect_drag_end(clone!(
                #[weak]
                obj,
                move |gesture, x, y| obj.ocr_drag_end(gesture, x, y)
            ));
            obj.imp().ocr_overlay.add_controller(cross_drag);

            self.spinner_revealer.connect_map(|revealer| {
                let revealer = revealer.clone();
                glib::timeout_add_local_once(Duration::from_millis(200), move || {
                    revealer.set_reveal_child(true);
                });
            });
        }
    }

    impl WidgetImpl for LpImagePage {}
    impl BinImpl for LpImagePage {}

    impl LpImagePage {
        pub fn image(&self) -> LpImage {
            self.image.get()
        }
    }
}

glib::wrapper! {
    pub struct LpImagePage(ObjectSubclass<imp::LpImagePage>)
        @extends gtk::Widget, adw::Bin,
        @implements gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

/// Character offset to byte offset (clamped).
fn char_to_byte(s: &str, ch: i32) -> i32 {
    let n = s.chars().count();
    let ch = ch.clamp(0, n as i32) as usize;
    s.char_indices().nth(ch).map(|(i, _)| i as i32).unwrap_or(s.len() as i32)
}

/// Byte-safe substring by character offsets (clamped, ordered).
fn slice_chars(s: &str, a: i32, b: i32) -> &str {
    let n = s.chars().count();
    let (mut a, mut b) = (a.clamp(0, n as i32) as usize, b.clamp(0, n as i32) as usize);
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    let start = s.char_indices().nth(a).map(|(i, _)| i).unwrap_or(s.len());
    let end = s.char_indices().nth(b).map(|(i, _)| i).unwrap_or(s.len());
    &s[start..end]
}

impl LpImagePage {
    pub fn from_file(file: &gio::File) -> Self {
        let obj = glib::Object::new::<Self>();

        obj.imp().image.init(file);

        glib::spawn_future_local(clone!(
            #[weak]
            obj,
            #[strong]
            file,
            async move {
                let imp = obj.imp();
                imp.image.load(&file).await;
            }
        ));

        obj
    }

    pub fn file(&self) -> gio::File {
        self.image().file().unwrap()
    }

    pub fn scrolled_window(&self) -> gtk::ScrolledWindow {
        self.imp().scrolled_window.clone()
    }

    /// Shows OCR results as selectable transparent labels over the image.
    ///
    /// Each label is added as a direct overlay child positioned with margins,
    /// so text can be drag-selected and copied (Ctrl+C) straight on the image,
    /// like WeChat/QQ screenshot OCR. Direct children stay pickable while
    /// empty areas fall through to the image (pan). Labels only cover text
    /// areas; dragging anywhere else still pans the image as usual.
    pub fn set_ocr_lines(&self, lines: Vec<crate::ocr::OcrLine>) {
        let imp = self.imp();

        for label in imp.ocr_labels.borrow().iter() {
            imp.ocr_overlay.remove_overlay(label);
        }
        imp.ocr_labels.replace(Vec::new());
        imp.ocr_lines.replace(lines);

        let mut labels = Vec::new();
        for line in imp.ocr_lines.borrow().iter() {
            let label = gtk::Label::new(Some(&line.text));
            // Deliberately NOT selectable: all highlights are self-drawn
            // (.ocr-hl) so there is exactly one selection visual (light
            // blue). Native dark-blue selection is gone for good.
            label.set_selectable(false);
            label.set_single_line_mode(true);
            label.set_xalign(0.0);
            label.set_yalign(0.0);
            label.set_halign(gtk::Align::Start);
            label.set_valign(gtk::Align::Start);
            // Full text on hover, useful when the rendered row is tiny.
            label.set_tooltip_text(Some(&line.text));
            label.add_css_class("ocr-line");
            imp.ocr_overlay.add_overlay(&label);
            labels.push(label);
        }
        imp.ocr_labels.replace(labels);

        self.relayout_ocr_lines();

        // The overlay may not have its final allocation yet when results arrive
        // (e.g. window still mapping). Re-run once settled. Temporary safety
        // net while validating; cheap (13 labels).
        let weak = glib::WeakRef::new();
        weak.set(Some(self));
        let weak2 = weak.clone();
        glib::idle_add_local_once(move || {
            if let Some(page) = weak.upgrade() {
                page.relayout_ocr_lines();
            }
        });
        glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
            if let Some(page) = weak2.upgrade() {
                page.relayout_ocr_lines();
            }
        });
    }

    /// Hides the OCR layer and drops its labels.
    pub fn clear_ocr_lines(&self) {
        self.set_ocr_lines(Vec::new());
    }

    /// Repositions OCR labels after zoom/scroll/resize/rotation changes.
    ///
    /// Labels are hidden when there is nothing to show, when the image is
    /// rotated/mirrored (boxes are in original orientation), or while the
    /// image is not loaded.
    pub fn relayout_ocr_lines(&self) {
        let imp = self.imp();
        let image = imp.image.get();
        let lines = imp.ocr_lines.borrow();
        let labels = imp.ocr_labels.borrow();

        let usable = image.is_loaded()
            && image.error().is_none()
            && image.rotation() == 0.0
            && !image.mirrored();
        let show = usable && !lines.is_empty() && lines.len() == labels.len();
        if !show {
            for label in labels.iter() {
                label.set_visible(false);
            }
            imp.ocr_rects.replace(Vec::new());
            return;
        }

        let widget_w = image.width() as f64;
        let widget_h = image.height() as f64;

        // Temporary diagnostics (removed before final replace).
        eprintln!(
            "[OCRDBG] relayout lines={} labels={} img_alloc={}x{} zoom={} loaded={}",
            lines.len(),
            labels.len(),
            widget_w,
            widget_h,
            image.zoom(),
            image.is_loaded()
        );

        for (idx, (line, label)) in lines.iter().zip(labels.iter()).enumerate() {
            // Bounding rect of the (possibly slightly skewed) quad.
            let (mut x0, mut y0) = image.img_to_widget_coord(line.points[0]);
            let (mut x1, mut y1) = (x0, y0);
            for p in line.points.iter().skip(1) {
                let (x, y) = image.img_to_widget_coord(*p);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }

            // Skip fully offscreen rows, clamp partially visible ones.
            if x1 < 0.0 || y1 < 0.0 || x0 > widget_w || y0 > widget_h {
                label.set_visible(false);
                continue;
            }
            label.set_visible(true);
            let cx0 = x0.max(0.0).round();
            let cy0 = y0.max(0.0).round();
            let w = ((x1.min(widget_w) - cx0).max(8.0).round() as i32).max(1);
            let h = ((y1.min(widget_h) - cy0).max(8.0).round() as i32).max(1);

            // Direct overlay child: margins position the label, empty areas
            // stay pick-through to the image underneath. Guards avoid
            // resize feedback loops with the layout system.
            let (mx, my) = (cx0 as i32, cy0 as i32);
            if label.margin_start() != mx {
                label.set_margin_start(mx);
            }
            if label.margin_top() != my {
                label.set_margin_top(my);
            }
            if label.width_request() != w || label.height_request() != h {
                label.set_size_request(w, h);
            }

            if idx == 0 {
                eprintln!(
                    "[OCRDBG] line0 widget=({:.1},{:.1})-({:.1},{:.1}) margin=({:.0},{:.0} {}x{}) label_alloc={}x{}",
                    x0,
                    y0,
                    x1,
                    y1,
                    cx0,
                    cy0,
                    w,
                    h,
                    label.width(),
                    label.height()
                );
            }

            // Font size follows the rendered row height so the transparent
            // text stays aligned with the pixels underneath. NOTE: absolute
            // size is in device (pixel) units; set_size would be points and
            // render ~33% too tall on 96dpi.
            // Shrink-to-fit: the OCR box is fixed, our text goes inside it.
            // It may be smaller than the real glyphs, never larger: measure
            // the natural width first and scale down if it overflows.
            let mut font_h = h as f64;
            let set_font = |label: &gtk::Label, font_h: f64| {
                let attrs = gtk::pango::AttrList::new();
                let mut desc = gtk::pango::FontDescription::new();
                desc.set_absolute_size(font_h * gtk::pango::SCALE as f64);
                attrs.insert(gtk::pango::AttrFontDesc::new(&desc));
                label.set_attributes(Some(&attrs));
            };
            set_font(label, font_h);
            let (_min_w, natural_w, _, _) = label.measure(gtk::Orientation::Horizontal, -1);
            if natural_w > w {
                font_h = (font_h * w as f64 / natural_w as f64).floor().max(4.0);
                set_font(label, font_h);
            }

            // Hit-test rects for the cross-box manager (overlay coords).
            if imp.ocr_rects.borrow().len() <= idx {
                imp.ocr_rects.borrow_mut().push((cx0, cy0, w as f64, h as f64));
            } else {
                imp.ocr_rects.borrow_mut()[idx] = (cx0, cy0, w as f64, h as f64);
            }
        }
        imp.ocr_rects.borrow_mut().truncate(labels.len());
    }

    /// Hit-test overlay coordinates against OCR label rects.
    fn ocr_hit_test(&self, x: f64, y: f64) -> Option<usize> {
        self.imp().ocr_rects.borrow().iter().position(|(rx, ry, rw, rh)| {
            x >= *rx && x < rx + rw && y >= *ry && y < ry + rh
        })
    }

    /// Character offset inside a box for overlay coordinates.
    ///
    /// Uses a throwaway Pango layout with the label's own font so the mapping
    /// matches the rendered text. Offsets are characters.
    fn ocr_char_at(&self, box_idx: usize, x: f64, y: f64) -> i32 {
        let Some((layout, rect)) = self.ocr_box_layout(box_idx) else {
            return 0;
        };
        let scale = gtk::pango::SCALE as f64;
        let (_, byte_idx, _) =
            layout.xy_to_index(((x - rect.0) * scale) as i32, ((y - rect.1) * scale) as i32);
        let imp = self.imp();
        let lines = imp.ocr_lines.borrow();
        lines
            .get(box_idx)
            .and_then(|l| {
                l.text
                    .get(..byte_idx.max(0) as usize)
                    .map(|s| s.chars().count() as i32)
            })
            .unwrap_or(0)
    }

    /// Throwaway Pango layout matching a label's font and width, plus its
    /// overlay rect. Used for both hit-testing and highlight metrics.
    fn ocr_box_layout(&self, box_idx: usize) -> Option<(gtk::pango::Layout, (f64, f64, f64, f64))> {
        let imp = self.imp();
        let lines = imp.ocr_lines.borrow();
        let labels = imp.ocr_labels.borrow();
        let rects = imp.ocr_rects.borrow();
        let (line, label, rect) = (
            lines.get(box_idx)?,
            labels.get(box_idx)?,
            rects.get(box_idx)?,
        );
        let layout = gtk::pango::Layout::new(&label.pango_context());
        layout.set_text(&line.text);
        if let Some(attrs) = label.attributes() {
            layout.set_attributes(Some(&attrs));
        }
        layout.set_width((rect.2 * gtk::pango::SCALE as f64) as i32);
        Some((layout, *rect))
    }

    /// Pixel rect of a character range inside a box (overlay coordinates).
    fn ocr_range_rect(&self, box_idx: usize, s: i32, e: i32) -> Option<(i32, i32, i32, i32)> {
        let imp = self.imp();
        let lines = imp.ocr_lines.borrow();
        let rects = imp.ocr_rects.borrow();
        let (line, rect) = (lines.get(box_idx)?, rects.get(box_idx)?);
        let n = line.text.chars().count() as i32;
        let (s, e) = (s.clamp(0, n), e.clamp(0, n));
        if s >= e {
            return None;
        }
        // Full row: exact box rect, no measuring needed.
        if s == 0 && e == n {
            return Some((
                rect.0.round() as i32,
                rect.1.round() as i32,
                rect.2.round() as i32,
                rect.3.round() as i32,
            ));
        }
        let (layout, _) = self.ocr_box_layout(box_idx)?;
        let scale = gtk::pango::SCALE as f64;
        let r0 = layout.index_to_pos(char_to_byte(&line.text, s));
        let r1 = layout.index_to_pos(char_to_byte(&line.text, e));
        let x0 = rect.0 + r0.x() as f64 / scale;
        let x1 = rect.0 + r1.x() as f64 / scale;
        Some((
            x0.round() as i32,
            rect.1.round() as i32,
            ((x1 - x0).round() as i32).max(2),
            rect.3.round() as i32,
        ))
    }

    /// Clears self-drawn highlights (labels themselves are never selected, so
    /// the PRIMARY clipboard is never disturbed).
    fn ocr_clear_selections(&self) {
        let imp = self.imp();
        for w in imp.ocr_hl.borrow().iter() {
            imp.ocr_overlay.remove_overlay(w);
        }
        imp.ocr_hl.replace(Vec::new());
    }

    /// GestureDrag::drag-begin: claim cross-box selection on text, else deny.
    ///
    /// Claiming here (capture phase, before labels' native gestures run)
    /// denies the native selection for this sequence, so every highlight is
    /// painted by [`Self::ocr_drive_cross`] — single- and multi-box alike,
    /// with identical visuals. Clicks lose native caret placement (accepted:
    /// this is a mouse-first extraction tool).
    fn ocr_drag_begin(&self, gesture: &gtk::GestureDrag, x: f64, y: f64) {
        // Fresh press: drop any stale multi-selection span.
        self.imp().ocr_span.replace(None);
        // Mouse / primary button only; touch keeps native pan behavior.
        if gesture.current_button() != gdk::BUTTON_PRIMARY {
            *self.imp().ocr_cross.borrow_mut() = None;
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        }
        if let Some(b) = self.ocr_hit_test(x, y) {
            self.ocr_clear_selections();
            let ch = self.ocr_char_at(b, x, y);
            eprintln!("[OCRDBG] begin at ({:.0},{:.0}) hit box {} char {} -> CLAIM", x, y, b, ch);
            *self.imp().ocr_cross.borrow_mut() = Some(OcrCrossSel {
                start_box: b,
                start_char: ch,
                cur_box: b,
                cur_char: ch,
            });
            gesture.set_state(gtk::EventSequenceState::Claimed);
        } else {
            *self.imp().ocr_cross.borrow_mut() = None;
            gesture.set_state(gtk::EventSequenceState::Denied);
        }
    }

    /// GestureDrag::drag-update: offsets are from the press point, overlay
    /// space (same as label margins) — no coordinate translation needed.
    fn ocr_drag_update(&self, gesture: &gtk::GestureDrag, dx: f64, dy: f64) {
        let Some((sx, sy)) = gesture.start_point() else {
            return;
        };
        let (x, y) = (sx + dx, sy + dy);
        let mut guard = self.imp().ocr_cross.borrow_mut();
        let Some(cross) = guard.as_mut() else {
            return;
        };
        if let Some(b) = self.ocr_hit_test(x, y) {
            cross.cur_box = b;
            cross.cur_char = self.ocr_char_at(b, x, y);
        }
        // Claim once the drag leaves its starting box: this cancels image
        // panning for the rest of the sequence.
        if cross.cur_box != cross.start_box {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
        eprintln!(
            "[OCRDBG] update cur=({},{}) start={} recognized={}",
            cross.cur_box,
            cross.cur_char,
            cross.start_box,
            gesture.is_recognized()
        );
        let snapshot = *cross;
        drop(guard);
        self.ocr_drive_cross(&snapshot);
    }

    /// GestureDrag::drag-end: disarm. Sub-threshold drags are treated as
    /// clicks: our programmatic paint is wiped (no caret is placed, since the
    /// native gesture never saw the press).
    fn ocr_drag_end(&self, _gesture: &gtk::GestureDrag, dx: f64, dy: f64) {
        self.imp().ocr_cross.borrow_mut().take();
        if dx.hypot(dy) < 8.0 {
            self.ocr_clear_selections();
            self.imp().ocr_span.replace(None);
        }
    }

    /// Paints the spanned range as self-drawn highlight rects.
    ///
    /// One widget per spanned row part, so any number of boxes highlights at
    /// once (native label selection can't: single PRIMARY owner).
    fn ocr_drive_cross(&self, cross: &OcrCrossSel) {
        let imp = self.imp();
        let lines = imp.ocr_lines.borrow();

        let (from_box, from_ch, to_box, to_ch) = if cross.cur_box >= cross.start_box {
            (cross.start_box, cross.start_char, cross.cur_box, cross.cur_char)
        } else {
            (cross.cur_box, cross.cur_char, cross.start_box, cross.start_char)
        };
        let char_len = |i: usize| lines.get(i).map(|l| l.text.chars().count() as i32).unwrap_or(0);

        for w in imp.ocr_hl.borrow().iter() {
            imp.ocr_overlay.remove_overlay(w);
        }
        let mut hls = Vec::new();
        for i in from_box..=to_box {
            if i >= lines.len() {
                break;
            }
            let (mut s, mut e) = if from_box == to_box {
                (from_ch, to_ch)
            } else if i == from_box {
                (from_ch.min(char_len(i)), char_len(i))
            } else if i == to_box {
                (0, to_ch.max(0))
            } else {
                (0, char_len(i))
            };
            (s, e) = if s <= e { (s, e) } else { (e, s) };
            if let Some((x, y, w, h)) = self.ocr_range_rect(i, s, e) {
                let hl = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                hl.add_css_class("ocr-hl");
                hl.set_halign(gtk::Align::Start);
                hl.set_valign(gtk::Align::Start);
                hl.set_margin_start(x);
                hl.set_margin_top(y);
                hl.set_size_request(w, h);
                imp.ocr_overlay.add_overlay(&hl);
                hls.push(hl);
            }
        }
        imp.ocr_hl.replace(hls);

        // Remember the painted range for Ctrl+C — single-box too, since the
        // native gesture was denied and only our paint exists.
        imp.ocr_span
            .replace(Some((from_box, from_ch, to_box, to_ch)));
    }

    /// Copies the painted span (reading order, rows joined).
    ///
    /// Returns whether a span was consumed. Called explicitly by Ctrl+C
    /// (window-level, so it works regardless of keyboard focus); releasing
    /// the drag never copies by itself.
    pub(crate) fn ocr_copy_span(&self) -> bool {
        let imp = self.imp();
        let span = *imp.ocr_span.borrow();
        let Some((from_box, from_ch, to_box, to_ch)) = span else {
            return false;
        };
        let lines = imp.ocr_lines.borrow();
        let rects = imp.ocr_rects.borrow();
        if lines.is_empty() || to_box >= lines.len() || from_box >= lines.len() {
            return false;
        }

        // Reading order: from from_box/from_ch through to_box/to_ch.
        let forward = to_box > from_box || (to_box == from_box && to_ch >= from_ch);
        let (lo, hi) = (from_box.min(to_box), from_box.max(to_box));
        let mut out = String::new();
        for i in lo..=hi {
            let t = &lines[i].text;
            let n = t.chars().count() as i32;
            let (mut s, mut e) = if lo == hi {
                (from_ch, to_ch)
            } else if forward && i == lo || !forward && i == hi {
                (from_ch, n)
            } else if forward && i == hi || !forward && i == lo {
                (0, to_ch)
            } else {
                (0, n)
            };
            (s, e) = (s.clamp(0, n), e.clamp(0, n));
            if i > lo {
                // Same visual row continues prose, otherwise new line.
                let same_row = match (rects.get(i - 1), rects.get(i)) {
                    (Some(pr), Some(cr)) => {
                        ((pr.1 + pr.3 / 2.0) - (cr.1 + cr.3 / 2.0)).abs()
                            < pr.3.min(cr.3) * 0.5
                    }
                    _ => false,
                };
                out.push_str(if same_row { "" } else { "\n" });
            }
            out.push_str(slice_chars(t, s, e));
        }
        if out.is_empty() {
            return false;
        }

        self.clipboard().set_text(&out);
        if let Some(root) = self.root() {
            if let Ok(win) = root.downcast::<crate::widgets::LpWindow>() {
                win.show_toast(
                    &gettext_f("Copied {} characters from image", [out.chars().count()]),
                    adw::ToastPriority::Normal,
                );
            }
        }
        true
    }

    pub fn content_provider(&self) -> Option<gdk::ContentProvider> {
        self.imp().image.content_provider()
    }

    pub fn show_popover_at(&self, x: f64, y: f64) {
        let imp = self.imp();

        let rect = gdk::Rectangle::new(x as i32, y as i32, 0, 0);

        imp.popover.set_pointing_to(Some(&rect));
        imp.popover.popup();
    }

    pub async fn show_error(&self) {
        let imp = self.imp();
        let image = self.image();

        if image.specific_error() == DecoderError::NoLoadersConfigured {
            // Translators: {} is replaced with a version number
            let message = gettext_f(
                "No image loaders available. Maybe the “glycin-loaders” package with compatibility version “{}” is not installed.",
                [ format!("{}+", glycin::COMPAT_VERSION)]);

            imp.error_page.set_description(Some(&message));
            imp.stack.set_visible_child(&*imp.error_page);
            imp.error_more_info.set_visible(false);
        } else if image.specific_error() == DecoderError::UnsupportedFormat {
            let mime_type = image.metadata().unreliable_mime_type().unwrap_or_default();
            let content_type = gio::content_type_from_mime_type(&mime_type).unwrap_or_default();
            let description = gio::content_type_get_description(&content_type).to_string();

            let message = if glycin::Loader::DEFAULT_MIME_TYPES.contains(&mime_type.as_str()) {
                // Translators: The first occurance of {} is replace with a description of the
                // format and the second with an id (mime-type) of the format.
                gettext_f(
                    "The image format “{} ({})” is known but not installed.",
                    [&description, &mime_type],
                )
            } else {
                // Translators: The first occurance of {} is replace with a description of the
                // format and the second with an id (mime-type) of the format.
                gettext_f(
                    "Unknown image format “{} ({}).”",
                    [&description, &mime_type],
                )
            };

            if let Some(err) = image.error() {
                imp.error_more_info.connect_clicked(glib::clone!(
                    #[weak(rename_to = obj)]
                    self,
                    move |_| {
                        LpErrorDetails::new(&obj.root().unwrap(), &err, ErrorType::Loader);
                    }
                ));
            }

            imp.error_page.set_description(Some(&message));
            imp.stack.set_visible_child(&*imp.error_page);
        } else if let Some(err) = image.error() {
            imp.error_more_info.connect_clicked(glib::clone!(
                #[weak(rename_to = obj)]
                self,
                move |_| {
                    LpErrorDetails::new(&obj.root().unwrap(), &err, ErrorType::Loader);
                }
            ));

            if image.specific_error() == DecoderError::OutOfMemory {
                imp.error_page.set_description(Some(&gettext(
                    "There is not enough free memory available to load this image.",
                )));
            } else {
                imp.error_page.set_description(Some(&gettext(
                    "Either the image file is corrupted or it contains unsupported elements.",
                )));
            }

            imp.error_more_info.set_visible(true);
            imp.stack.set_visible_child(&*imp.error_page);
        }
    }
}
