//! The `rec` claim type's own projection over [`fold`]: the typed reader
//! that knows what a recommendation's body schema means, sitting on top
//! of a fold core that doesn't. `subject`/`body` are pulled out as
//! convenience accessors (the two fields every recommendation has); the
//! full `fields` map stays available underneath for anything else anyone
//! ever enriches it with, conflicts included.

use std::collections::{BTreeMap, BTreeSet};

use crate::fold::{self, Comment, FieldState};
use crate::keys::LogId;
use crate::store::{ClaimStore, StoredClaim};
use crate::value::{ClaimHash, Value};

pub const ROOT_TYPE: &str = "rec";
pub const EDIT_TYPE: &str = "edit";
pub const COMMENT_TYPE: &str = "comment";

#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    pub id: ClaimHash,
    pub claims: BTreeSet<ClaimHash>,
    pub subject: String,
    pub body: String,
    pub fields: BTreeMap<String, FieldState>,
    pub comments: Vec<Comment>,
}

impl Recommendation {
    /// The author currently shown — whoever's contribution is winning
    /// for `subject` right now. A recommendation can have more than one
    /// contributor (collated originals, accepted edits); this is a
    /// pragmatic single label for a UI that wants one name, not the full
    /// provenance (read `fields` for that).
    pub fn author(&self) -> Option<LogId> {
        self.fields
            .get("subject")
            .and_then(FieldState::current_contribution)
            .map(|c| c.author)
    }

    /// When the currently-shown `subject` text was written — display
    /// only, same caveat as [`Self::author`].
    pub fn at_ms(&self) -> i64 {
        self.fields
            .get("subject")
            .and_then(FieldState::current_contribution)
            .map(|c| c.at)
            .unwrap_or(0)
    }
}

fn as_text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(t) => Some(t.as_str()),
        _ => None,
    }
}

/// Every `rec` component that still has a `subject` and `body` after
/// folding — a component missing either (malformed, or every contributor
/// redacted) isn't a recommendation the UI can show.
pub fn recommendations(
    store: &ClaimStore,
    accept: &dyn Fn(&StoredClaim) -> bool,
) -> Vec<Recommendation> {
    fold::fold(store, ROOT_TYPE, EDIT_TYPE, COMMENT_TYPE, accept)
        .into_iter()
        .filter_map(|c| {
            let subject = c
                .fields
                .get("subject")
                .and_then(FieldState::current)
                .and_then(as_text)?
                .to_string();
            let body = c
                .fields
                .get("body")
                .and_then(FieldState::current)
                .and_then(as_text)?
                .to_string();
            Some(Recommendation {
                id: c.id,
                claims: c.claims,
                subject,
                body,
                fields: c.fields,
                comments: c.comments,
            })
        })
        .collect()
}
