//! The `rec` claim type's own projection over [`fold`]: the typed reader
//! that knows what a recommendation's body schema means, sitting on top
//! of a fold core that doesn't. `subject`/`body` are pulled out as
//! convenience accessors (the two fields every recommendation has); the
//! full `fields` map stays available underneath for anything else anyone
//! ever enriches it with, conflicts included.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::e2ee;
use crate::fold::{self, ClaimView, Comment, FieldState};
use crate::keys::LogId;
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
    /// Claims in this component the author went on the record about:
    /// every claim bound by a *valid* attestation (right author, signature
    /// verifies against the exact plaintext in the view). An attestation
    /// binds specific words — an edit after attesting is new, unattested
    /// speech, which is why this is a set of claims and not a flag.
    pub attested: BTreeSet<ClaimHash>,
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

    /// On the record: everything currently shown (the live `subject` and
    /// `body` contributions) is attested. The strongest badge state.
    pub fn on_the_record(&self) -> bool {
        ["subject", "body"].iter().all(|field| {
            self.fields
                .get(*field)
                .and_then(FieldState::current_contribution)
                .is_some_and(|c| self.attested.contains(&c.claim))
        })
    }

    /// The author attested some earlier version, but the text shown now
    /// has moved past it — render "attested as of an earlier version",
    /// never the full badge.
    pub fn attested_earlier(&self) -> bool {
        !self.attested.is_empty() && !self.on_the_record()
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
/// redacted) isn't a recommendation the UI can show. Takes the decrypted
/// view (see [`e2ee::decrypted_view`]): what you can't decrypt, you
/// can't fold.
///
/// [`e2ee::decrypted_view`]: crate::e2ee::decrypted_view
pub fn recommendations(
    view: &[ClaimView],
    accept: &dyn Fn(&ClaimView) -> bool,
) -> Vec<Recommendation> {
    let attested = valid_attestations(view);
    fold::fold(view, ROOT_TYPE, EDIT_TYPE, COMMENT_TYPE, accept)
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
                attested: c.claims.intersection(&attested).copied().collect(),
                claims: c.claims,
                subject,
                body,
                fields: c.fields,
                comments: c.comments,
            })
        })
        .collect()
}

/// Every claim in the view bound by a valid attestation: the attest claim
/// names it (`of`), was uttered by the *same log* (your attestation only
/// ever binds your own words), and its Ed25519 signature verifies against
/// the exact plaintext this view decrypted. An attest that fails any of
/// those is inert — an invalid signature never downgrades the claim it
/// points at, it just fails to escalate it.
fn valid_attestations(view: &[ClaimView]) -> BTreeSet<ClaimHash> {
    let by_id: HashMap<ClaimHash, &ClaimView> = view.iter().map(|c| (c.id, c)).collect();
    let mut attested = BTreeSet::new();
    for attest in view {
        if attest.claim_type() != Some(e2ee::ATTEST_TYPE) {
            continue;
        }
        for target_id in &attest.refs {
            let Some(target) = by_id.get(target_id) else {
                continue;
            };
            if target.author != attest.author {
                continue;
            }
            let words = Value::Map(target.body.clone());
            if e2ee::verify_attest(target.author, *target_id, &words, &attest.body) {
                attested.insert(*target_id);
            }
        }
    }
    attested
}
