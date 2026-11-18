use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

/// Bytes for the PTY, or a local scrollback action.
#[derive(Debug)]
pub enum Input {
    Bytes(Vec<u8>),
    Scroll(isize),
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    GotoTab(usize),
    Copy,
    Paste,
}

pub fn encode(event: &KeyEvent, mods: ModifiersState, app_cursor: bool, rows: u16) -> Option<Input> {
    let plain = event.key_without_modifiers();
    encode_parts(
        &event.logical_key,
        event.text.as_deref(),
        event.text_with_all_modifiers(),
        &plain,
        event.state,
        mods,
        app_cursor,
        rows,
    )
}

pub fn encode_parts(
    logical: &Key,
    text: Option<&str>,
    text_with_modifiers: Option<&str>,
    plain: &Key,
    state: ElementState,
    mods: ModifiersState,
    app_cursor: bool,
    rows: u16,
) -> Option<Input> {
    if state == ElementState::Released || is_modifier(logical) {
        return None;
    }
    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let shift = mods.shift_key();

    if shift && !ctrl {
        let page = ((rows.max(1) / 2) as isize).max(1);
        match logical {
            Key::Named(NamedKey::PageUp) => return Some(Input::Scroll(page)),
            Key::Named(NamedKey::PageDown) => return Some(Input::Scroll(-page)),
            _ => {}
        }
    }

    if ctrl && !alt {
        if shift {
            if let Some(ch) = key_char(plain).or_else(|| key_char(logical)) {
                match ch.to_ascii_lowercase() {
                    'c' => return Some(Input::Copy),
                    'v' => return Some(Input::Paste),
                    _ => {}
                }
            }
        }
        if let Some(action) = tab_action(logical, plain, shift) {
            return Some(action);
        }
    }

    if let Some(bytes) = function_key(logical, ctrl, alt, shift, app_cursor) {
        return Some(Input::Bytes(bytes));
    }

    if ctrl {
        if let Some(bytes) = text_with_modifiers.and_then(control_text) {
            return Some(Input::Bytes(prefix_alt(bytes, alt)));
        }
        if let Some(byte) = key_char(plain).or_else(|| key_char(logical)).and_then(ctrl_byte) {
            return Some(Input::Bytes(prefix_alt(vec![byte], alt)));
        }
    }

    let produced = text
        .filter(|value| !value.is_empty())
        .or_else(|| logical.to_text())
        .or_else(|| plain.to_text());
    let bytes = produced?.as_bytes().to_vec();
    if bytes.is_empty() {
        return None;
    }
    Some(Input::Bytes(prefix_alt(bytes, alt)))
}

fn tab_action(logical: &Key, plain: &Key, shift: bool) -> Option<Input> {
    match logical {
        Key::Named(NamedKey::Tab) => return Some(if shift { Input::PrevTab } else { Input::NextTab }),
        Key::Named(NamedKey::PageUp) => return Some(Input::PrevTab),
        Key::Named(NamedKey::PageDown) => return Some(Input::NextTab),
        Key::Named(NamedKey::ArrowRight) if shift => return Some(Input::NextTab),
        Key::Named(NamedKey::ArrowLeft) if shift => return Some(Input::PrevTab),
        _ => {}
    }
    if !shift {
        return None;
    }
    let ch = key_char(plain)
        .or_else(|| key_char(logical))?
        .to_ascii_lowercase();
    match ch {
        't' => Some(Input::NewTab),
        'w' | 'q' => Some(Input::CloseTab),
        '1'..='9' => Some(Input::GotoTab((ch as u8 - b'1') as usize)),
        _ => None,
    }
}

fn is_modifier(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::Super
                | NamedKey::AltGraph
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::Symbol
        )
    )
}

fn prefix_alt(mut bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn control_text(text: &str) -> Option<Vec<u8>> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let byte = u32::from(ch);
    if byte < 32 || byte == 127 {
        Some(vec![byte as u8])
    } else {
        None
    }
}

fn key_char(key: &Key) -> Option<char> {
    match key {
        Key::Character(value) => value.chars().next(),
        Key::Named(named) => named.to_text().and_then(|text| text.chars().next()),
        _ => None,
    }
}

fn ctrl_byte(ch: char) -> Option<u8> {
    match ch {
        'a'..='z' => Some(ch as u8 - b'a' + 1),
        'A'..='Z' => Some(ch as u8 - b'A' + 1),
        ' ' | '@' | '2' => Some(0x00),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' => Some(0x1f),
        '/' | '?' | '8' => Some(0x7f),
        _ => None,
    }
}

