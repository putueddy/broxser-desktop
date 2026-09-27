//! Minimal single-line text field: typing, caret movement, paste and select-all.
//! It serves the URL bar and the answer of a prompt dialog. IME composition,
//! partial selection and copy are not supported yet.

use crate::{ACCENT, BG, BORDER, MUTED, RAISED, TEXT};
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Keystroke, MouseButton,
    Window, div, prelude::*, px, rgb,
};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UrlEvent {
    /// Enter was pressed; the text is as typed, not trimmed or validated.
    Submit(String),
    /// Escape was pressed.
    Cancel,
}

pub(crate) struct UrlInput {
    edit: LineEdit,
    focus: FocusHandle,
}

/// The text of a single-line field and how key presses edit it.
#[derive(Debug)]
pub(crate) struct LineEdit {
    text: String,
    /// Byte offset of the caret, always on a character boundary.
    cursor: usize,
    /// The next edit replaces everything, like a browser address bar after focus.
    select_all: bool,
    /// Longest text in characters, if bounded; typing and paste stop there.
    max_chars: Option<usize>,
}

/// What a key press did in a [`LineEdit`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    /// The key is not the field's; it goes on to the application.
    Unhandled,
    /// The field kept the key: it edited the text, moved the caret, or found
    /// no room for more text.
    Edited,
    /// The field kept the key and reports it to its owner.
    Event(UrlEvent),
}

impl EventEmitter<UrlEvent> for UrlInput {}

impl Focusable for UrlInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl UrlInput {
    /// A field showing `text`. With `max_chars`, typing and paste stop at that
    /// many characters.
    pub(crate) fn new(text: String, max_chars: Option<usize>, cx: &mut Context<Self>) -> Self {
        Self {
            edit: LineEdit::new(text, max_chars),
            focus: cx.focus_handle(),
        }
    }

    pub(crate) fn text(&self) -> &str {
        self.edit.text()
    }

    /// Shows `text` unless the user is editing the field.
    pub(crate) fn show(&mut self, text: &str, window: &Window, cx: &mut Context<Self>) {
        if self.focus.is_focused(window) || self.edit.text == text {
            return;
        }
        self.edit.text = text.to_owned();
        self.edit.cursor = self.edit.text.len();
        cx.notify();
    }

