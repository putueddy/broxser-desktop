use super::{CaretRect, ImeAction, MAX_PASTE_CHARS};
use serde_json::Value;

pub(super) const IME_WORLD: &str = "broxser_ime_observer";
pub(super) const IME_BINDING: &str = "__broxserImeCaret";

/// Reports only an identity and bounded viewport geometry. Text stays in the renderer.
/// The isolated world observes the main document only; inaccessible frames and
/// closed shadow roots are deliberately outside this contract.
pub(super) const IME_OBSERVER: &str = r#"(() => {
  let anchor = 0, priorElement = null, priorNode = null, priorOffset = -1,
      pending = false, composing = false;
  const editable = el => {
    if (!(el instanceof HTMLElement) || !el.isConnected) return false;
    if (el instanceof HTMLTextAreaElement) return !el.disabled && !el.readOnly;
    if (el instanceof HTMLInputElement) return !el.disabled && !el.readOnly &&
      ['text','search','url','tel','email'].includes(el.type);
    return el.isContentEditable;
  };
  const caret = (el, offset) => {
    if (!(el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement)) {
      const s = getSelection();
      if (!s || !s.rangeCount || !el.contains(s.anchorNode)) return null;
      const r = document.createRange();
      r.setStart(s.focusNode, s.focusOffset); r.collapse(true);
      const rect = r.getClientRects()[0] || r.getBoundingClientRect();
      if (rect && rect.height > 0) return {x:rect.x,y:rect.y,width:rect.width,height:rect.height};
      // Chromium gives an empty editable a zero-sized collapsed Range.
      if (el.textContent) return null;
      const cs = getComputedStyle(el), box = el.getBoundingClientRect();
      const px = n => Number.parseFloat(n) || 0;
      return {x:box.left+px(cs.borderLeftWidth)+px(cs.paddingLeft),
        y:box.top+px(cs.borderTopWidth)+px(cs.paddingTop),width:1,
        height:px(cs.lineHeight) || px(cs.fontSize)*1.2};
    }
    if (el.value.length > 65536) return null;
    // A text mirror preserves wrapping and the real UTF-16 selection offset.
    const cs = getComputedStyle(el), box = el.getBoundingClientRect();
    const px = n => Number.parseFloat(n) || 0;
    const mirrorWidth = el.clientWidth + px(cs.borderLeftWidth) + px(cs.borderRightWidth);
    const host = document.createElement('div');
    Object.assign(host.style, {position:'fixed',left:'0',top:'0',width:'0',height:'0',
      visibility:'hidden',pointerEvents:'none'});
    // The value lives only in a closed shadow root while measured. A page
    // MutationObserver can see the empty host, never password or form text.
    const shadow = host.attachShadow({mode:'closed'});
    const mirror = document.createElement('div'), marker = document.createElement('span');
    const props = ['font','fontFamily','fontSize','fontWeight','fontStyle','letterSpacing',
      'lineHeight','textTransform','textIndent','textAlign','direction','wordSpacing',
      'padding','border','boxSizing','tabSize','overflowWrap','wordBreak'];
    for (const p of props) mirror.style[p] = cs[p];
    Object.assign(mirror.style, {position:'fixed',visibility:'hidden',pointerEvents:'none',
      left:box.left+'px',top:box.top+'px',width:mirrorWidth+'px',height:el.offsetHeight+'px',
      boxSizing:'border-box',
      whiteSpace:el instanceof HTMLTextAreaElement?'pre-wrap':'pre',overflow:'hidden'});
    mirror.textContent = el.value.slice(0, offset);
    marker.textContent = '\u200b'; mirror.append(marker,document.createTextNode(el.value.slice(offset)));
    shadow.append(mirror);
    document.documentElement.append(host);
    const m = marker.getBoundingClientRect();
    host.remove();
    const vertical = el instanceof HTMLInputElement ? Math.max(0,
      (el.clientHeight-px(cs.paddingTop)-px(cs.paddingBottom)-m.height)/2) : 0;
    return {x:m.x-el.scrollLeft,y:m.y-el.scrollTop+vertical,width:1,height:m.height};
  };
  const read = () => {
    const el = document.activeElement;
    if (!editable(el)) return null;
    let offset, node = null;
    if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
      offset = el.selectionStart;
      if (!Number.isInteger(offset)) return null;
    } else {
      const s = getSelection();
      if (!s || !s.anchorNode || !el.contains(s.anchorNode)) {
        return null;
      }
      offset = s.anchorOffset; node = s.anchorNode;
    }
    return {el, offset, node};
  };
  const sample = () => {
    const current = read();
    if (!current) {
      priorElement = null; priorNode = null; priorOffset = -1;
      return null;
    }
    const {el, offset, node} = current;
    if (priorElement !== el || (!composing && (priorNode !== node || priorOffset !== offset))) {
      anchor++; priorElement = el; priorNode = node; priorOffset = offset;
    }
    return {el, offset};
  };
  globalThis.__broxserImeCurrent = () => sample() ? anchor : 0;
  const report = () => {
    pending = false;
    const current = sample();
    if (!current) { __broxserImeCaret('{"active":false}'); return; }
    const {el, offset} = current;
    const rect = caret(el, offset);
    if (!rect || ![rect.x,rect.y,rect.width,rect.height].every(Number.isFinite)) {
      __broxserImeCaret('{"active":false}'); return;
    }
    __broxserImeCaret(JSON.stringify({active:true,anchor,x:rect.x,y:rect.y,
      width:rect.width,height:rect.height}));
  };
  const schedule = () => { if (!pending) { pending=true; requestAnimationFrame(report); } };
  const focusChanged = event => {
    if (!event.isTrusted) return;
    // focusout and focusin can both occur in one page script task. Advance
    // before the next animation frame so an immediate CDP identity read rejects
    // the previous target even if focus returns to the same element.
    anchor++; priorElement=null; priorNode=null; priorOffset=-1; composing=false;
    schedule();
  };
  globalThis.__broxserImeRefresh = schedule;
  addEventListener('focusin',focusChanged,true);
  addEventListener('focusout',focusChanged,true);
  addEventListener('selectionchange',schedule,true);
  addEventListener('input',event => {
    // Browser-inserted text moves the caret itself. Keep the current target
    // across repeated commit-only callbacks that have no intervening key-up.
    const current = read();
    if (event.isTrusted && current && event.target === current.el) {
      if (priorElement !== current.el) anchor++;
      priorElement = current.el; priorNode = current.node; priorOffset = current.offset;
    }
    schedule();
  },true);
  addEventListener('scroll',schedule,true);
  addEventListener('resize',schedule,true);
  addEventListener('pointerdown',event => {
    if (event.isTrusted) { priorElement=null; priorNode=null; priorOffset=-1; }
  },true);
  addEventListener('pointerup',schedule,true);
  addEventListener('compositionstart',event => {
    if (event.isTrusted && event.target === document.activeElement && editable(event.target))
      composing=true;
    schedule();
  },true);
  addEventListener('compositionend',event => {
    if (event.isTrusted && event.target === document.activeElement && editable(event.target))
      composing=false;
    schedule();
  },true);
  schedule();
})();"#;

