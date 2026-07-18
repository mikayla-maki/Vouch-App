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

    /// Every claim that shaped this recommendation, oldest first: each
    /// field's full history (superseded entries included, marked
    /// `current: false`) interleaved with comments by claimed time. The
    /// raw material for a "see edits/changes" view — unlike `subject`/
    /// `body`, nothing here is folded away.
    pub fn timeline(&self) -> Vec<TimelineEntry> {
        let mut entries: Vec<TimelineEntry> = Vec::new();
        for (field, state) in &self.fields {
            let current: BTreeSet<ClaimHash> = state.frontier.iter().map(|c| c.claim).collect();
            for c in &state.history {
                entries.push(TimelineEntry::Field {
                    claim: c.claim,
                    author: c.author,
                    at: c.at,
                    field: field.clone(),
                    value: c.value.clone(),
                    current: current.contains(&c.claim),
                });
            }
        }
        for c in &self.comments {
            entries.push(TimelineEntry::Comment {
                claim: c.claim,
                author: c.author,
                at: c.at,
                text: c.text.clone(),
            });
        }
        entries.sort_by(|a, b| a.at().cmp(&b.at()).then_with(|| a.claim().cmp(&b.claim())));
        entries
    }
}

/// One entry in a recommendation's timeline — a fact about a single claim,
/// not a folded conclusion. `current` on a `Field` entry says whether it's
/// still part of the live frontier or has since been superseded.
#[derive(Debug, Clone, PartialEq)]
pub enum TimelineEntry {
    Field {
        claim: ClaimHash,
        author: LogId,
        at: i64,
        field: String,
        value: Value,
        current: bool,
    },
    Comment {
        claim: ClaimHash,
        author: LogId,
        at: i64,
        text: String,
    },
}

impl TimelineEntry {
    pub fn at(&self) -> i64 {
        match self {
            TimelineEntry::Field { at, .. } | TimelineEntry::Comment { at, .. } => *at,
        }
    }

    pub fn claim(&self) -> ClaimHash {
        match self {
            TimelineEntry::Field { claim, .. } | TimelineEntry::Comment { claim, .. } => *claim,
        }
    }

    pub fn author(&self) -> LogId {
        match self {
            TimelineEntry::Field { author, .. } | TimelineEntry::Comment { author, .. } => *author,
        }
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
