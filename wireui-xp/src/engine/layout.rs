use super::dom::{Document, NodeKey};
use super::style::{Computed, DisplayKind, Flow, Length, Position, TextAlign, VAlign};
use std::collections::HashMap;
use taffy::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct ColorBrush(pub [f32; 4]);

pub struct TextRun {
    pub layout: parley::Layout<ColorBrush>,
}

pub struct LayoutResult {
    pub rects: HashMap<NodeKey, (f32, f32, f32, f32)>,
    pub text_layouts: HashMap<NodeKey, TextRun>,
}

/// Right-hand strip a collapsed <select> keeps clear for its chevron. Layout
/// reserves it when sizing the control; paint keeps the label out of it.
pub const SELECT_CHEVRON_WIDTH: f32 = 22.0;

struct NodeCtx {
    dom: NodeKey,
    text: Option<String>,
    font_size: f32,
    font_weight: u16,
    font_family: Vec<String>,
    color: [f32; 4],
    line_height: Option<f32>,
    letter_spacing: f32,
    nowrap: bool,
    // Width added to the measured text: the room a collapsed <select> needs for
    // its chevron, which paint keeps clear on the right.
    extra_width: f32,
}

pub struct TextSystem {
    pub font_cx: parley::FontContext,
    pub layout_cx: parley::LayoutContext<ColorBrush>,
    pub aliases: HashMap<String, String>,
}

impl TextSystem {
    pub fn new() -> TextSystem {
        Self::new_with(true)
    }

    // system_fonts=false skips the OS font-collection scan. On Windows 7/8 that
    // scan goes through fontique's DirectWrite backend, which uses IDWriteFactory2
    // (Windows 8.1+) and access-violates on Win7; we register our own embedded
    // fonts, so the scan is optional.
    pub fn new_with(system_fonts: bool) -> TextSystem {
        let collection = parley::fontique::Collection::new(parley::fontique::CollectionOptions {
            system_fonts,
            shared: false,
        });
        let font_cx = parley::FontContext {
            collection,
            source_cache: parley::fontique::SourceCache::default(),
        };
        TextSystem {
            font_cx,
            layout_cx: parley::LayoutContext::new(),
            aliases: HashMap::new(),
        }
    }

    pub fn register_font(&mut self, css_family: &str, data: Vec<u8>) {
        let registered = self.font_cx.collection.register_fonts(data);
        for (family_id, _) in registered {
            if let Some(name) = self.font_cx.collection.family_name(family_id) {
                self.aliases
                    .insert(css_family.to_lowercase(), name.to_string());
                break;
            }
        }
    }