pub(super) fn parse_caret_report(
    payload: &str,
    viewport: (f64, f64),
) -> Option<Option<(u64, CaretRect)>> {
    if payload.len() > 256 {
        return None;
    }
    let value: Value = serde_json::from_str(payload).ok()?;
    match value.get("active")?.as_bool()? {
        false => Some(None),
        true => {
            let anchor = value.get("anchor")?.as_u64().filter(|id| *id > 0)?;
            let number = |name| {
                value
                    .get(name)
                    .and_then(Value::as_f64)
                    .filter(|v| v.is_finite())
            };
            let (x, y, width, height) = (
                number("x")?,
                number("y")?,
                number("width")?,
                number("height")?,
            );
            if !viewport.0.is_finite()
                || !viewport.1.is_finite()
                || viewport.0 <= 0.0
                || viewport.1 <= 0.0
                || width < 0.0
                || height <= 0.0
                || width > viewport.0 * 2.0
                || height > viewport.1 * 2.0
                || x + width < 0.0
                || y + height < 0.0
                || x > viewport.0
                || y > viewport.1
            {
                return None;
            }
            let x = x.clamp(0.0, viewport.0);
            let y = y.clamp(0.0, viewport.1);
            Some(Some((
                anchor,
                CaretRect {
                    x,
                    y,
                    width: width.min(viewport.0 - x),
                    height: height.min(viewport.1 - y),
                },
            )))
        }
    }
}

pub(super) fn valid_ime_action(action: &ImeAction) -> bool {
    let (text, range) = match action {
        ImeAction::Preedit { text, selection } => (text, Some(selection)),
        ImeAction::Commit { text } => (text, None),
        ImeAction::Cancel => return true,
    };
    if text.chars().count() > MAX_PASTE_CHARS
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
    {
        return false;
    }
    let len = text.encode_utf16().count();
    range.is_none_or(|range| {
        if range.start > range.end || range.end > len {
            return false;
        }
        let mut boundary = 0;
        let mut start_ok = range.start == 0;
        let mut end_ok = range.end == 0;
        for ch in text.chars() {
            boundary += ch.len_utf16();
            start_ok |= range.start == boundary;
            end_ok |= range.end == boundary;
        }
        start_ok && end_ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn untrusted_reports_are_bounded_and_clamped() {
        let viewport = (100.0, 80.0);
        assert_eq!(
            parse_caret_report("{\"active\":false}", viewport),
            Some(None)
        );
        for bad in [
            "{}",
            "{\"active\":true,\"anchor\":0,\"x\":1,\"y\":2,\"width\":1,\"height\":10}",
            "{\"active\":true,\"anchor\":1,\"x\":1e400,\"y\":2,\"width\":1,\"height\":10}",
            "{\"active\":true,\"anchor\":1,\"x\":999,\"y\":2,\"width\":1,\"height\":10}",
        ] {
            assert_eq!(parse_caret_report(bad, viewport), None);
        }
        let reported = parse_caret_report(
            "{\"active\":true,\"anchor\":2,\"x\":-2,\"y\":75,\"width\":5,\"height\":10}",
            viewport,
        )
        .unwrap()
        .unwrap();
        assert_eq!(reported.0, 2);
        assert_eq!(
            reported.1,
            CaretRect {
                x: 0.0,
                y: 75.0,
                width: 5.0,
                height: 5.0
            }
        );
    }
    #[test]
    fn utf16_selection_and_controls_are_checked() {
        assert!(valid_ime_action(&ImeAction::Preedit {
            text: "😀".into(),
            selection: 0..2
        }));
        assert!(!valid_ime_action(&ImeAction::Preedit {
            text: "😀".into(),
            selection: 0..3
        }));
        assert!(!valid_ime_action(&ImeAction::Preedit {
            text: "😀".into(),
            selection: 0..1
        }));
        assert!(!valid_ime_action(&ImeAction::Preedit {
            text: "😀".into(),
            selection: 1..2
        }));
        assert!(!valid_ime_action(&ImeAction::Preedit {
            text: "x".into(),
            selection: std::ops::Range { start: 1, end: 0 }
        }));
        assert!(!valid_ime_action(&ImeAction::Commit { text: "x\0".into() }));
        assert!(!valid_ime_action(&ImeAction::Commit {
            text: "x".repeat(MAX_PASTE_CHARS + 1)
        }));
    }
}
