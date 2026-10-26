//! `shutil.get_terminal_size().columns`, which argparse sizes its help with.

use crate::pytext;

/// `int(text)` for a decimal string: Python whitespace around it, an optional
/// sign, Unicode decimal digits and single underscores between digits.
/// Values beyond i64 saturate; only their sign matters to the caller.
pub fn py_int(text: &str) -> Option<i64> {
    let body = pytext::strip(text);
    let (negative, digits) = match body.chars().next()? {
        '-' => (true, &body[1..]),
        '+' => (false, &body[1..]),
        _ => (false, body),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut value: i64 = 0;
    for ch in digits.chars().filter(|&c| c != '_') {
        let digit = pytext::decimal_value(ch as u32)?;
        value = value.saturating_mul(10).saturating_add(i64::from(digit));
    }
    Some(if negative { -value } else { value })
}

/// The width of the terminal on stdout, as `os.get_terminal_size(1)` reports it.
#[cfg(unix)]
fn stdout_terminal() -> Option<(i64, i64)> {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes one winsize into the struct passed by pointer.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let ok = unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } == 0;
    ok.then(|| (i64::from(size.ws_col), i64::from(size.ws_row)))
}

#[cfg(windows)]
fn stdout_terminal() -> Option<(i64, i64)> {
    #[repr(C)]
    #[derive(Default)]
    struct Coord {
        x: i16,
        y: i16,
    }
    #[repr(C)]
    #[derive(Default)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ScreenBufferInfo {
        size: Coord,
        cursor: Coord,
        attributes: u16,
        window: SmallRect,
        maximum: Coord,
    }
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut core::ffi::c_void;
        fn GetConsoleScreenBufferInfo(
            console: *mut core::ffi::c_void,
            info: *mut ScreenBufferInfo,
        ) -> i32;
    }
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    let mut info = ScreenBufferInfo::default();
    // SAFETY: GetStdHandle takes a constant; the info struct matches CONSOLE_SCREEN_BUFFER_INFO.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage
    let ok = unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        !handle.is_null() && GetConsoleScreenBufferInfo(handle, &mut info) != 0
    };
    let window = &info.window;
    ok.then(|| {
        (
            i64::from(window.right) - i64::from(window.left) + 1,
            i64::from(window.bottom) - i64::from(window.top) + 1,
        )
    })
}

#[cfg(not(any(unix, windows)))]
fn stdout_terminal() -> Option<(i64, i64)> {
    None
}

/// `shutil.get_terminal_size((80, 24)).columns`: $COLUMNS when it is a
/// positive integer; otherwise the terminal on stdout, else 80. The terminal
/// is asked whenever either $COLUMNS or $LINES is not a positive integer.
pub fn columns() -> i64 {
    let env_int = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|value| py_int(&value))
            .unwrap_or(0)
    };
    let columns = env_int("COLUMNS");
    if columns > 0 && env_int("LINES") > 0 {
        return columns;
    }
    let terminal = stdout_terminal();
    if columns > 0 {
        return columns;
    }
    terminal
        .map(|(width, _)| width)
        .filter(|&width| width > 0)
        .unwrap_or(80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_parse_like_python_int() {
        assert_eq!(py_int(" 40 "), Some(40));
        assert_eq!(py_int("+1_0"), Some(10));
        assert_eq!(py_int("-3"), Some(-3));
        assert_eq!(py_int("\u{0664}\u{0660}"), Some(40));
        for bad in ["", " ", "4__0", "_40", "40_", "4.0", "0x10", "--1"] {
            assert_eq!(py_int(bad), None, "{bad}");
        }
    }
}
