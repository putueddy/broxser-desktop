//! Graphite & Signal: the shell's palette, type, icons and the few pieces every
//! view repeats (ADR 0027). Filled controls keep one solid color per kind, lime
//! to confirm and coral to remove or clear, so a real-window check can still
//! find each kind by its color alone.

use gpui::{
    BoxShadow, Div, ElementId, Hsla, SharedString, Stateful, Svg, div, point, prelude::*, px, rgb,
    rgba, svg,
};
use std::borrow::Cow;

/// The device stage behind the cards.
pub(crate) const CANVAS: u32 = 0x0b0c0e;
/// Toolbar, sidebar, panels and the status bar.
pub(crate) const CHROME: u32 = 0x111316;
pub(crate) const CARD: u32 = 0x15181c;
/// Secondary buttons and keys.
pub(crate) const RAISED: u32 = 0x171a1e;
/// Selected rows and the active tab.
pub(crate) const HOVER: u32 = 0x1d2126;
/// Field and note wells inside chrome.
pub(crate) const WELL: u32 = 0x0e1013;
pub(crate) const BORDER: u32 = 0x23272d;
pub(crate) const BORDER_STRONG: u32 = 0x2a2e35;
pub(crate) const DIVIDER: u32 = 0x1c1f24;
pub(crate) const TEXT: u32 = 0xeceef0;
pub(crate) const TEXT_2: u32 = 0xb4bac3;
/// Captions; 5.9:1 on chrome.
pub(crate) const MUTED: u32 = 0x8a929c;
/// Decorative strokes only, never text.
pub(crate) const FAINT: u32 = 0x6b737d;
pub(crate) const ACCENT: u32 = 0xc6f26b;
/// Text on filled buttons.
pub(crate) const INK: u32 = 0x0b0c0e;
pub(crate) const INFO: u32 = 0x7cb4ff;
pub(crate) const WARN: u32 = 0xf2b35e;
pub(crate) const WARN_TEXT: u32 = 0xf5c784;
pub(crate) const DANGER: u32 = 0xff7b72;
pub(crate) const DANGER_TEXT: u32 = 0xffa39c;
/// A page dialog's panel, amber on the card.
pub(crate) const DIALOG_FILL: u32 = 0x25221d;
/// RGBA: the accent at 13%, for toggles that are on.
pub(crate) const ACCENT_SOFT: u32 = 0xc6f26b21;
/// RGBA: the edge of a standalone toggle that is on.
pub(crate) const ACCENT_EDGE: u32 = 0xc6f26b59;
/// Session tags in workspace order; sessions 7 and 8 repeat the first two.
pub(crate) const SESSION_HUES: [u32; 6] =
    [0xa898ff, 0x4fd1c5, 0xf28dc0, 0xd6b48c, 0x8ea2ff, 0xd39bf0];

pub(crate) const SANS: &str = "Geist";
pub(crate) const MONO: &str = "Geist Mono";

/// The bundled faces (SIL Open Font License 1.1, `fonts/README.md`).
pub(crate) fn fonts() -> Vec<Cow<'static, [u8]>> {
    vec![
        Cow::Borrowed(include_bytes!("../fonts/Geist-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../fonts/Geist-Medium.ttf")),
        Cow::Borrowed(include_bytes!("../fonts/Geist-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!("../fonts/GeistMono-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../fonts/GeistMono-Medium.ttf")),
    ]
}

macro_rules! icons {
    ($($variant:ident => $file:literal,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum Icon { $($variant,)* }

        impl Icon {
            fn path(self) -> &'static str {
                match self { $(Icon::$variant => concat!("icons/", $file),)* }
            }
        }

        /// The icon an asset path names, if any.
        pub(crate) fn icon_asset(path: &str) -> Option<&'static [u8]> {
            match path {
                $(concat!("icons/", $file) => Some(include_bytes!(concat!("../icons/", $file))),)*
                _ => None,
            }
        }
    };
}

icons! {
    Brand => "brand.svg",
    Globe => "globe.svg",
    Go => "go.svg",
    Reload => "reload.svg",
    Links => "links.svg",
    Scroll => "scroll.svg",
    Minus => "minus.svg",
    Plus => "plus.svg",
    Workspace => "workspace.svg",
    Console => "console.svg",
    Phone => "phone.svg",
    Tablet => "tablet.svg",
    Desktop => "desktop.svg",
    Visible => "visible.svg",
    Hidden => "hidden.svg",
    Info => "info.svg",
    Close => "close.svg",
    Warning => "warning.svg",
    Error => "error.svg",
    Dialog => "dialog.svg",
    Window => "window.svg",
    Download => "download.svg",
    Report => "report.svg",
    Clear => "clear.svg",
    File => "file.svg",
    Save => "save.svg",
    Paused => "paused.svg",
    Loading => "loading.svg",
    Static => "static.svg",
    Navigated => "navigated.svg",
}

/// A stroke icon tinted `color`.
pub(crate) fn icon(icon: Icon, size: f32, color: u32) -> Svg {
    svg()
        .path(icon.path())
        .size(px(size))
        .flex_none()
        .text_color(rgb(color))
}

