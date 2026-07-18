//! The `profile` claim type's projection: the name each log currently
//! suggests for itself.
//!
//! A profile claim is ordinary speech (`type: "profile"`, `name: ...`),
//! published to your own log and riding the same sync as everything else
//! — which is what makes an advertised name reach followers transitively
//! through relays instead of only whoever you're directly connected to.
//! Names are self-asserted: nothing stops two logs from both claiming
//! "Alice", which is why UIs render them alongside the log id's hash
//! prefix (and why local petname overrides exist as a concept — they
//! just aren't this module's business; this is only the *suggestion*).
//!
//! Resolution is deliberately simpler than the rec fold: the newest
//! profile claim per log wins, by self-reported `at` with a hash
//! tie-break. Display metadata doesn't need causal frontiers — if your
//! two devices disagree about your name for an hour, showing either is
//! fine, and the next profile claim settles it.

use std::collections::BTreeMap;

use crate::keys::LogId;
use crate::store::ClaimStore;
use crate::value::{ClaimHash, Fields, Value};

/// Longest name we'll surface, in characters. A name is display text, not
/// a payload channel.
pub const MAX_NAME_LEN: usize = 40;

/// The name each log currently suggests for itself: newest `profile`
/// claim per log, sanitized. Logs with no (usable) profile claim are
/// simply absent — callers fall back to the hash prefix.
pub fn names(store: &ClaimStore) -> BTreeMap<LogId, String> {
    let mut best: BTreeMap<LogId, (i64, ClaimHash, String)> = BTreeMap::new();
    for claim in store.by_type("profile") {
        let Some(Value::Map(map)) = &claim.body else {
            continue;
        };
        let fields = Fields::of(map);
        let Some(name) = fields.text("name").map(sanitize_name) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let at = fields.int("at").unwrap_or(claim.received_at);
        let id = claim.signed.id();
        let log = claim.header.log_id;
        let newer = match best.get(&log) {
            Some((best_at, best_id, _)) => (at, id) > (*best_at, *best_id),
            None => true,
        };
        if newer {
            best.insert(log, (at, id, name));
        }
    }
    best.into_iter().map(|(log, (_, _, name))| (log, name)).collect()
}

/// Trim, strip control characters, cap length. Applied at both publish
/// and display, so a hostile profile claim can't smuggle layout-breaking
/// text into every follower's UI.
pub fn sanitize_name(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_LEN)
        .collect::<String>()
        .trim()
        .to_string()
}
