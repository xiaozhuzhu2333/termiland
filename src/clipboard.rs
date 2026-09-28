#[cfg(not(windows))]
const BASE64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[cfg(not(windows))]
const OSC52_LIMIT: usize = 100_000;

#[cfg(not(windows))]
fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64_TABLE[(n >> 18) as usize & 63] as char);
        out.push(BASE64_TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            BASE64_TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(not(windows))]
fn osc52_sequence(text: &str) -> Option<String> {
    let encoded = base64_encode(text.as_bytes());
    (encoded.len() <= OSC52_LIMIT).then(|| format!("\x1b]52;c;{encoded}\x07"))
}

pub fn copy(text: &str) -> bool {
    #[cfg(windows)]
    {
        arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(text.to_owned()))
            .is_ok()
    }
    #[cfg(not(windows))]
    {
        use std::io::Write;
        let Some(sequence) = osc52_sequence(text) else {
            return false;
        };
        let mut stdout = std::io::stdout();
        stdout.write_all(sequence.as_bytes()).is_ok() && stdout.flush().is_ok()
    }
}

#[cfg(windows)]
pub fn paste() -> Option<String> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.get_text())
        .ok()
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode("你好".as_bytes()), "5L2g5aW9");
    }

    #[test]
    fn osc52_sequence_wraps_and_caps() {
        let sequence = osc52_sequence("hello").unwrap();
        assert!(sequence.starts_with("\x1b]52;c;aGVsbG8="));
        assert!(sequence.ends_with('\x07'));
        assert!(
            osc52_sequence(&"x".repeat(200_000)).is_none(),
            "超限内容应拒绝"
        );
    }
}
