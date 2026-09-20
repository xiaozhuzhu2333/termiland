use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub fn encode(event: &KeyEvent) -> Option<Vec<u8>> {
    let mods = event.modifiers;
    match event.code {
        KeyCode::Char(c) => encode_char(c, mods),
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Tab => Some(if mods.contains(KeyModifiers::SHIFT) {
            b"\x1b[Z".to_vec()
        } else {
            vec![b'\t']
        }),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Backspace => Some(if mods.contains(KeyModifiers::CONTROL) {
            vec![0x08]
        } else {
            vec![0x7f]
        }),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => csi_key('A', mods),
        KeyCode::Down => csi_key('B', mods),
        KeyCode::Right => csi_key('C', mods),
        KeyCode::Left => csi_key('D', mods),
        KeyCode::Home => csi_key('H', mods),
        KeyCode::End => csi_key('F', mods),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Insert => Some(b"\x1b[2~".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::F(n) => f_key(n),
        _ => None,
    }
}

fn encode_char(c: char, mods: KeyModifiers) -> Option<Vec<u8>> {
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    if !ctrl && !alt {
        let mut buf = [0u8; 4];
        return Some(c.encode_utf8(&mut buf).as_bytes().to_vec());
    }
    let mut out = Vec::with_capacity(5);
    if alt {
        out.push(0x1b);
    }
    if ctrl {
        out.push(ctrl_byte(c)?);
    } else {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    Some(out)
}

fn ctrl_byte(c: char) -> Option<u8> {
    let lower = c.to_ascii_lowercase();
    match lower {
        'a'..='z' => Some(1 + (lower as u8 - b'a')),
        ' ' | '@' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn csi_key(final_char: char, mods: KeyModifiers) -> Option<Vec<u8>> {
    let m = modifier_code(mods)?;
    let seq = if m == 1 {
        format!("\x1b[{final_char}")
    } else {
        format!("\x1b[1;{m}{final_char}")
    };
    Some(seq.into_bytes())
}

fn modifier_code(mods: KeyModifiers) -> Option<u8> {
    if mods.intersects(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META) {
        return None;
    }
    let mut m = 1u8;
    if mods.contains(KeyModifiers::SHIFT) {
        m += 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        m += 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        m += 4;
    }
    Some(m)
}

fn f_key(n: u8) -> Option<Vec<u8>> {
    let seq = match n {
        1 => "\x1bOP",
        2 => "\x1bOQ",
        3 => "\x1bOR",
        4 => "\x1bOS",
        5 => "\x1b[15~",
        6 => "\x1b[17~",
        7 => "\x1b[18~",
        8 => "\x1b[19~",
        9 => "\x1b[20~",
        10 => "\x1b[21~",
        11 => "\x1b[23~",
        12 => "\x1b[24~",
        _ => return None,
    };
    Some(seq.as_bytes().to_vec())
}

pub fn paste_bytes(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn normalizes_paste_newlines() {
        assert_eq!(paste_bytes("hello"), b"hello".to_vec());
        assert_eq!(paste_bytes("a\r\nb"), b"a\rb".to_vec());
        assert_eq!(paste_bytes("a\nb"), b"a\rb".to_vec());
        assert_eq!(paste_bytes("a\rb"), b"a\rb".to_vec());
    }

    #[test]
    fn encodes_plain_chars() {
        assert_eq!(
            encode(&key(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(b"a".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            Some(b"A".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Char('中'), KeyModifiers::NONE)),
            Some("中".as_bytes().to_vec())
        );
    }

    #[test]
    fn encodes_ctrl_and_alt() {
        assert_eq!(
            encode(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(b"\x03".to_vec())
        );
        assert_eq!(
            encode(&key(
                KeyCode::Char('x'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            Some(b"\x1b\x18".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Char('a'), KeyModifiers::ALT)),
            Some(b"\x1ba".to_vec())
        );
    }

    #[test]
    fn encodes_editing_keys() {
        assert_eq!(
            encode(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(b"\x7f".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Tab, KeyModifiers::NONE)),
            Some(b"\t".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Tab, KeyModifiers::SHIFT)),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(b"\x1b".to_vec())
        );
    }

    #[test]
    fn encodes_navigation_keys() {
        assert_eq!(
            encode(&key(KeyCode::Up, KeyModifiers::NONE)),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Right, KeyModifiers::CONTROL)),
            Some(b"\x1b[1;5C".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Home, KeyModifiers::NONE)),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::Delete, KeyModifiers::NONE)),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            encode(&key(KeyCode::F(5), KeyModifiers::NONE)),
            Some(b"\x1b[15~".to_vec())
        );
    }
}