/// The glyph of a viewport: phones, then tablets and other mobile or narrow
/// viewports, then desktops.
pub(crate) fn device_icon(width: u32, mobile: bool) -> Icon {
    if width < 600 {
        Icon::Phone
    } else if mobile || width < 1024 {
        Icon::Tablet
    } else {
        Icon::Desktop
    }
}

/// The tag color of the session at `index` in the workspace.
pub(crate) fn session_hue(index: usize) -> u32 {
    SESSION_HUES[index % SESSION_HUES.len()]
}

/// `color` at `alpha` (0–255) for tinted fills.
pub(crate) fn tint(color: u32, alpha: u8) -> gpui::Rgba {
    rgba((color << 8) | u32::from(alpha))
}

fn shadow(color: Hsla, y: f32, blur: f32, spread: f32) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
    }
}

/// A card lifted off the canvas; selected cards add a lime ring.
pub(crate) fn card_shadow(selected: bool) -> Vec<BoxShadow> {
    let mut shadows = vec![shadow(tint(0x000000, 0xcc).into(), 18., 40., -24.)];
    if selected {
        shadows.push(shadow(tint(ACCENT, 0x33).into(), 0., 0., 3.));
    }
    shadows
}

/// A device frame: a faint light edge over a short drop.
pub(crate) fn frame_shadow() -> Vec<BoxShadow> {
    vec![
        shadow(tint(0x000000, 0xb3).into(), 10., 24., -12.),
        shadow(tint(0xffffff, 0x12).into(), 0., 0., 1.),
    ]
}

/// A round status dot.
pub(crate) fn dot(color: u32, size: f32) -> Div {
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(rgb(color))
}

/// A small upper-case section label.
pub(crate) fn caps(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(color))
        .child(text.into())
}

/// Monospaced text: addresses, sizes, counts and console lines.
pub(crate) fn mono(text: impl Into<SharedString>, size: f32, color: u32) -> Div {
    div()
        .font_family(MONO)
        .text_size(px(size))
        .text_color(rgb(color))
        .child(text.into())
}

/// A key cap in a shortcut hint.
pub(crate) fn kbd(key: &'static str) -> Div {
    div()
        .h(px(18.))
        .px(px(5.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(rgb(0x2f343b))
        .bg(rgb(RAISED))
        .font_family(MONO)
        .text_size(px(10.5))
        .text_color(rgb(0xa4abb5))
        .child(key)
}

/// Keys and what they do, for the status bar.
pub(crate) fn shortcut(keys: &[&'static str], action: &'static str) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .gap(px(3.))
                .children(keys.iter().map(|key| kbd(key))),
        )
        .child(action)
}

/// A one-pixel vertical rule in a toolbar.
pub(crate) fn divider() -> Div {
    div().w(px(1.)).h(px(22.)).flex_none().bg(rgb(BORDER))
}

/// The lime mark and the name, then the mode chip.
pub(crate) fn brand(mode: Div) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .size(px(22.))
                .flex_none()
                .rounded(px(6.))
                .bg(rgb(ACCENT))
                .child(icon(Icon::Brand, 22., INK)),
        )
        .child(
            div()
                .text_size(px(15.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Broxser"),
        )
        .child(mode)
}

/// A rounded chip with a leading dot, such as a session tag.
pub(crate) fn tag(label: impl Into<SharedString>, color: u32) -> Div {
    div()
        .h(px(20.))
        .px(px(7.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .rounded_full()
        .bg(tint(color, 0x1f))
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgb(color))
        .child(dot(color, 6.))
        .child(label.into())
}

/// How a button is filled.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    /// Lime: go ahead, apply, save, open.
    Primary,
    /// Raised with an edge: everything else that is available.
    Secondary,
    /// Coral: remove or clear.
    Danger,
    /// Unavailable: keeps its label and drops the fill.
    Disabled,
}

/// A button of `height` px. Filled tones do not change on hover, so a window
/// check can find them wherever the pointer is.
pub(crate) fn button(id: impl Into<ElementId>, tone: Tone, height: f32) -> Stateful<Div> {
    let base = div()
        .id(id)
        .h(px(height))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(7.))
        .px(px(if height >= 30. { 12. } else { 9. }))
        .rounded(px(if height >= 30. { 8. } else { 7. }))
        .text_size(px(if height >= 30. { 12.5 } else { 12. }))
        .font_weight(gpui::FontWeight::SEMIBOLD);
    match tone {
        Tone::Primary => base.cursor_pointer().bg(rgb(ACCENT)).text_color(rgb(INK)),
        Tone::Danger => base.cursor_pointer().bg(rgb(DANGER)).text_color(rgb(INK)),
        Tone::Secondary => base
            .cursor_pointer()
            .border_1()
            .border_color(rgb(BORDER_STRONG))
            .bg(rgb(RAISED))
            .text_color(rgb(TEXT))
            .font_weight(gpui::FontWeight::MEDIUM)
            .hover(|style| style.bg(rgb(HOVER))),
        Tone::Disabled => base.bg(rgb(RAISED)).text_color(rgb(MUTED)),
    }
}

