//! Minimal single-line URL field: typing, caret movement, paste and select-all.
//! IME composition, partial selection and copy are not supported yet.

use crate::{ACCENT, BG, BORDER, MUTED, RAISED, TEXT};
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton, Window, div,
    prelude::*, px, rgb,
};

pub(crate) enum UrlEvent {
    /// Enter was pressed; the text is trimmed and not yet validated.
    Submit(String),
}

pub(crate) struct UrlInput {
    text: String,
    /// Byte offset of the caret, always on a character boundary.
    cursor: usize,
    /// The next edit replaces everything, like a browser address bar after focus.
    select_all: bool,
    focus: FocusHandle,
}

impl EventEmitter<UrlEvent> for UrlInput {}

impl Focusable for UrlInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl UrlInput {
    pub(crate) fn new(text: String, cx: &mut Context<Self>) -> Self {
        Self {
            cursor: text.len(),
            text,
            select_all: false,
            focus: cx.focus_handle(),
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Shows `text` unless the user is editing the field.
    pub(crate) fn show(&mut self, text: &str, window: &Window, cx: &mut Context<Self>) {
        if self.focus.is_focused(window) || self.text == text {
            return;
        }
        self.text = text.to_owned();
        self.cursor = self.text.len();
        cx.notify();
    }

    pub(crate) fn focus_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window);
        self.select_all = true;
        self.cursor = self.text.len();
        cx.notify();
    }

    fn insert(&mut self, text: &str) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if self.select_all {
            self.text.clear();
            self.cursor = 0;
            self.select_all = false;
        }
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    fn previous(&self) -> usize {
        self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        let shortcut = modifiers.control || modifiers.alt || modifiers.platform;
        match keystroke.key.as_str() {
            "enter" => {
                self.select_all = false;
                cx.emit(UrlEvent::Submit(self.text.trim().to_owned()));
            }
            "escape" => {
                self.select_all = false;
                window.blur();
            }
            "backspace" | "delete" if self.select_all => {
                self.text.clear();
                self.cursor = 0;
                self.select_all = false;
            }
            "backspace" => {
                let start = self.previous();
                self.text.replace_range(start..self.cursor, "");
                self.cursor = start;
            }
            "delete" => {
                let end = self.next();
                self.text.replace_range(self.cursor..end, "");
            }
            "left" => {
                self.select_all = false;
                self.cursor = self.previous();
            }
            "right" => {
                self.select_all = false;
                self.cursor = self.next();
            }
            "home" => {
                self.select_all = false;
                self.cursor = 0;
            }
            "end" => {
                self.select_all = false;
                self.cursor = self.text.len();
            }
            "a" if modifiers.control => self.select_all = true,
            "v" if modifiers.control => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.insert(text.lines().next().unwrap_or_default().trim());
                }
            }
            _ if !shortcut => match &keystroke.key_char {
                Some(text) => self.insert(text),
                None => return,
            },
            // Leave other shortcuts to the application.
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for UrlInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let (before, after) = self.text.split_at(self.cursor);
        let text = if focused && self.select_all {
            div()
                .bg(rgb(RAISED))
                .text_color(rgb(ACCENT))
                .child(self.text.clone())
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .child(before.to_owned())
                .when(focused, |this| {
                    this.child(div().w(px(1.5)).h(px(16.)).bg(rgb(TEXT)))
                })
                .child(after.to_owned())
                .into_any_element()
        };
        div()
            .id("url-input")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|input, _, window, cx| {
                    if !input.focus.is_focused(window) {
                        input.focus_all(window, cx);
                    }
                }),
            )
            .flex_1()
            .min_w_0()
            .h(px(32.))
            .flex()
            .items_center()
            .px_3()
            .rounded_md()
            .border_1()
            .border_color(rgb(if focused { ACCENT } else { BORDER }))
            .bg(rgb(BG))
            .text_sm()
            .text_color(rgb(if self.text.is_empty() { MUTED } else { TEXT }))
            .overflow_hidden()
            .whitespace_nowrap()
            .child(text)
    }
}