fn function_key(logical: &Key, ctrl: bool, alt: bool, shift: bool, app_cursor: bool) -> Option<Vec<u8>> {
    let named = match logical {
        Key::Named(named) => named,
        _ => return None,
    };
    let param = modifier_param(ctrl, alt, shift);
    let bytes = match named {
        // Ctrl+Enter and Shift+Enter insert a newline without submitting the prompt. Enter alone stays CR.
        NamedKey::Enter if ctrl || shift => b"\x1b\r".to_vec(),
        NamedKey::Enter => b"\r".to_vec(),
        NamedKey::Tab if shift && !ctrl => b"\x1b[Z".to_vec(),
        NamedKey::Tab => b"\t".to_vec(),
        NamedKey::Backspace if ctrl => b"\x08".to_vec(),
        NamedKey::Backspace => b"\x7f".to_vec(),
        NamedKey::Escape => b"\x1b".to_vec(),
        NamedKey::ArrowUp => arrow(b'A', param, app_cursor),
        NamedKey::ArrowDown => arrow(b'B', param, app_cursor),
        NamedKey::ArrowRight => arrow(b'C', param, app_cursor),
        NamedKey::ArrowLeft => arrow(b'D', param, app_cursor),
        NamedKey::Home => home_end(b'H', param, app_cursor),
        NamedKey::End => home_end(b'F', param, app_cursor),
        NamedKey::Delete => tilde(3, param),
        NamedKey::PageUp => tilde(5, param),
        NamedKey::PageDown => tilde(6, param),
        NamedKey::Insert => tilde(2, param),
        NamedKey::F1 => ss3_or_tilde(b'P', 11, param),
        NamedKey::F2 => ss3_or_tilde(b'Q', 12, param),
        NamedKey::F3 => ss3_or_tilde(b'R', 13, param),
        NamedKey::F4 => ss3_or_tilde(b'S', 14, param),
        NamedKey::F5 => tilde(15, param),
        NamedKey::F6 => tilde(17, param),
        NamedKey::F7 => tilde(18, param),
        NamedKey::F8 => tilde(19, param),
        NamedKey::F9 => tilde(20, param),
        NamedKey::F10 => tilde(21, param),
        NamedKey::F11 => tilde(23, param),
        NamedKey::F12 => tilde(24, param),
        _ => return None,
    };
    let enter_already_escaped = *named == NamedKey::Enter && (ctrl || shift);
    Some(if alt && !enter_already_escaped && (*named == NamedKey::Enter || *named == NamedKey::Backspace || *named == NamedKey::Tab) {
        prefix_alt(bytes, true)
    } else {
        bytes
    })
}

fn modifier_param(ctrl: bool, alt: bool, shift: bool) -> u8 {
    let mut param = 1u8;
    if shift {
        param += 1;
    }
    if alt {
        param += 2;
    }
    if ctrl {
        param += 4;
    }
    param
}

fn arrow(letter: u8, param: u8, app_cursor: bool) -> Vec<u8> {
    if param == 1 && app_cursor {
        vec![0x1b, b'O', letter]
    } else if param == 1 {
        vec![0x1b, b'[', letter]
    } else {
        format!("\x1b[1;{param}{letter}", letter = letter as char).into_bytes()
    }
}

fn home_end(letter: u8, param: u8, app_cursor: bool) -> Vec<u8> {
    if param == 1 && app_cursor {
        vec![0x1b, b'O', letter]
    } else if param == 1 {
        vec![0x1b, b'[', letter]
    } else {
        format!("\x1b[1;{param}{letter}", letter = letter as char).into_bytes()
    }
}

fn tilde(code: u16, param: u8) -> Vec<u8> {
    if param == 1 {
        format!("\x1b[{code}~").into_bytes()
    } else {
        format!("\x1b[{code};{param}~").into_bytes()
    }
}