    fn font_stack_source(&self, families: &[String]) -> String {
        let mut out: Vec<String> = Vec::new();
        for f in families {
            let resolved = self
                .aliases
                .get(&f.to_lowercase())
                .cloned()
                .unwrap_or_else(|| f.clone());
            if resolved.contains(' ') && !resolved.starts_with('\'') {
                out.push(format!("'{}'", resolved));
            } else {
                out.push(resolved);
            }
        }
        out.push("system-ui".to_string());
        // Final fallback: the embedded @font-face families (their resolved OS
        // names). On Win7/8 the system-font scan is off, so "system-ui" and any
        // unregistered family resolve to nothing; without this, such text shapes
        // blank. Tried last, so modern-Windows system fonts still win.
        for name in self.aliases.values() {
            if !out.iter().any(|f| f.trim_matches('\'') == name) {
                if name.contains(' ') {
                    out.push(format!("'{}'", name));
                } else {
                    out.push(name.clone());
                }
            }
        }
        out.join(", ")
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_layout(
        &mut self,
        text: &str,
        font_size: f32,
        font_weight: u16,
        families: &[String],
        color: [f32; 4],
        max_width: Option<f32>,
        align: parley::layout::Alignment,
        scale: f32,
        line_height: Option<f32>,
        letter_spacing: f32,
        nowrap: bool,
    ) -> parley::Layout<ColorBrush> {
        // Substitute symbol codepoints the UI fonts lack (parley fallback doesn't
        // cover them) with visually-equivalent ones the fonts DO have, so they
        // don't render as tofu: e.g. the settings/modal close glyph ✕ (U+2715)
        // shows as a box, while × (U+00D7) renders.
        let sub = if text.contains('\u{2715}') {
            text.replace('\u{2715}', "\u{00D7}")
        } else {
            text.to_string()
        };
        let text: &str = &sub;
        let stack = self.font_stack_source(families);
        let mut builder = self.layout_cx.ranged_builder(&mut self.font_cx, text, scale);
        builder.push_default(parley::style::StyleProperty::FontSize(font_size));
        builder.push_default(parley::style::StyleProperty::FontWeight(
            parley::style::FontWeight::new(font_weight as f32),
        ));
        builder.push_default(parley::style::StyleProperty::FontStack(
            parley::style::FontStack::Source(std::borrow::Cow::Borrowed(&stack)),
        ));
        builder.push_default(parley::style::StyleProperty::Brush(ColorBrush(color)));
        if let Some(lh) = line_height {
            if font_size > 0.0 {
                builder.push_default(parley::style::StyleProperty::LineHeight(lh / font_size));
            }
        } else {
            // Sciter's default line box is the font's metric line (ascent +
            // descent, ~1.36em for the embedded OpenSans faces), not 1.0em.
            builder.push_default(parley::style::StyleProperty::LineHeight(1.36));
        }
        if letter_spacing != 0.0 {
            builder.push_default(parley::style::StyleProperty::LetterSpacing(letter_spacing));
        }
        let mut layout = builder.build(text);
        layout.break_all_lines(if nowrap { None } else { max_width });
        layout.align(max_width, align);
        layout
    }
}

pub fn layout_document(
    doc: &Document,
    styles: &HashMap<NodeKey, Computed>,
    text_system: &mut TextSystem,
    viewport: (f32, f32),
    scale: f32,
) -> LayoutResult {
    let mut tree: TaffyTree<NodeCtx> = TaffyTree::new();
    let mut dom_to_taffy: HashMap<NodeKey, taffy::NodeId> = HashMap::new();
    let mut absolute_set: std::collections::HashSet<NodeKey> = std::collections::HashSet::new();

    let built = build_node(
        doc,
        styles,
        doc.root,
        None,
        &mut tree,
        &mut dom_to_taffy,
        &mut absolute_set,
    );
    let (root_taffy, root_pending) = match built {
        Some(r) => r,
        None => {
            return LayoutResult {
                rects: HashMap::new(),
                text_layouts: HashMap::new(),
            }
        }
    };
    if !root_pending.is_empty() {
        let mut root_children = tree.children(root_taffy).unwrap_or_default();
        root_children.extend(root_pending.into_iter().map(|(id, _)| id));
        tree.set_children(root_taffy, &root_children).ok();
    }
    if let Ok(root_style) = tree.style(root_taffy) {
        let mut pinned = root_style.clone();
        pinned.size = Size {
            width: Dimension::Length(viewport.0),
            height: Dimension::Length(viewport.1),
        };
        tree.set_style(root_taffy, pinned).ok();
    }

    let ts = std::cell::RefCell::new(text_system);
    tree.compute_layout_with_measure(
        root_taffy,
        Size {
            width: AvailableSpace::Definite(viewport.0),
            height: AvailableSpace::Definite(viewport.1),
        },
        |known, available, _id, ctx, _style| {
            let ctx = match ctx {
                Some(c) => c,
                None => return Size::ZERO,
            };
            let text = match &ctx.text {
                Some(t) => t.clone(),
                None => return Size::ZERO,
            };
            // MinContent must wrap at every opportunity (longest word), or a
            // flex item's automatic minimum size becomes the full unwrapped
            // line and long text overflows its container instead of wrapping
            // (the 2FA dialog description ran past the dialog edge).
            let max_width = known.width.or(match available.width {
                AvailableSpace::Definite(w) => Some(w),
                AvailableSpace::MinContent => Some(0.0),
                AvailableSpace::MaxContent => None,
            });
            let mut ts = ts.borrow_mut();
            let layout = ts.build_layout(
                &text,
                ctx.font_size,
                ctx.font_weight,
                &ctx.font_family,
                ctx.color,
                max_width,
                parley::layout::Alignment::Start,
                1.0,
                ctx.line_height,
                ctx.letter_spacing,
                ctx.nowrap,
            );
            if std::env::var("WIREUI_MEASURE_DBG").is_ok() && text.starts_with("Open your") {
                eprintln!(
                    "MEASURE known={:?} avail=({:?},{:?}) -> w={} h={}",
                    known,
                    available.width,
                    available.height,
                    layout.full_width(),
                    layout.height()
                );
            }
            Size {
                // full_width keeps trailing whitespace advance; width() trims it,
                // which collapsed an inline span ending in &nbsp; so the following
                // inline text butted right against it ("licensed underAGPL 3.0").
                width: (layout.full_width() + ctx.extra_width).ceil(),
                height: layout.height().ceil(),
            }
        },
    )
    .ok();

    let mut result = LayoutResult {
        rects: HashMap::new(),
        text_layouts: HashMap::new(),
    };
    let _ = &absolute_set;
    collect_rects(&tree, root_taffy, (0.0, 0.0), &mut result.rects);

    for (dom_key, rect) in result.rects.clone() {
        let node = match doc.arena.get(dom_key) {
            Some(n) => n,
            None => continue,
        };
        if let Some(text) = node.text_content() {
            let style = styles.get(&dom_key).cloned().unwrap_or_default();
            let transformed = style.text_transform.apply(text);
            let mut ts = ts.borrow_mut();
            let layout = ts.build_layout(
                &transformed,
                style.font_size,
                style.font_weight,
                &style.font_family,
                [style.color.r, style.color.g, style.color.b, style.color.a],
                Some(rect.2),
                parley::layout::Alignment::Start,
                1.0,
                style.line_height,
                style.letter_spacing,
                style.white_space_nowrap,
            );
            result
                .text_layouts
                .insert(dom_key, TextRun { layout });
        } else if node.tag == "textarea" {
            // A textarea shows its multi-line value (or placeholder), wrapped to
            // the box's inner width -- the ticket description/reply compose box.
            let style = styles.get(&dom_key).cloned().unwrap_or_default();
            let (text, color) = input_display_text(node, Some(&style));
            if !text.is_empty() {
                let pad_px = |l: &super::style::Length| match l {
                    super::style::Length::Px(v) => *v,
                    _ => 0.0,
                };
                let inner =
                    (rect.2 - pad_px(&style.padding[1]) - pad_px(&style.padding[3])).max(1.0);
                let mut ts = ts.borrow_mut();
                let layout = ts.build_layout(
                    &text,
                    style.font_size,
                    style.font_weight,
                    &style.font_family,
                    color,
                    Some(inner),
                    parley::layout::Alignment::Start,
                    1.0,
                    style.line_height,
                    style.letter_spacing,
                    false,
                );
                result.text_layouts.insert(dom_key, TextRun { layout });
            }
        } else if node.tag == "input" || node.tag == "select" {
            let (text, color) = if node.tag == "select" {
                // An `editable` select (the file-transfer path box) shows its
                // assigned free-text .value, not just a matching <option>.
                let t = match node.attr("value").filter(|_| node.attr("editable").is_some()) {
                    Some(v) if !v.is_empty() => v.to_string(),
                    _ => selected_option_text(doc, dom_key),
                };
                let c = styles
                    .get(&dom_key)
                    .map(|s| [s.color.r, s.color.g, s.color.b, s.color.a])
                    .unwrap_or([0.1, 0.15, 0.2, 1.0]);
                (t, c)
            } else {
                input_display_text(node, styles.get(&dom_key))
            };
            if !text.is_empty() {
                let style = styles.get(&dom_key).cloned().unwrap_or_default();
                let mut ts = ts.borrow_mut();
                let layout = ts.build_layout(
                    &text,
                    style.font_size,
                    style.font_weight,
                    &style.font_family,
                    color,
                    None,
                    parley::layout::Alignment::Start,
                    1.0,
                    style.line_height,
                    style.letter_spacing,
                    true,
                );
                result.text_layouts.insert(dom_key, TextRun { layout });
            }
        }
    }
    let _ = scale;
    result
}

/// Widest <option> label of a <select>. Its options are display:none, so the
/// collapsed control has no laid-out content to size against and would shrink to
/// its padding. Sizing to the widest (rather than the selected) label also keeps
/// the control from resizing as the user changes the selection.
pub fn widest_option_text(doc: &Document, select: NodeKey) -> String {
    let node = match doc.arena.get(select) {
        Some(n) => n,
        None => return String::new(),
    };
    let mut widest = String::new();
    for &c in &node.children {
        if doc.arena.get(c).map_or(false, |n| n.tag == "option") {
            let t = doc.collect_text(c).trim().to_string();
            if t.chars().count() > widest.chars().count() {
                widest = t;
            }
        }
    }
    widest
}

/// Label shown by a collapsed <select>: the text of its selected <option>
/// (the one marked `selected`, else the first).
pub fn selected_option_text(doc: &Document, select: NodeKey) -> String {
    let node = match doc.arena.get(select) {
        Some(n) => n,
        None => return String::new(),
    };
    let mut first: Option<NodeKey> = None;
    for &c in &node.children {
        if doc.arena.get(c).map_or(false, |n| n.tag == "option") {
            if first.is_none() {
                first = Some(c);
            }
            if doc.arena.get(c).map_or(false, |n| n.attr("selected").is_some()) {
                return doc.collect_text(c).trim().to_string();
            }
        }
    }
    first.map(|c| doc.collect_text(c).trim().to_string()).unwrap_or_default()
}

pub fn input_display_text(
    node: &super::dom::Node,
    style: Option<&Computed>,
) -> (String, [f32; 4]) {
    let value = node.attr("value").unwrap_or("");
    if !value.is_empty() {
        let shown = if node.attr("type") == Some("password") {
            "\u{2022}".repeat(value.chars().count())
        } else {
            value.to_string()
        };
        let c = style
            .map(|s| [s.color.r, s.color.g, s.color.b, s.color.a])
            .unwrap_or([0.1, 0.15, 0.2, 1.0]);
        (shown, c)
    } else {
        // Sciter uses `novalue` for placeholder text; also accept HTML placeholder.
        let placeholder = node
            .attr("novalue")
            .or_else(|| node.attr("placeholder"))
            .unwrap_or("");
        (placeholder.to_string(), [0.58, 0.64, 0.72, 1.0])
    }
}

fn axis_is_row(flow: Flow, children_inline: bool) -> bool {
    match flow {
        Flow::Horizontal | Flow::HorizontalWrap => true,
        Flow::Vertical => false,
        Flow::Text => true,
        Flow::Default => children_inline,
    }
}

fn build_node(
    doc: &Document,
    styles: &HashMap<NodeKey, Computed>,
    key: NodeKey,
    parent_row_axis: Option<bool>,
    tree: &mut TaffyTree<NodeCtx>,
    map: &mut HashMap<NodeKey, taffy::NodeId>,
    absolute_set: &mut std::collections::HashSet<NodeKey>,
) -> Option<(taffy::NodeId, Vec<(taffy::NodeId, bool)>)> {
    let node = doc.arena.get(key)?;
    let style = styles.get(&key).cloned().unwrap_or_default();

    if style.display == DisplayKind::None || !style.visible {
        return None;
    }

    if let Some(text) = node.text_content() {
        let ctx = NodeCtx {
            dom: key,
            text: Some(style.text_transform.apply(text)),
            font_size: style.font_size,
            font_weight: style.font_weight,
            font_family: style.font_family.clone(),
            color: [style.color.r, style.color.g, style.color.b, style.color.a],
            line_height: style.line_height,
            letter_spacing: style.letter_spacing,
            nowrap: style.white_space_nowrap,
            extra_width: 0.0,
        };
        let taffy_style = Style::default();
        let id = tree.new_leaf_with_context(taffy_style, ctx).ok()?;
        map.insert(key, id);
        return Some((id, Vec::new()));
    }

    // display:none children are not laid out, so they must not influence the
    // "all children are inline" heuristic (a hidden progress next to inline
    // buttons was flipping the row to a vertical column, stacking the buttons).
    let laid_out: Vec<NodeKey> = node
        .children
        .iter()
        .copied()
        .filter(|c| {
            styles
                .get(c)
                .map_or(true, |s| s.display != DisplayKind::None && s.visible)
        })
        .collect();
    // Whitespace-only text children (e.g. a false JSX conditional rendering
    // "") must not inline-ize a block container - that turned the settings
    // panel's column of sections into a wrapping row.
    let meaningful_text = |c: &NodeKey| {
        doc.arena.get(*c).map_or(false, |n| {
            n.is_text() && n.text_content().map_or(false, |t| !t.trim().is_empty())
        })
    };
    let children_inline = laid_out.iter().any(meaningful_text)
        || (!laid_out.is_empty()
            && laid_out.iter().all(|c| {
                let cs = styles.get(c);
                doc.arena.get(*c).map_or(false, |n| n.is_text())
                    || cs.map_or(false, |s| {
                        matches!(s.display, DisplayKind::Inline | DisplayKind::InlineBlock)
                    })
            }));

    let row_axis = axis_is_row(style.flow, children_inline);

    let mut ts = Style {
        display: Display::Flex,
        flex_direction: if row_axis {
            FlexDirection::Row
        } else {
            FlexDirection::Column
        },
        flex_wrap: if style.flow == Flow::HorizontalWrap || (children_inline && style.flow == Flow::Default)
        {
            FlexWrap::Wrap
        } else {
            FlexWrap::NoWrap
        },
        ..Style::default()
    };

    // Sciter's `padding: *` (flexible star padding on every side) centres the
    // content HORIZONTALLY but leaves it TOP-anchored vertically -- measured
    // against the real engine with the session-card platform idiom (the icon
    // sits at the platform top; the bottom-anchored label owns the lower band).
    // taffy has no star padding, so map it to alignment with zero padding.
    let star_padding = style
        .padding
        .iter()
        .all(|p| matches!(p, Length::Star(_)));

    if star_padding {
        if row_axis {
            ts.justify_content = Some(JustifyContent::Center);
            ts.align_items = Some(AlignItems::FlexStart);
        } else {
            ts.align_items = Some(AlignItems::Center);
            ts.justify_content = Some(JustifyContent::FlexStart);
        }
    } else if row_axis {
        // vertical-align wins when set (the Copy/Invite/Set pills use middle). Else
        // an EXPLICIT flow:horizontal top-aligns its children (so the card footer's
        // padded .text>div positions its text via padding-top), while implicit inline
        // rows (button/label text) stay vertically centred.
        ts.align_items = Some(match style.valign {
            VAlign::Middle => AlignItems::Center,
            VAlign::Top => AlignItems::FlexStart,
            VAlign::Bottom => AlignItems::FlexEnd,
            VAlign::Default => match style.flow {
                Flow::Horizontal | Flow::HorizontalWrap => AlignItems::FlexStart,
                _ => AlignItems::Center,
            },
        });
        ts.justify_content = Some(match style.text_align {
            // A FIXED-width ellipsis box (the CM tab: width:70px) anchors its text
            // at the start and clips the end -- centering an overflowing name
            // clips both ends. Grow/auto-width centered buttons keep centering.
            TextAlign::Center
                if style.text_ellipsis && matches!(style.width, Length::Px(_)) =>
            {
                JustifyContent::FlexStart
            }
            TextAlign::Right => JustifyContent::FlexEnd,
            TextAlign::Center => JustifyContent::Center,
            TextAlign::Left => JustifyContent::FlexStart,
        });
    } else if node.tag == "center" {
        // <center> centres its children on the inline axis (the 2FA dialog's
        // QR image and code input), unlike a plain block's stretch.
        ts.align_items = Some(AlignItems::Center);
    } else {
        ts.align_items = Some(AlignItems::Stretch);
    }

    // horizontal-align:center centers this block within its container's cross
    // axis (the empty-state / footer brand art), overriding the parent's stretch.
    if style.halign_center {
        ts.align_self = Some(AlignItems::Center);
    }

    // An inline-block with no explicit width shrink-wraps to its content instead
    // of stretching to the parent's width (the CM "Security Code" button was
    // filling the whole info column). Skip when the element sets its own width.
    if style.display == DisplayKind::InlineBlock
        && matches!(style.width, Length::Auto)
        && !style.halign_center
    {
        ts.align_self = Some(AlignItems::FlexStart);
    }

    // Column containers honour vertical-align on the MAIN axis (the palette
    // width-dot: a block child inside a vertical-align:middle button centres
    // vertically).
    if !row_axis && !star_padding {
        match style.valign {
            VAlign::Middle => ts.justify_content = Some(JustifyContent::Center),
            VAlign::Bottom => ts.justify_content = Some(JustifyContent::FlexEnd),
            _ => {}
        }
    }

    let is_absolute = matches!(style.position, Position::Absolute | Position::Fixed);
    ts.position = if is_absolute {
        taffy::Position::Absolute
    } else {
        taffy::Position::Relative
    };
    ts.inset = Rect {
        left: to_lpa(style.left),
        right: to_lpa(style.right),
        top: to_lpa(style.top),
        bottom: to_lpa(style.bottom),
    };
    if is_absolute
        && matches!(style.left, Length::Auto)
        && matches!(style.top, Length::Auto)
        && matches!(style.right, Length::Auto)
        && matches!(style.bottom, Length::Auto)
    {
        ts.inset.left = LengthPercentageAuto::Length(0.0);
        ts.inset.top = LengthPercentageAuto::Length(0.0);
    }

    let parent_row = parent_row_axis.unwrap_or(false);
    if is_absolute {
        ts.size = Size {
            width: match style.width {
                Length::Star(_) => Dimension::Percent(1.0),
                other => to_dimension_simple(other),
            },
            height: match style.height {
                Length::Star(_) => Dimension::Percent(1.0),
                other => to_dimension_simple(other),
            },
        };
    } else {
        ts.size = Size {
            width: to_dimension(style.width, parent_row, true, &mut ts),
            height: to_dimension(style.height, parent_row, false, &mut ts),
        };
    }
    // Sciter (like CSS default) is content-box: an explicit width/height is the
    // CONTENT size and padding+border add around it. taffy 0.5 has no box_sizing
    // and treats size as the border-box, which made e.g. the window controls
    // (width:22px; padding:0 10px) render 22px instead of 42px. Emulate content-box
    // for definite pixel sizes by folding padding+border into the size.
    let px = |l: Length| match l {
        Length::Px(v) => v,
        _ => 0.0,
    };
    let h_extra = px(style.padding[1]) + px(style.padding[3])
        + style.border_width[1] + style.border_width[3];
    let v_extra = px(style.padding[0]) + px(style.padding[2])
        + style.border_width[0] + style.border_width[2];
    if let Length::Px(w) = style.width {
        ts.size.width = Dimension::Length(w + h_extra);
    }
    if let Length::Px(h) = style.height {
        ts.size.height = Dimension::Length(h + v_extra);
    }
    // An inline <svg> with no CSS/attribute size gets a font-sized square (the
    // file-transfer Send/Receive arrows) instead of collapsing to 0x0.
    if node.tag == "svg"
        && matches!(style.width, Length::Auto)
        && matches!(style.height, Length::Auto)
        && node.attr("width").is_none()
        && node.attr("height").is_none()
    {
        ts.size = Size {
            width: Dimension::Length(style.font_size + h_extra),
            height: Dimension::Length(style.font_size + v_extra),
        };
    }
    if doc.root == key {
        ts.size = Size {
            width: Dimension::Percent(1.0),
            height: Dimension::Percent(1.0),
        };
    }
    // A flex item with an explicit pixel size on the parent's MAIN axis keeps
    // that size (Sciter honors it in flow) rather than shrinking to fit as
    // default flexbox would: fixed-width session tiles wrap instead of
    // squishing (row parent), and fixed-height rows overflow a scroll
    // container instead of compressing (column parent).
    let main_definite = if parent_row {
        matches!(style.width, Length::Px(_))
    } else {
        matches!(style.height, Length::Px(_))
    };
    if !is_absolute && main_definite && ts.flex_grow == 0.0 {
        ts.flex_shrink = 0.0;
    }
    // A scroll container lets its content overflow (and be clipped+scrolled at
    // paint time) instead of compressing children to fit.
    if style.scroll_y {
        ts.overflow = taffy::Point {
            x: taffy::Overflow::Visible,
            y: taffy::Overflow::Scroll,
        };
    }
    let is_body = doc
        .arena
        .get(key)
        .map_or(false, |n| n.tag == "body" || n.tag == "html");
    if is_body && matches!(style.height, Length::Auto) && !is_absolute {
        ts.flex_grow = 1.0;
        ts.flex_shrink = 1.0;
        ts.flex_basis = Dimension::Length(0.0);
    }
    ts.min_size = Size {
        width: to_dimension_simple(style.min_width),
        height: to_dimension_simple(style.min_height),
    };
    // CSS automatic minimum size: a flex item whose overflow is not visible may
    // shrink below its content. Without this an unbreakable string (a file path,
    // a device name) holds its box open at natural width and shoves the row's
    // later children out of view instead of being clipped/ellipsised.
    if (style.overflow_hidden || style.text_ellipsis)
        && matches!(style.min_width, Length::Auto)
    {
        ts.min_size.width = Dimension::Length(0.0);
    }
    // The vertical analog: a vertical scroll container may shrink below its
    // content (that is what the scrollbar is for). Without this its content-tall
    // automatic minimum holds the box open, so a long file list grows the whole
    // window instead of scrolling inside a fixed pane.
    if (style.scroll_y || is_body) && matches!(style.min_height, Length::Auto) {
        ts.min_size.height = Dimension::Length(0.0);
    }
    ts.max_size = Size {
        width: to_dimension_simple(style.max_width),
        height: to_dimension_simple(style.max_height),
    };
    ts.margin = Rect {
        top: to_lpa(style.margin[0]),
        right: to_lpa(style.margin[1]),
        bottom: to_lpa(style.margin[2]),
        left: to_lpa(style.margin[3]),
    };
    // On an absolutely-positioned box an auto/star margin only means "absorb the
    // free space" when BOTH insets on that axis are set (true centering between
    // them). Otherwise Sciter resolves it to 0, whereas taffy would let a single
    // auto cross-axis margin push the box off its inset. Zero those so a badge
    // authored `top:6px; margin:* 0` stays at top:6 instead of drifting down.
    if is_absolute {
        let auto_m = |l: Length| matches!(l, Length::Star(_) | Length::Auto);
        let set = |l: Length| !matches!(l, Length::Auto);
        if !(set(style.top) && set(style.bottom)) {
            if auto_m(style.margin[0]) {
                ts.margin.top = LengthPercentageAuto::Length(0.0);
            }
            if auto_m(style.margin[2]) {
                ts.margin.bottom = LengthPercentageAuto::Length(0.0);
            }
        }
        if !(set(style.left) && set(style.right)) {
            if auto_m(style.margin[3]) {
                ts.margin.left = LengthPercentageAuto::Length(0.0);
            }
            if auto_m(style.margin[1]) {
                ts.margin.right = LengthPercentageAuto::Length(0.0);
            }
        }
    }
    // `padding: *` makes the box a spring: besides centering its own content
    // (justify/align above), the definite-size box floats centered in its
    // parent's free space -- measured on the real session-card platform, which
    // is 100px tall but sits vertically centered in the 120px card. In flexbox
    // that self-centering is auto margins (no per-item main-axis justify).
    if star_padding && !is_absolute {
        ts.margin = Rect {
            top: LengthPercentageAuto::Auto,
            right: LengthPercentageAuto::Auto,
            bottom: LengthPercentageAuto::Auto,
            left: LengthPercentageAuto::Auto,
        };
    }
    ts.padding = Rect {
        top: to_lp(style.padding[0]),
        right: to_lp(style.padding[1]),
        bottom: to_lp(style.padding[2]),
        left: to_lp(style.padding[3]),
    };
    ts.border = Rect {
        top: LengthPercentage::Length(style.border_width[0]),
        right: LengthPercentage::Length(style.border_width[1]),
        bottom: LengthPercentage::Length(style.border_width[2]),
        left: LengthPercentage::Length(style.border_width[3]),
    };
    ts.gap = Size {
        width: LengthPercentage::Length(style.gap[0]),
        height: LengthPercentage::Length(style.gap[1]),
    };

    let mut child_ids = Vec::new();
    // (taffy node, is_fixed): an absolute descendant bubbles up to its nearest
    // positioned ancestor; a fixed one bubbles all the way to the viewport
    // (root) -- otherwise a fixed popup positioned in view coords lands shifted
    // by its offset ancestor (the remote Display menu appeared far right).
    let mut pending: Vec<(taffy::NodeId, bool)> = Vec::new();
    if node.tag != "svg" {
        for &child in &node.children {
            let child_pos = styles.get(&child).map(|s| s.position);
            let child_abs =
                matches!(child_pos, Some(Position::Absolute) | Some(Position::Fixed));
            let child_fixed = matches!(child_pos, Some(Position::Fixed));
            if let Some((id, mut child_pending)) =
                build_node(doc, styles, child, Some(row_axis), tree, map, absolute_set)
            {
                if child_abs {
                    absolute_set.insert(child);
                    pending.push((id, child_fixed));
                } else {
                    child_ids.push(id);
                }
                pending.append(&mut child_pending);
            }
        }
    }

    let is_root = doc.root == key;
    let cb_for_absolute = style.position != Position::Static || is_root;
    let mut bubble = Vec::new();
    for (id, is_fixed) in pending {
        let caught = if is_fixed { is_root } else { cb_for_absolute };
        if caught {
            child_ids.push(id);
        } else {
            bubble.push((id, is_fixed));
        }
    }

    // A collapsed <select> is childless once its display:none options are
    // dropped, so taffy measures it as an empty box unless we hand it the label
    // it will actually paint.
    let select_label = if node.tag == "select" && child_ids.is_empty() {
        match node.attr("value").filter(|_| node.attr("editable").is_some()) {
            Some(v) if !v.is_empty() => Some(v.to_string()),
            _ => Some(widest_option_text(doc, key)).filter(|t| !t.is_empty()),
        }
    } else {
        None
    };
    let has_select_label = select_label.is_some();
    let ctx = NodeCtx {
        dom: key,
        text: select_label,
        font_size: style.font_size,
        font_weight: style.font_weight,
        font_family: style.font_family.clone(),
        color: [0.0; 4],
        line_height: style.line_height,
        letter_spacing: style.letter_spacing,
        nowrap: style.white_space_nowrap || has_select_label,
        extra_width: if has_select_label { SELECT_CHEVRON_WIDTH } else { 0.0 },
    };
    let id = tree.new_with_children(ts, &child_ids).ok()?;
    tree.set_node_context(id, Some(ctx)).ok();
    map.insert(key, id);
    Some((id, bubble))
}

fn to_dimension(l: Length, parent_row: bool, is_width: bool, ts: &mut Style) -> Dimension {
    match l {
        Length::Auto => Dimension::Auto,
        Length::Px(v) => Dimension::Length(v),
        Length::Percent(p) => Dimension::Percent(p / 100.0),
        Length::Star(f) => {
            let main_axis = parent_row == is_width;
            if main_axis {
                ts.flex_grow = f;
                ts.flex_shrink = 1.0;
                ts.flex_basis = Dimension::Length(0.0);
                Dimension::Auto
            } else {
                Dimension::Percent(1.0)
            }
        }
    }
}

fn to_dimension_simple(l: Length) -> Dimension {
    match l {
        Length::Auto => Dimension::Auto,
        Length::Px(v) => Dimension::Length(v),
        Length::Percent(p) => Dimension::Percent(p / 100.0),
        Length::Star(_) => Dimension::Auto,
    }
}

fn to_lpa(l: Length) -> LengthPercentageAuto {
    match l {
        Length::Auto => LengthPercentageAuto::Auto,
        Length::Px(v) => LengthPercentageAuto::Length(v),
        Length::Percent(p) => LengthPercentageAuto::Percent(p / 100.0),
        Length::Star(_) => LengthPercentageAuto::Auto,
    }
}

fn to_lp(l: Length) -> LengthPercentage {
    match l {
        Length::Px(v) => LengthPercentage::Length(v),
        Length::Percent(p) => LengthPercentage::Percent(p / 100.0),
        _ => LengthPercentage::Length(0.0),
    }
}

fn collect_rects(
    tree: &TaffyTree<NodeCtx>,
    id: taffy::NodeId,
    origin: (f32, f32),
    out: &mut HashMap<NodeKey, (f32, f32, f32, f32)>,
) {
    let layout = match tree.layout(id) {
        Ok(l) => l,
        Err(_) => return,
    };
    let abs = (origin.0 + layout.location.x, origin.1 + layout.location.y);
    if let Some(Some(ctx)) = tree.get_node_context(id).map(Some) {
        out.insert(ctx.dom, (abs.0, abs.1, layout.size.width, layout.size.height));
    }
    let children = tree.children(id).unwrap_or_default();
    for child in children {
        collect_rects(tree, child, abs, out);
    }
}
