use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Bytes que vão para o PTY, ou uma ação local de scrollback.
#[derive(Debug)]
pub enum Input {
    Bytes(Vec<u8>),
    Scroll(isize),
}

pub fn encode(event: &KeyEvent, mods: ModifiersState, app_cursor: bool, rows: u16) -> Option<Input> {
    encode_parts(
        &event.logical_key,
        event.text.as_deref(),
        event.state,
        mods,
        app_cursor,
        rows,
    )
}

pub fn encode_parts(
    logical: &Key,
    text: Option<&str>,
    state: ElementState,
    mods: ModifiersState,
    app_cursor: bool,
    rows: u16,
) -> Option<Input> {
    if state == ElementState::Released {
        return None;
    }
    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let shift = mods.shift_key();

    if let Key::Named(named) = logical {
        let page = ((rows.max(1) / 2) as isize).max(1);
        let seq = match named {
            NamedKey::PageUp if shift => return Some(Input::Scroll(page)),
            NamedKey::PageDown if shift => return Some(Input::Scroll(-page)),
            NamedKey::Enter => b"\r".to_vec(),
            NamedKey::Tab if shift => b"\x1b[Z".to_vec(),
            NamedKey::Tab => b"\t".to_vec(),
            NamedKey::Backspace => {
                if ctrl {
                    b"\x08".to_vec()
                } else {
                    b"\x7f".to_vec()
                }
            }
            NamedKey::Delete => b"\x1b[3~".to_vec(),
            NamedKey::Escape => b"\x1b".to_vec(),
            NamedKey::ArrowUp => arrow(b'A', app_cursor),
            NamedKey::ArrowDown => arrow(b'B', app_cursor),
            NamedKey::ArrowRight => arrow(b'C', app_cursor),
            NamedKey::ArrowLeft => arrow(b'D', app_cursor),
            NamedKey::Home => b"\x1b[H".to_vec(),
            NamedKey::End => b"\x1b[F".to_vec(),
            NamedKey::PageUp => b"\x1b[5~".to_vec(),
            NamedKey::PageDown => b"\x1b[6~".to_vec(),
            NamedKey::Insert => b"\x1b[2~".to_vec(),
            NamedKey::F1 => b"\x1bOP".to_vec(),
            NamedKey::F2 => b"\x1bOQ".to_vec(),
            NamedKey::F3 => b"\x1bOR".to_vec(),
            NamedKey::F4 => b"\x1bOS".to_vec(),
            NamedKey::F5 => b"\x1b[15~".to_vec(),
            NamedKey::F6 => b"\x1b[17~".to_vec(),
            NamedKey::F7 => b"\x1b[18~".to_vec(),
            NamedKey::F8 => b"\x1b[19~".to_vec(),
            NamedKey::F9 => b"\x1b[20~".to_vec(),
            NamedKey::F10 => b"\x1b[21~".to_vec(),
            NamedKey::F11 => b"\x1b[23~".to_vec(),
            NamedKey::F12 => b"\x1b[24~".to_vec(),
            _ => return None,
        };
        return Some(Input::Bytes(seq));
    }

    if ctrl {
        if let Key::Character(value) = logical {
            let ch = value.chars().next()?.to_ascii_lowercase();
            if ch.is_ascii_lowercase() {
                return Some(Input::Bytes(vec![ch as u8 - b'a' + 1]));
            }
        }
    }

    if let Some(text) = text {
        if text.is_empty() || ctrl {
            return None;
        }
        let mut bytes = text.as_bytes().to_vec();
        if alt {
            let mut prefixed = vec![0x1b];
            prefixed.append(&mut bytes);
            bytes = prefixed;
        }
        return Some(Input::Bytes(bytes));
    }
    None
}

fn arrow(letter: u8, app_cursor: bool) -> Vec<u8> {
    if app_cursor {
        vec![0x1b, b'O', letter]
    } else {
        vec![0x1b, b'[', letter]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, NamedKey};

    #[test]
    fn ctrl_c_is_etx() {
        let key = Key::Character("c".into());
        match encode_parts(&key, Some("c"), ElementState::Pressed, ModifiersState::CONTROL, false, 40)
        {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, vec![0x03]),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn shift_page_up_scrolls_locally() {
        let key = Key::Named(NamedKey::PageUp);
        match encode_parts(&key, None, ElementState::Pressed, ModifiersState::SHIFT, false, 40) {
            Some(Input::Scroll(n)) => assert_eq!(n, 20),
            other => panic!("inesperado: {other:?}"),
        }
    }

    #[test]
    fn app_cursor_uses_ss3() {
        let key = Key::Named(NamedKey::ArrowUp);
        match encode_parts(&key, None, ElementState::Pressed, ModifiersState::empty(), true, 24) {
            Some(Input::Bytes(bytes)) => assert_eq!(bytes, b"\x1bOA"),
            other => panic!("inesperado: {other:?}"),
        }
    }
}
