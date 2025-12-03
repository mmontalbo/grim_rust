pub(crate) fn normalize_legacy_lua(source: &str) -> String {
    #[derive(Copy, Clone)]
    enum State {
        Normal,
        LineComment,
        BlockComment(usize),
        String(u8),
        LongString(usize),
    }

    let bytes = source.as_bytes();
    let mut result = String::with_capacity(bytes.len());
    let mut i = 0usize;
    let mut state = State::Normal;

    while i < bytes.len() {
        match state {
            State::Normal => {
                let c = bytes[i];
                let remaining = &bytes[i..];
                if c == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
                    result.push_str("--");
                    i += 2;
                    if let Some((eq_count, consumed)) = read_long_start(&bytes[i..]) {
                        result.push_str(&source[i..i + consumed]);
                        i += consumed;
                        state = State::BlockComment(eq_count);
                    } else {
                        state = State::LineComment;
                    }
                    continue;
                }
                if c == b'"' || c == b'\'' {
                    result.push(c as char);
                    i += 1;
                    state = State::String(c);
                    continue;
                }
                if c == b'[' {
                    if let Some((eq_count, consumed)) = read_long_start(remaining) {
                        result.push_str(&source[i..i + consumed]);
                        i += consumed;
                        state = State::LongString(eq_count);
                        continue;
                    }
                }
                if c == b'%' {
                    i += 1;
                    continue;
                }
                if is_ident_start(c) {
                    let start = i;
                    i += 1;
                    while i < bytes.len() && is_ident_part(bytes[i]) {
                        i += 1;
                    }
                    let ident = &source[start..i];
                    if ident == "in" {
                        result.push_str("grim_in");
                    } else {
                        result.push_str(ident);
                    }
                    continue;
                }
                result.push(c as char);
                i += 1;
            }
            State::String(delim) => {
                let c = bytes[i];
                result.push(c as char);
                i += 1;
                if c == b'\\' {
                    if i < bytes.len() {
                        result.push(bytes[i] as char);
                        i += 1;
                    }
                } else if c == delim {
                    state = State::Normal;
                }
            }
            State::LineComment => {
                let c = bytes[i];
                result.push(c as char);
                i += 1;
                if c == b'\n' {
                    state = State::Normal;
                }
            }
            State::BlockComment(eq_count) => {
                if let Some(consumed) = matches_long_end(&bytes[i..], eq_count) {
                    result.push_str(&source[i..i + consumed]);
                    i += consumed;
                    state = State::Normal;
                } else {
                    result.push(bytes[i] as char);
                    i += 1;
                }
            }
            State::LongString(eq_count) => {
                if let Some(consumed) = matches_long_end(&bytes[i..], eq_count) {
                    result.push_str(&source[i..i + consumed]);
                    i += consumed;
                    state = State::Normal;
                } else {
                    result.push(bytes[i] as char);
                    i += 1;
                }
            }
        }
    }

    result
}

fn read_long_start(bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.len() < 2 || bytes[0] != b'[' {
        return None;
    }
    let mut idx = 1usize;
    let mut eq_count = 0usize;
    while idx < bytes.len() && bytes[idx] == b'=' {
        eq_count += 1;
        idx += 1;
    }
    if idx < bytes.len() && bytes[idx] == b'[' {
        Some((eq_count, idx + 1))
    } else {
        None
    }
}

fn matches_long_end(bytes: &[u8], eq_count: usize) -> Option<usize> {
    if bytes.is_empty() || bytes[0] != b']' {
        return None;
    }
    let mut idx = 1usize;
    for _ in 0..eq_count {
        if idx >= bytes.len() || bytes[idx] != b'=' {
            return None;
        }
        idx += 1;
    }
    if idx < bytes.len() && bytes[idx] == b']' {
        Some(idx + 1)
    } else {
        None
    }
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || (c as char).is_ascii_alphabetic()
}

fn is_ident_part(c: u8) -> bool {
    is_ident_start(c) || (c as char).is_ascii_digit()
}
