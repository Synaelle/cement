// Small helpers shared by the server: path resolution, number parsing, formatting.

pub const PATH_SIZE: usize = 1024;

/// Resolves `arg` against `cwd` into an absolute, normalized SD card path ("/", "/a/b").
/// ".." can't climb above "/", and only the SD card archive is ever opened, so
/// nothing outside the SD card is reachable. None for a path that can't exist
/// on the SD card (a backslash, invalid UTF-8, too long).
pub fn resolve_path(cwd: &str, arg: &[u8]) -> Option<String> {
    let arg = std::str::from_utf8(arg).ok()?;
    if arg.contains('\\') {
        return None; // not a valid name character on the SD card
    }
    let joined = if arg.starts_with('/') { arg.to_string() } else { format!("{cwd}/{arg}") };
    let mut out = String::new();
    for tok in joined.split('/') {
        if tok.is_empty() || tok == "." {
            continue;
        }
        if tok == ".." {
            if let Some(slash) = out.rfind('/') {
                out.truncate(slash);
            }
            continue;
        }
        if out.len() + 1 + tok.len() >= PATH_SIZE {
            return None;
        }
        out.push('/');
        out.push_str(tok);
    }
    if out.is_empty() {
        out.push('/');
    }
    Some(out)
}

/// strtoull: leading spaces, digits, anything after ignored; 0 when there are none
pub fn parse_ull(s: &[u8]) -> u64 {
    let mut v: u64 = 0;
    let mut it = s.iter().skip_while(|c| c.is_ascii_whitespace()).peekable();
    if it.peek() == Some(&&b'+') {
        it.next();
    }
    for &c in it {
        if !c.is_ascii_digit() {
            break;
        }
        v = v.wrapping_mul(10).wrapping_add((c - b'0') as u64);
    }
    v
}

/// "12.3 MB" / "4.5 KB", as on the 3DS screens
pub fn format_size(v: u64) -> String {
    if v >= 1024 * 1024 {
        format!("{:.1} MB", v as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} KB", v as f64 / 1024.0)
    }
}

pub fn base_name(path: &str) -> &str {
    path.rfind('/').map_or(path, |i| &path[i + 1..])
}

/// first `n` characters of `s` (C's "%.ns")
pub fn clip(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

/// MDTM text "YYYYMMDDhhmmss" from milliseconds since 2000-01-01 (the 3DS clock keeps local time)
pub fn mdtm_text(ms2000: u64) -> String {
    let secs = ms2000 / 1000 + 946_684_800; // seconds since 1970
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // civil date from days since 1970 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}",
        year,
        month,
        day,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn ip_text(ip: u32) -> String {
    let a = ip.to_ne_bytes(); // s_addr: network order in memory
    format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

pub fn is_cia(path: &str) -> bool {
    path.len() > 4 && path.is_char_boundary(path.len() - 4) && path[path.len() - 4..].eq_ignore_ascii_case(".cia")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(resolve_path("/", b"").as_deref(), Some("/"));
        assert_eq!(resolve_path("/a", b"b/../c").as_deref(), Some("/a/c"));
        assert_eq!(resolve_path("/a", b"../../..").as_deref(), Some("/"));
        assert_eq!(resolve_path("/", b"/x//./y/").as_deref(), Some("/x/y"));
        assert_eq!(resolve_path("/", b"..\\x"), None);
    }

    #[test]
    fn dates() {
        assert_eq!(mdtm_text(0), "20000101000000");
        // 2026-10-18 12:34:56
        assert_eq!(mdtm_text((1_792_326_896 - 946_684_800) * 1000), "20261018123456");
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_ull(b" 123abc"), 123);
        assert_eq!(parse_ull(b"x"), 0);
    }
}
