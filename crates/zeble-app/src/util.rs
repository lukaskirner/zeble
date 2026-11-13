//! Small helpers shared by the views.

use std::future::Future;

use gpui_kit::{Hsla, SharedString, rgb};

/// Run `fut` on the database runtime and await it from GPUI's executor.
pub fn on_db<T: Send + 'static>(
    fut: impl Future<Output = T> + Send + 'static,
) -> impl Future<Output = anyhow::Result<T>> {
    let handle = zeble_core::runtime().spawn(fut);
    async move { handle.await.map_err(|e| anyhow::anyhow!("database task failed: {e}")) }
}

/// `1234567` -> `1,234,567`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A short human duration: `850 ms`, `2.3 s`, `1 min 5 s`.
pub fn duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", d.as_secs_f64())
    } else {
        format!("{} min {} s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// One line of a cell value for the grid: the first line, at most `max` chars.
pub fn cell_preview(value: &str, max: usize) -> SharedString {
    let first = value.lines().next().unwrap_or("");
    let multiline = value.contains('\n');
    let mut out: String = first.chars().take(max).collect();
    if multiline || first.chars().count() > max {
        out.push('…');
    }
    out.into()
}

pub fn color_tag(tag: zeble_core::ColorTag) -> Hsla {
    rgb(tag.rgb()).into()
}

pub fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(999), "999");
        assert_eq!(cell_preview("abc\ndef", 10).as_ref(), "abc…");
        assert_eq!(cell_preview("abcdef", 3).as_ref(), "abc…");
        assert_eq!(cell_preview("ab", 3).as_ref(), "ab");
        assert_eq!(duration(std::time::Duration::from_millis(2300)), "2.3 s");
    }
}

/// Translate `mod-x` into `cmd-x` on macOS and `ctrl-x` elsewhere.
/// Bindings are registered once at startup, so leaking the string is fine.
pub fn keys(binding: &str) -> &'static str {
    let m = if cfg!(target_os = "macos") { "cmd-" } else { "ctrl-" };
    Box::leak(binding.replace("mod-", m).into_boxed_str())
}