    pub(crate) fn focus_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window);
        self.edit.select_all = true;
        self.edit.cursor = self.edit.text.len();
        cx.notify();
    }

    /// Puts select-all back after a key whose repeat this field must ignore:
    /// [`LineEdit::key`] already cleared it, as Enter and Escape do on the
    /// assumption the field is about to submit or cancel.
    pub(crate) fn restore_select_all(&mut self, cx: &mut Context<Self>) {
        self.edit.restore_select_all();
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let clipboard = || cx.read_from_clipboard().and_then(|item| item.text());
        match self.edit.key(&event.keystroke, clipboard) {
            KeyOutcome::Unhandled => return,
            KeyOutcome::Edited => {}
            KeyOutcome::Event(report) => cx.emit(report),
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl LineEdit {
    pub(crate) fn new(text: String, max_chars: Option<usize>) -> Self {
        Self {
            cursor: text.len(),
            text,
            select_all: false,
            max_chars,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Applies a key press. `clipboard` reads the system clipboard's text for
    /// Ctrl+V.
    pub(crate) fn key(
        &mut self,
        keystroke: &Keystroke,
        clipboard: impl FnOnce() -> Option<String>,
    ) -> KeyOutcome {
        let modifiers = &keystroke.modifiers;
        let shortcut = modifiers.control || modifiers.alt || modifiers.platform;
        match keystroke.key.as_str() {
            "enter" => {
                self.select_all = false;
                return KeyOutcome::Event(UrlEvent::Submit(self.text.clone()));
            }
            "escape" => {
                self.select_all = false;
                return KeyOutcome::Event(UrlEvent::Cancel);
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
                if let Some(text) = clipboard() {
                    self.insert(text.lines().next().unwrap_or_default().trim());
                }
            }
            _ if !shortcut => match &keystroke.key_char {
                Some(text) => self.insert(text),
                None => return KeyOutcome::Unhandled,
            },
            // Leave other shortcuts to the application.
            _ => return KeyOutcome::Unhandled,
        }
        KeyOutcome::Edited
    }

    /// Puts select-all back after a key whose repeat the caller ignored: Enter
    /// and Escape in [`key`](Self::key) clear it unconditionally.
    pub(crate) fn restore_select_all(&mut self) {
        self.select_all = true;
    }

    /// Inserts `text` at the caret without its control characters, as far as
    /// the length limit leaves room.
    fn insert(&mut self, text: &str) {
        if self.select_all {
            self.text.clear();
            self.cursor = 0;
            self.select_all = false;
        }
        let room = self.max_chars.map_or(usize::MAX, |max| {
            max.saturating_sub(self.text.chars().count())
        });
        let text: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(room)
            .collect();
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
}

impl Render for UrlInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let edit = &self.edit;
        let (before, after) = edit.text.split_at(edit.cursor);
        let text = if focused && edit.select_all {
            div()
                .bg(rgb(RAISED))
                .text_color(rgb(ACCENT))
                .child(edit.text.clone())
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
            .text_color(rgb(if edit.text.is_empty() { MUTED } else { TEXT }))
            .overflow_hidden()
            .whitespace_nowrap()
            .child(text)
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyOutcome, LineEdit, UrlEvent};
    use gpui::{Keystroke, Modifiers};

    fn stroke(key: &str, key_char: Option<&str>) -> Keystroke {
        Keystroke {
            key: key.into(),
            key_char: key_char.map(Into::into),
            ..Keystroke::default()
        }
    }

    fn control(key: &str) -> Keystroke {
        Keystroke {
            key: key.into(),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            ..Keystroke::default()
        }
    }

    #[test]
    fn a_bounded_field_takes_typing_and_paste_only_up_to_its_limit() {
        let mut field = LineEdit::new("ab".into(), Some(4));
        assert_eq!(
            field.key(&stroke("c", Some("c")), || None),
            KeyOutcome::Edited
        );
        assert_eq!(
            field.key(&stroke("eacute", Some("é")), || None),
            KeyOutcome::Edited
        );
        assert_eq!(field.text(), "abcé");
        // A full field keeps typed keys from the application, and their text out.
        assert_eq!(
            field.key(&stroke("d", Some("d")), || None),
            KeyOutcome::Edited
        );
        assert_eq!(field.text(), "abcé");
        field.key(&stroke("backspace", None), || None);
        // Paste takes the clipboard's first line, trimmed, as far as it fits.
        let clipboard = || Some("  xyz \nsecond line".to_owned());
        assert_eq!(field.key(&control("v"), clipboard), KeyOutcome::Edited);
        assert_eq!(field.text(), "abcx");
        // A select-all paste replaces the text and is cut at the limit too.
        field.key(&control("a"), || None);
        field.key(&control("v"), || Some("0123456789".to_owned()));
        assert_eq!(field.text(), "0123");
        // The URL bar has no limit.
        let mut url = LineEdit::new(String::new(), None);
        url.key(&control("v"), || Some("u".repeat(3000)));
        assert_eq!(url.text().chars().count(), 3000);
    }

    #[test]
    fn enter_reports_the_text_as_typed_and_escape_a_cancel() {
        let mut field = LineEdit::new(String::new(), None);
        for (key, typed) in [("space", " "), ("a", "a"), ("space", " ")] {
            field.key(&stroke(key, Some(typed)), || None);
        }
        assert_eq!(
            field.key(&stroke("enter", None), || None),
            KeyOutcome::Event(UrlEvent::Submit(" a ".into()))
        );
        assert_eq!(
            field.key(&stroke("escape", None), || None),
            KeyOutcome::Event(UrlEvent::Cancel)
        );
        assert_eq!(field.text(), " a ");
    }

    #[test]
    fn restoring_select_all_after_enter_makes_the_next_key_replace_again() {
        let mut field = LineEdit::new("Ada".into(), None);
        field.restore_select_all(); // as a fresh focus leaves it
        assert_eq!(
            field.key(&stroke("enter", None), || None),
            KeyOutcome::Event(UrlEvent::Submit("Ada".into()))
        );
        // A caller for whom this Enter answers nothing, such as an ignored
        // repeat, restores select-all so the next key still replaces the
        // whole proposal instead of appending to it.
        field.restore_select_all();
        assert_eq!(
            field.key(&stroke("x", Some("x")), || None),
            KeyOutcome::Edited
        );
        assert_eq!(field.text(), "x");
    }

    #[test]
    fn keys_without_text_and_other_shortcuts_are_left_to_the_application() {
        let mut field = LineEdit::new("text".into(), None);
        let shift_insert = Keystroke {
            key: "insert".into(),
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            ..Keystroke::default()
        };
        for keystroke in [
            stroke("tab", None),
            stroke("up", None),
            stroke("pagedown", None),
            stroke("f5", None),
            shift_insert,
            control("x"),
            control("l"),
        ] {
            assert_eq!(
                field.key(&keystroke, || Some("clipboard".into())),
                KeyOutcome::Unhandled,
                "{}",
                keystroke.key
            );
        }
        assert_eq!(field.text(), "text");
    }
}
