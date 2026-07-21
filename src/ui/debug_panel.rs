//! The raw-claims debug viewer: one row per claim in the local database, of
//! any type, rendered generically.
//!
//! This is a developer tool, not consumer UI. It renders whatever is actually
//! in each claim body — no per-type special-casing — so a developer can see
//! the true converged state (every `rec`, `edit`, `comment`, `profile`,
//! `redact`, unknown vocabulary, …), not just the folded `rec` feed the
//! normal UI shows. It observes [`DebugFeed`] and live-updates on every
//! firehose event, same as the real feed.

use crate::debug_feed::{DebugFeed, claim_at, claim_type};
use crate::ui::format::{TimeStyle, format_relative_time};
use gpui::*;
use gpui_component::theme::{ActiveTheme, Theme};
use std::time::{Duration, UNIX_EPOCH};
use vouch_core::{LogId, StoredClaim, Value};

pub struct DebugPanel {
    debug: Entity<DebugFeed>,
    local_log_id: Option<LogId>,
}

impl DebugPanel {
    pub fn new(debug: Entity<DebugFeed>, cx: &mut Context<Self>) -> Self {
        cx.observe(&debug, |_, _, cx| cx.notify()).detach();
        let local_log_id = debug.read(cx).peer().id();
        Self {
            debug,
            local_log_id,
        }
    }

    fn render_row(&self, claim: &StoredClaim, theme: &Theme) -> impl IntoElement {
        let id = claim.event.id();
        let author = claim.header.log_id;
        let by = match self.local_log_id {
            Some(me) if me == author => "you".to_string(),
            _ => author.short(),
        };
        let type_tag = claim_type(claim).unwrap_or("(untyped)").to_string();
        let timestamp = format_relative_time(
            UNIX_EPOCH + Duration::from_millis(claim_at(claim).max(0) as u64),
            TimeStyle::Compact,
        );

        // Header: type badge, short hash, author, time.
        let header = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                div()
                    .px_2()
                    .py(px(1.0))
                    .rounded_md()
                    .bg(theme.secondary)
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.secondary_foreground)
                    .child(type_tag),
            )
            .child(
                div()
                    .font_family("monospace")
                    .text_xs()
                    .text_color(theme.foreground)
                    .child(id.short()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.primary_hover)
                    .child(format!("by {by}")),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child("•"))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(timestamp),
            );

        // Body: a generic recursive dump, one div per line, indented by depth.
        let body_lines: Vec<Div> = match &claim.body {
            Some(body) => format_body_lines(body)
                .into_iter()
                .map(|(depth, text)| {
                    div()
                        .pl(px(depth as f32 * 14.0))
                        .font_family("monospace")
                        .text_xs()
                        .text_color(theme.foreground)
                        .child(text)
                })
                .collect(),
            None => vec![
                div()
                    .font_family("monospace")
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("(no body — tombstone or not yet fetched)"),
            ],
        };

        div()
            .id(ElementId::Name(format!("debug-claim-{id}").into()))
            .w_full()
            .p_3()
            .bg(theme.colors.list)
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .flex()
            .flex_col()
            .gap_2()
            .child(header)
            .child(div().flex().flex_col().gap_1().children(body_lines))
    }
}

impl Render for DebugPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let claims = self.debug.read(cx).claims();
        let count = claims.len();

        let rows: Vec<_> = claims
            .iter()
            .map(|claim| self.render_row(claim, &theme))
            .collect();

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.foreground)
                    .child("Raw claims"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{count} claim{}", if count == 1 { "" } else { "s" })),
            );

        let list = if rows.is_empty() {
            div()
                .id("debug-empty")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .flex_1()
                .p_4()
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("The local database holds no claims yet"),
                )
        } else {
            div()
                .id("debug-claim-list")
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .overflow_y_scroll()
                .flex_1()
                .children(rows)
        };

        div()
            .flex()
            .flex_col()
            .h_full()
            .flex_1()
            .bg(theme.background)
            .child(header)
            .child(list)
    }
}

/// Render a claim body as a flat list of `(depth, text)` lines — a readable
/// tree of whatever is actually in the map, for ANY claim type. Unknown
/// fields, unknown tags, links, embeds and blobs all render generically; no
/// vocabulary is special-cased. Callers indent each line by its depth.
pub fn format_body_lines(body: &Value) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    fmt_value(None, body, 0, &mut out);
    out
}

fn fmt_value(label: Option<String>, value: &Value, depth: usize, out: &mut Vec<(usize, String)>) {
    match value {
        Value::Map(entries) => {
            // The root body has no label; its keys sit at depth 0. A nested
            // map prints its label as a header, then its keys one deeper.
            let child_depth = match &label {
                Some(l) => {
                    out.push((depth, format!("{l}:")));
                    depth + 1
                }
                None => depth,
            };
            for (k, v) in entries {
                fmt_value(Some(k.clone()), v, child_depth, out);
            }
        }
        Value::Array(items) => {
            let l = label.unwrap_or_default();
            out.push((depth, format!("{l}: [{} item(s)]", items.len())));
            for (i, item) in items.iter().enumerate() {
                fmt_value(Some(format!("[{i}]")), item, depth + 1, out);
            }
        }
        scalar => {
            let text = match label {
                Some(l) => format!("{l}: {}", fmt_scalar(scalar)),
                None => fmt_scalar(scalar),
            };
            out.push((depth, text));
        }
    }
}

/// A one-line rendering of a leaf value (everything that isn't a map or array).
fn fmt_scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Int(n) => n.to_string(),
        Value::Text(t) => format!("{t:?}"),
        Value::Bytes(b) => format!("<{} bytes>", b.len()),
        Value::ClaimRef(r) => {
            format!("→ claim {}/{}", r.log_id.short(), r.hash.short())
        }
        Value::BlobRef(b) => {
            format!("⧉ blob {} ({} bytes, {})", b.hash.short(), b.size, b.mime)
        }
        Value::Embed(e) => match e.check() {
            Ok(claim) => {
                let ty = match &claim.body {
                    Some(Value::Map(m)) => match m.get("type") {
                        Some(Value::Text(t)) => t.as_str(),
                        _ => "?",
                    },
                    _ => "?",
                };
                format!("« embed {} type={ty} »", e.id().short())
            }
            Err(_) => format!("« embed {} (malformed) »", e.id().short()),
        },
        Value::Tagged(tag, inner) => format!("tag({tag}) {}", fmt_scalar(inner)),
        // Maps and arrays are handled by fmt_value; a nested one reaching here
        // (only via Tagged) degrades to a compact summary rather than panicking.
        Value::Map(m) => format!("{{{} field(s)}}", m.len()),
        Value::Array(a) => format!("[{} item(s)]", a.len()),
    }
}
