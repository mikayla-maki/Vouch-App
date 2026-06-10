//! Shared text-formatting helpers used by multiple UI components.

use std::time::SystemTime;

/// Output style for [`format_relative_time`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeStyle {
    /// Short form, e.g. "5m ago", "2h ago", "3d ago", "1mo ago".
    Compact,
    /// Long form, e.g. "5 minutes ago", "2 hours ago", "3 days ago".
    Verbose,
}

/// Formats how long ago `timestamp` was, relative to now.
pub fn format_relative_time(timestamp: SystemTime, style: TimeStyle) -> String {
    let duration = SystemTime::now()
        .duration_since(timestamp)
        .unwrap_or(std::time::Duration::ZERO);

    let seconds = duration.as_secs();
    let minutes = seconds / 60;
    let hours = seconds / 3600;
    let days = seconds / 86400;

    if seconds < 60 {
        return "Just now".to_string();
    }

    match style {
        TimeStyle::Compact => {
            if hours == 0 {
                format!("{}m ago", minutes)
            } else if days == 0 {
                format!("{}h ago", hours)
            } else if days < 30 {
                format!("{}d ago", days)
            } else {
                format!("{}mo ago", days / 30)
            }
        }
        TimeStyle::Verbose => {
            let (count, unit) = if days > 0 {
                (days, "day")
            } else if hours > 0 {
                (hours, "hour")
            } else {
                (minutes, "minute")
            };
            format!(
                "{} {}{} ago",
                count,
                unit,
                if count == 1 { "" } else { "s" }
            )
        }
    }
}

/// Truncates `text` to at most `max_chars` characters, appending "..." if
/// anything was cut. Both the guard and the cut are measured in chars, so
/// multi-byte text within the character limit is returned unchanged.
pub fn truncate(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        None => text.to_string(),
        Some((byte_index, _)) => format!("{}...", text[..byte_index].trim_end()),
    }
}
