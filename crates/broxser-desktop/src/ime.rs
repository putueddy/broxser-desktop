//! Transient native composition state. Page text is never copied into this buffer.
use broxser_engine::{ImeAction, MAX_PASTE_CHARS};
use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Origin {
    pub device: usize,
    pub target: u64,
    pub generation: u64,
}

#[derive(Clone, Copy)]
enum Tombstone {
    /// Focus/target was lost while composing. An empty preedit or unmark
    /// separates the old composition from a possible new one, but a commit
    /// immediately after that boundary may still belong to the old target.
    Cancelled { origin: Origin, boundary_seen: bool },
}

#[derive(Default)]
pub(crate) struct ImeBuffer {
    origin: Option<Origin>,
    mark: String,
    selection: Range<usize>,
    /// A cancelled/finalized composition may still produce a terminal native
    /// callback. Keep its provenance until that callback has been consumed.
    tombstone: Option<Tombstone>,
}

impl ImeBuffer {
    pub fn marked_range(&self) -> Option<Range<usize>> {
        (!self.mark.is_empty()).then(|| 0..self.mark.encode_utf16().count())
    }

    pub fn selected_range(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn text_for_range(&self, range: Range<usize>) -> Option<(String, Range<usize>)> {
        let units: Vec<u16> = self.mark.encode_utf16().collect();
        let start = range.start.min(units.len());
        let end = range.end.max(start).min(units.len());
        // Do not return half of a surrogate pair to a native input method.
        let mut start = start;
        let mut end = end;
        if start > 0 && start < units.len() && (0xdc00..=0xdfff).contains(&units[start]) {
            start -= 1;
        }
        if end > 0 && end < units.len() && (0xdc00..=0xdfff).contains(&units[end]) {
            end += 1;
        }
        Some((String::from_utf16(&units[start..end]).ok()?, start..end))
    }

    pub fn has_mark(&self) -> bool {
        !self.mark.is_empty()
    }

    pub fn accepts_replacement_range(&self, range: Option<&Range<usize>>) -> bool {
        range.is_none_or(|range| range == &self.marked_range().unwrap_or(0..0))
    }

    pub fn preedit(
        &mut self,
        current: Option<Origin>,
        text: &str,
        selection: Option<Range<usize>>,
    ) -> Option<(Origin, ImeAction)> {
        if !valid_native_text(text) {
            return None;
        }
        // IMEs can send an empty cleanup preedit after a successful commit.
        // It must not create a phantom composition that survives a field change.
        if text.is_empty() && self.origin.is_none() && self.tombstone.is_none() {
            return None;
        }
        if let Some(tombstone) = self.tombstone.as_mut() {
            match tombstone {
                Tombstone::Cancelled { boundary_seen, .. } if text.is_empty() => {
                    *boundary_seen = true;
                    return None;
                }
                Tombstone::Cancelled {
                    boundary_seen: true,
                    ..
                } => {
                    current?;
                    // A nonempty preedit after an observed boundary establishes
                    // a fresh composition. Keep the old tombstone until then.
                    self.tombstone = None;
                }
                Tombstone::Cancelled { .. } => return None,
            }
        }
        let origin = match self.origin {
            Some(origin) if Some(origin) == current => origin,
            Some(_) => return None,
            None => current?,
        };
        self.origin = Some(origin);
        self.mark = text.to_owned();
        let len = text.encode_utf16().count();
        self.selection = selection
            .filter(|range| range.start <= range.end)
            .map(|range| {
                normalize_utf16_offset(text, range.start.min(len))
                    ..normalize_utf16_offset(text, range.end.min(len))
            })
            .unwrap_or(len..len);
        if text.is_empty() {
            // XIM can clear a preedit immediately before sending its commit.
            // Keep the origin for that terminal callback.
            Some((origin, ImeAction::Cancel))
        } else {
            Some((
                origin,
                ImeAction::Preedit {
                    text: self.mark.clone(),
                    selection: self.selection.clone(),
                },
            ))
        }
    }

    pub fn commit(&mut self, current: Option<Origin>, text: &str) -> Option<(Origin, ImeAction)> {
        if let Some(Tombstone::Cancelled {
            origin: _old_origin,
            ..
        }) = self.tombstone.take()
        {
            return None;
        }
        if text.is_empty() || !valid_native_text(text) {
            return None;
        }
        let origin = match self.origin {
            Some(origin) if Some(origin) == current => origin,
            Some(_) => return None,
            None => current?,
        };
        self.origin = None;
        self.mark.clear();
        self.selection = 0..0;
        Some((
            origin,
            ImeAction::Commit {
                text: text.to_owned(),
            },
        ))
    }

    pub fn unmark(&mut self, current: Option<Origin>) -> Option<(Origin, ImeAction)> {
        if let Some(tombstone) = self.tombstone.as_mut() {
            let Tombstone::Cancelled { boundary_seen, .. } = tombstone;
            *boundary_seen = true;
            return None;
        }
        let origin = self.origin?;
        if Some(origin) != current {
            return None;
        }
        let text = std::mem::take(&mut self.mark);
        self.origin = None;
        self.selection = 0..0;
        // Unmark is finalization, not cancellation. GPUI's pointer reset uses
        // it to preserve marked text; XIM ResetIcReply is not another commit.
        // The next native commit is fresh and must not be swallowed.
        (!text.is_empty()).then_some((origin, ImeAction::Commit { text }))
    }

    pub fn invalidate(&mut self) -> Option<(Origin, ImeAction)> {
        let origin = self.origin.take()?;
        self.mark.clear();
        self.selection = 0..0;
        self.tombstone = Some(Tombstone::Cancelled {
            origin,
            boundary_seen: false,
        });
        Some((origin, ImeAction::Cancel))
    }

    pub fn terminal_delete(&mut self) -> Option<(Origin, ImeAction)> {
        if let Some(tombstone) = self.tombstone.as_mut() {
            let Tombstone::Cancelled { boundary_seen, .. } = tombstone;
            *boundary_seen = true;
            None
        } else {
            let action = self.invalidate();
            if let Some(Tombstone::Cancelled { boundary_seen, .. }) = self.tombstone.as_mut() {
                // DeleteText is itself the native end-of-composition boundary.
                *boundary_seen = true;
            }
            action
        }
    }
}

fn valid_native_text(text: &str) -> bool {
    text.chars().take(MAX_PASTE_CHARS + 1).count() <= MAX_PASTE_CHARS
        && !text
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
}

fn normalize_utf16_offset(text: &str, offset: usize) -> usize {
    let mut boundary = 0;
    for ch in text.chars() {
        let next = boundary + ch.len_utf16();
        if next > offset {
            break;
        }
        boundary = next;
    }
    boundary
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Origin = Origin {
        device: 0,
        target: 4,
        generation: 1,
    };
    const B: Origin = Origin {
        device: 1,
        target: 7,
        generation: 2,
    };

    #[test]
    fn preedit_uses_utf16_offsets_and_stays_local() {
        let mut state = ImeBuffer::default();
        assert_eq!(
            state.preedit(Some(A), "a😀", Some(1..3)),
            Some((
                A,
                ImeAction::Preedit {
                    text: "a😀".into(),
                    selection: 1..3
                }
            ))
        );
        assert_eq!(state.marked_range(), Some(0..3));
        assert_eq!(state.selected_range(), 1..3);
        assert_eq!(state.text_for_range(2..2), Some(("😀".into(), 1..3)));
        assert!(state.accepts_replacement_range(Some(&(0..3))));
        assert!(!state.accepts_replacement_range(Some(&(3..4))));
        assert_eq!(
            state.commit(Some(A), "漢"),
            Some((A, ImeAction::Commit { text: "漢".into() }))
        );
        assert_eq!(state.marked_range(), None);
    }

    #[test]
    fn repeated_native_commits_need_no_keyup() {
        let mut state = ImeBuffer::default();
        for _ in 0..2 {
            assert_eq!(
                state.commit(Some(A), "a"),
                Some((A, ImeAction::Commit { text: "a".into() }))
            );
        }
    }

    #[test]
    fn cleanup_after_commit_does_not_block_composition_in_the_next_field() {
        let mut state = ImeBuffer::default();
        state.preedit(Some(A), "ni hao", None);
        assert!(state.commit(Some(A), "你好").is_some());
        assert_eq!(state.preedit(Some(A), "", None), None);
        assert_eq!(state.invalidate(), None);
        assert!(state.preedit(Some(B), "ni hao", None).is_some());
        assert_eq!(
            state.commit(Some(B), "你好"),
            Some((
                B,
                ImeAction::Commit {
                    text: "你好".into()
                }
            ))
        );
    }

    #[test]
    fn native_delete_allows_the_next_composition() {
        let mut state = ImeBuffer::default();
        state.preedit(Some(A), "old", None);
        assert_eq!(state.terminal_delete(), Some((A, ImeAction::Cancel)));
        assert!(state.preedit(Some(A), "fresh", None).is_some());
        assert_eq!(
            state.commit(Some(A), "fresh"),
            Some((
                A,
                ImeAction::Commit {
                    text: "fresh".into()
                }
            ))
        );
    }

    #[test]
    fn stale_origin_is_cancelled_and_late_commit_dropped() {
        let mut state = ImeBuffer::default();
        state.preedit(Some(A), "字", None);
        assert_eq!(state.invalidate(), Some((A, ImeAction::Cancel)));
        assert_eq!(state.preedit(Some(B), "late", None), None);
        assert_eq!(state.commit(Some(B), "字"), None);
        assert_eq!(
            state.preedit(Some(B), "新", None),
            Some((
                B,
                ImeAction::Preedit {
                    text: "新".into(),
                    selection: 1..1
                }
            ))
        );
        assert_eq!(
            state.commit(Some(B), "新"),
            Some((B, ImeAction::Commit { text: "新".into() }))
        );
    }

    #[test]
    fn empty_preedit_after_cancel_does_not_let_old_commit_migrate() {
        let mut state = ImeBuffer::default();
        state.preedit(Some(A), "secret", None);
        assert_eq!(state.invalidate(), Some((A, ImeAction::Cancel)));
        assert_eq!(state.preedit(Some(B), "", None), None);
        assert_eq!(state.commit(Some(B), "secret"), None);
        assert_eq!(
            state.commit(Some(B), "fresh"),
            Some((
                B,
                ImeAction::Commit {
                    text: "fresh".into()
                }
            ))
        );
    }

    #[test]
    fn empty_boundary_allows_fresh_preedit_without_old_commit() {
        let mut state = ImeBuffer::default();
        state.preedit(Some(A), "old", None);
        state.invalidate();
        assert_eq!(state.preedit(Some(B), "", None), None);
        assert_eq!(
            state.preedit(Some(B), "new", None),
            Some((
                B,
                ImeAction::Preedit {
                    text: "new".into(),
                    selection: 3..3
                }
            ))
        );
        assert_eq!(
            state.commit(Some(B), "new"),
            Some((B, ImeAction::Commit { text: "new".into() }))
        );
    }

    #[test]
    fn unmark_allows_fresh_preedit_on_another_target() {
        let mut state = ImeBuffer::default();
        state.preedit(Some(A), "old", None);
        assert_eq!(
            state.unmark(Some(A)),
            Some((A, ImeAction::Commit { text: "old".into() }))
        );
        assert_eq!(
            state.preedit(Some(B), "new", None),
            Some((
                B,
                ImeAction::Preedit {
                    text: "new".into(),
                    selection: 3..3
                }
            ))
        );
        assert_eq!(
            state.commit(Some(B), "new"),
            Some((B, ImeAction::Commit { text: "new".into() }))
        );
    }

    #[test]
    fn unmark_commits_once_and_allows_fresh_commit_only_input() {
        let mut state = ImeBuffer::default();
        state.preedit(Some(A), "é", None);
        assert_eq!(
            state.unmark(Some(A)),
            Some((A, ImeAction::Commit { text: "é".into() }))
        );
        assert_eq!(state.marked_range(), None);
        assert_eq!(
            state.commit(Some(A), "é"),
            Some((A, ImeAction::Commit { text: "é".into() }))
        );
        assert_eq!(
            state.commit(Some(A), "a"),
            Some((A, ImeAction::Commit { text: "a".into() }))
        );
    }

    #[test]
    fn selection_is_normalized_to_complete_utf16_scalars_and_input_is_bounded() {
        let mut state = ImeBuffer::default();
        state.preedit(Some(A), "a😀b", Some(2..4));
        assert_eq!(state.selected_range(), 1..4);
        assert_eq!(
            state.preedit(Some(A), &"x".repeat(MAX_PASTE_CHARS + 1), None),
            None
        );
        assert_eq!(state.preedit(Some(A), "bad\0", None), None);
        assert_eq!(state.marked_range(), Some(0..4));
    }
}