fn ss3_or_tilde(letter: u8, code: u16, param: u8) -> Vec<u8> {
    if param == 1 {
        vec![0x1b, b'O', letter]
    } else {
        tilde(code, param)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, NamedKey};

    fn press(
        logical: Key,
        text: Option<&str>,
        modified: Option<&str>,
        mods: ModifiersState,
    ) -> Option<Input> {
        let plain = match &logical {
            Key::Character(value) => Key::Character(value.to_lowercase().into()),
            other => other.clone(),
        };
        encode_parts(&logical, text, modified, &plain, ElementState::Pressed, mods, false, 40)
    }

    #[test]
    fn ctrl_shift_c_copies_and_v_pastes() {
        let copy = Key::Character("c".into());
        let paste = Key::Character("v".into());
        let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert!(matches!(press(copy, Some("\u{3}"), Some("\u{3}"), mods), Some(Input::Copy)));
        assert!(matches!(press(paste, None, None, mods), Some(Input::Paste)));
    }

    #[test]
    fn ctrl_c_is_etx() {
        let key = Key::Character("c".into());
        match press(key, Some("c"), Some("\u{3}"), ModifiersState::CONTROL) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, vec![0x03]),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn ctrl_c_without_platform_text() {
        let key = Key::Character("c".into());
        match press(key, None, None, ModifiersState::CONTROL) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, vec![0x03]),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn space_is_a_space_even_as_named_key() {
        let key = Key::Named(NamedKey::Space);
        match press(key.clone(), None, None, ModifiersState::empty()) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, b" "),
            other => panic!("inesperado: {other:?}"),
        }
        match press(key, Some(" "), None, ModifiersState::empty()) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, b" "),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn enter_is_carriage_return() {
        let key = Key::Named(NamedKey::Enter);
        match press(key, Some("\r"), None, ModifiersState::empty()) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, b"\r"),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn ctrl_or_shift_enter_inserts_a_newline() {
        let key = Key::Named(NamedKey::Enter);
        for mods in [ModifiersState::CONTROL, ModifiersState::SHIFT, ModifiersState::CONTROL | ModifiersState::SHIFT] {
            match press(key.clone(), Some("\r"), None, mods) {
                Some(Input::Bytes(bytes)) => assert_eq!(bytes, b"\x1b\r"),
                other => panic!("inesperado: {other:?}"),
            }
        }
    }

    #[test]
    fn letter_uses_text_or_the_key() {
        let key = Key::Character("á".into());
        match press(key, Some("á"), None, ModifiersState::empty()) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, "á".as_bytes()),
            other => panic!("inesperado: {other:?}"),
        }
        let key = Key::Character("a".into());
        match press(key, None, None, ModifiersState::empty()) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, b"a"),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn alt_letter_is_meta() {
        let key = Key::Character("b".into());
        match press(key, Some("b"), None, ModifiersState::ALT) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, b"\x1bb"),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn ctrl_arrow_uses_xterm_modifier() {
        let key = Key::Named(NamedKey::ArrowLeft);
        match press(key, None, None, ModifiersState::CONTROL) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, b"\x1b[1;5D"),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn shift_page_up_scrolls_locally() {
        let key = Key::Named(NamedKey::PageUp);
        match press(key, None, None, ModifiersState::SHIFT) {
            Some(Input::Scroll(n)) => assert_eq!(n, 20),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn ctrl_shift_t_opens_a_tab() {
        let key = Key::Character("T".into());
        let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert!(matches!(press(key, Some("T"), None, mods), Some(Input::NewTab)));
    }

    #[test]
    fn ctrl_shift_q_closes_a_tab() {
        let key = Key::Character("Q".into());
        let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert!(matches!(press(key, Some("Q"), None, mods), Some(Input::CloseTab)));
        match press(Key::Character("q".into()), Some("\u{11}"), Some("\u{11}"), ModifiersState::CONTROL) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, vec![0x11]),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn ctrl_page_switches_tabs() {
        let key = Key::Named(NamedKey::PageDown);
        assert!(matches!(
            press(key, None, None, ModifiersState::CONTROL),
            Some(Input::NextTab)
        ));
    }

    #[test]
    fn ctrl_tab_switches_tabs_and_shift_goes_back() {
        let key = Key::Named(NamedKey::Tab);
        assert!(matches!(
            press(key.clone(), None, None, ModifiersState::CONTROL),
            Some(Input::NextTab)
        ));
        assert!(matches!(
            press(key.clone(), None, None, ModifiersState::CONTROL | ModifiersState::SHIFT),
            Some(Input::PrevTab)
        ));
        match press(key.clone(), Some("\t"), None, ModifiersState::empty()) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, b"\t"),
            other => panic!("inesperado: {other:?}"),
        }
        match press(key, None, None, ModifiersState::SHIFT) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, b"\x1b[Z"),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn ctrl_shift_digit_jumps_to_a_tab() {
        match encode_parts(
            &Key::Character("!".into()),
            Some("!"),
            None,
            &Key::Character("1".into()),
            ElementState::Pressed,
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            false,
            24,
        ) {
            Some(Input::GotoTab(index)) => assert_eq!(index, 0),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn app_cursor_uses_ss3() {
        let key = Key::Named(NamedKey::ArrowUp);
        match encode_parts(
            &key,
            None,
            None,
            &key,
            ElementState::Pressed,
            ModifiersState::empty(),
            true,
            24,
        ) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, b"\x1bOA"),
            other => panic!("inesperado: {other:?}"),
        }
    }
}