/// A 28 px square icon button without a fill until hovered.
pub(crate) fn icon_button(id: impl Into<ElementId>, glyph: Icon, color: u32) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(HOVER)))
        .child(icon(glyph, 15., color))
}

/// A dark well holding segmented buttons.
pub(crate) fn segmented() -> Div {
    div()
        .h(px(34.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(9.))
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(CANVAS))
}

/// One segment: soft lime with lime text when on, plain text when off.
pub(crate) fn segment(
    id: impl Into<ElementId>,
    glyph: Option<Icon>,
    label: &'static str,
    on: bool,
) -> Stateful<Div> {
    let color = if on { ACCENT } else { TEXT_2 };
    div()
        .id(id)
        .h(px(28.))
        .px(px(10.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .rounded(px(7.))
        .cursor_pointer()
        .text_size(px(12.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgb(color))
        .when(on, |this| this.bg(tint(ACCENT, 0x21)))
        .when(!on, |this| this.hover(|style| style.bg(rgb(HOVER))))
        .children(glyph.map(|glyph| icon(glyph, 14., color)))
        .child(label)
}

/// A standalone toolbar toggle with an icon, such as a panel button.
pub(crate) fn toggle(
    id: impl Into<ElementId>,
    glyph: Icon,
    label: &'static str,
    on: bool,
    width: f32,
) -> Stateful<Div> {
    let color = if on { ACCENT } else { TEXT };
    div()
        .id(id)
        .w(px(width))
        .h(px(34.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(7.))
        .rounded(px(9.))
        .border_1()
        .cursor_pointer()
        .text_size(px(13.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgb(color))
        .when(on, |this| {
            this.border_color(rgba(ACCENT_EDGE)).bg(rgba(ACCENT_SOFT))
        })
        .when(!on, |this| {
            this.border_color(rgb(BORDER_STRONG))
                .bg(rgb(RAISED))
                .hover(|style| style.bg(rgb(HOVER)))
        })
        .child(icon(glyph, 14., color))
        .child(label)
}

/// The zoom stepper: raised minus and plus around the percentage.
pub(crate) fn stepper(percent: String, out: Stateful<Div>, into: Stateful<Div>) -> Div {
    let step = |button: Stateful<Div>, glyph: Icon| {
        button
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .bg(rgb(RAISED))
            .cursor_pointer()
            .child(icon(glyph, 14., 0xc9ced5))
    };
    segmented()
        .gap(px(0.))
        .child(step(out, Icon::Minus))
        .child(
            div()
                .w(px(46.))
                .flex()
                .justify_center()
                .font_family(MONO)
                .text_size(px(12.))
                .child(percent),
        )
        .child(step(into, Icon::Plus))
}

/// The bottom bar: a state dot with its word, details, then whatever the
/// caller adds on the right.
pub(crate) fn status_bar() -> Div {
    div()
        .h(px(32.))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(12.))
        .pl(px(16.))
        .pr(px(5.))
        .bg(rgb(CHROME))
        .border_t_1()
        .border_color(rgb(BORDER))
        .text_size(px(12.))
        .text_color(rgb(MUTED))
}

/// A state dot with a soft halo, then its word.
pub(crate) fn state(color: u32, label: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(7.))
        .child(dot(color, 7.).shadow(vec![shadow(tint(color, 0x29).into(), 0., 0., 3.)]))
        .child(
            div()
                .text_color(rgb(TEXT))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(label.into()),
        )
}

/// A labelled note with a leading icon, as in the sidebar's footer.
pub(crate) fn note(glyph: Icon, text: &'static str) -> Div {
    div()
        .flex()
        .gap(px(10.))
        .p(px(12.))
        .rounded(px(10.))
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(WELL))
        .text_size(px(12.))
        .line_height(px(18.))
        .text_color(rgb(0xa4abb5))
        .child(div().pt(px(2.)).child(icon(glyph, 14., MUTED)))
        .child(div().flex_1().min_w_0().child(text))
}

#[cfg(test)]
mod tests {
    use super::{Icon, device_icon, icon_asset, session_hue};

    #[test]
    fn every_icon_is_embedded_as_svg() {
        for icon in [Icon::Brand, Icon::Globe, Icon::Navigated, Icon::Static] {
            let bytes = icon_asset(icon.path()).expect("embedded icon");
            assert!(bytes.starts_with(b"<svg"));
        }
        assert!(icon_asset("icons/missing.svg").is_none());
    }

    #[test]
    fn devices_get_a_glyph_by_viewport_class() {
        assert_eq!(device_icon(390, true), Icon::Phone);
        assert_eq!(device_icon(360, false), Icon::Phone);
        assert_eq!(device_icon(768, true), Icon::Tablet);
        assert_eq!(device_icon(1024, true), Icon::Tablet);
        assert_eq!(device_icon(1366, false), Icon::Desktop);
    }

    #[test]
    fn session_hues_repeat_after_six() {
        assert_eq!(session_hue(0), session_hue(6));
        assert_ne!(session_hue(0), session_hue(1));
    }
}
