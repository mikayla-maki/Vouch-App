//! The materializer's core: a pure fold over claims that knows nothing
//! about "recommendations" — only that some claims (of any type)
//! reference others, and that connectivity through those references is
//! what identity means here. There is no minted id: a "thing" is the
//! connected component of the reference graph, so two claims filed
//! independently that later get linked by a third claim collapse into
//! one component with nothing to rewrite.
//!
//! Three roles participate:
//! - **root** (e.g. `rec`) — the thing itself.
//! - **edit** — a field-level patch, counted only when its author matches
//!   one of the component's root authors. Edits are for the source to
//!   correct or enrich their own claim, not for anyone to rewrite anyone
//!   else's; a stray edit from someone else is simply inert, excluded
//!   from the fold, still sitting in the store for the debug viewer to
//!   show.
//! - **comment** — wide open, any author, contributes to connectivity
//!   like anything else but is collected as its own list rather than
//!   merged into `fields`. Comments don't compete for a value; they just
//!   coexist.
//!
//! Each field is folded independently, as a proper join-semilattice: the
//! state of a field is the *causal frontier* of claims that set it — the
//! ones not dominated (via the same reference edges) by some other claim
//! that also sets that field. A frontier of one is an unambiguous current
//! value; a frontier of more than one is a real, unreconciled conflict,
//! exposed rather than silently resolved — most often now the same
//! author's own concurrent edits (two devices, offline, neither aware of
//! the other) rather than a dispute between strangers. Reconciliation is
//! nothing special: a later edit that references every currently-
//! concurrent claim and re-asserts the field collapses the frontier back
//! to one — the same shape as a merge commit. An edit that references the
//! same claims but doesn't re-touch the field resolves nothing for it;
//! the conflict just stands until someone does.
//!
//! Union-then-drop-dominated is commutative, associative, and idempotent,
//! so this converges to the same answer regardless of what order claims
//! arrived in — the same invariant the rest of this crate already leans
//! on for sync. Content-addressing makes the reference graph acyclic by
//! construction (a claim can't reference a hash that doesn't exist yet),
//! so dominance is a plain reachability walk with no cycle bookkeeping.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::keys::LogId;
use crate::value::{ClaimHash, Fields, Value};

/// A claim as the fold sees it: body already resolved to plaintext.
///
/// This is the seam end-to-end encryption slots under: plaintext claims
/// pass into the view unchanged, encrypted envelopes are resolved by
/// whoever holds the key (see [`e2ee::decrypted_view`]), and the fold
/// itself never knows the difference. Its `refs` are recomputed from the
/// resolved body — an encrypted claim's references are invisible to
/// ingest-time indexes by design.
///
/// [`e2ee::decrypted_view`]: crate::e2ee::decrypted_view
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimView {
    pub id: ClaimHash,
    pub author: LogId,
    pub received_at: i64,
    /// The plaintext body map.
    pub body: BTreeMap<String, Value>,
    /// Outgoing claim references of the plaintext body.
    pub refs: Vec<ClaimHash>,
}

impl ClaimView {
    /// The body's vocabulary tag.
    pub fn claim_type(&self) -> Option<&str> {
        match self.body.get("type") {
            Some(Value::Text(t)) => Some(t.as_str()),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Root,
    Edit,
    Comment,
}

/// One claim's contribution to one field: what it said, and who said it.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldContribution {
    pub claim: ClaimHash,
    pub author: LogId,
    pub value: Value,
    /// The claimed (self-reported) time — display only, never load-bearing
    /// for the fold itself. Ordering and dominance come from the reference
    /// graph, not from anyone's clock.
    pub at: i64,
}

/// A field's current state: one contribution is an unambiguous value;
/// more than one is an exposed, unreconciled conflict.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FieldState {
    /// The causal frontier: what's currently live (undominated).
    pub frontier: Vec<FieldContribution>,
    /// Every contribution ever made to this field, oldest first —
    /// including ones a later edit has since dominated. This is the raw
    /// material for a "history of edits" view; `frontier` alone can't
    /// answer "what did this used to say."
    pub history: Vec<FieldContribution>,
}

impl FieldState {
    /// The contribution shown by default — a deterministic tiebreak by
    /// claim hash over the frontier, never authoritative. Callers that
    /// care about a live conflict should read `frontier` directly instead.
    pub fn current_contribution(&self) -> Option<&FieldContribution> {
        self.frontier.iter().min_by_key(|c| c.claim)
    }

    /// Just the value half of [`Self::current_contribution`].
    pub fn current(&self) -> Option<&Value> {
        self.current_contribution().map(|c| &c.value)
    }
}

/// One comment: never merged, just collected — a comment doesn't compete
/// with anything, it just is.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub claim: ClaimHash,
    pub author: LogId,
    pub text: String,
    pub at: i64,
}

/// One materialized "thing": every claim in its connected component, the
/// folded state of every field any root/edit claim set, and every comment
/// attached to it.
#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    /// A deterministic, recomputed-not-stored key: the smallest claim hash
    /// in the component. Stable across re-folds of the same claim set;
    /// not a minted identity, just a reproducible way to key a list.
    pub id: ClaimHash,
    pub claims: BTreeSet<ClaimHash>,
    pub fields: BTreeMap<String, FieldState>,
    pub comments: Vec<Comment>,
}

/// Fold every `root_type` claim, plus any `edit_type`/`comment_type` claim
/// that references into its component, into components.
///
/// `accept` is the viewer's policy: a claim it rejects is dropped before
/// folding, exactly as if it had never arrived. This is what makes
/// "current state" viewer-relative rather than one objective answer — the
/// same claim set can fold to different results for different policies.
pub fn fold(
    view: &[ClaimView],
    root_type: &str,
    edit_type: &str,
    comment_type: &str,
    accept: &dyn Fn(&ClaimView) -> bool,
) -> Vec<Component> {
    let mut nodes: HashMap<ClaimHash, ClaimView> = HashMap::new();
    let mut roles: HashMap<ClaimHash, Role> = HashMap::new();
    for claim in view.iter().filter(|c| accept(c)) {
        let role = match claim.claim_type() {
            Some(t) if t == root_type => Role::Root,
            Some(t) if t == edit_type => Role::Edit,
            Some(t) if t == comment_type => Role::Comment,
            _ => continue,
        };
        roles.insert(claim.id, role);
        nodes.insert(claim.id, claim.clone());
    }

    // Outgoing edges: only references that land on another node we kept —
    // a reference to something outside this claim-type triple (a vouch, a
    // blob) isn't part of this graph.
    let edges: HashMap<ClaimHash, Vec<ClaimHash>> = nodes
        .iter()
        .map(|(id, claim)| {
            let out: Vec<ClaimHash> = claim
                .refs
                .iter()
                .copied()
                .filter(|h| nodes.contains_key(h))
                .collect();
            (*id, out)
        })
        .collect();

    connected_components(&nodes, &edges)
        .into_iter()
        .map(|ids| build_component(&nodes, &roles, &edges, ids))
        .collect()
}

fn connected_components(
    nodes: &HashMap<ClaimHash, ClaimView>,
    edges: &HashMap<ClaimHash, Vec<ClaimHash>>,
) -> Vec<BTreeSet<ClaimHash>> {
    let mut undirected: HashMap<ClaimHash, HashSet<ClaimHash>> = HashMap::new();
    for id in nodes.keys() {
        undirected.entry(*id).or_default();
    }
    for (from, targets) in edges {
        for to in targets {
            undirected.entry(*from).or_default().insert(*to);
            undirected.entry(*to).or_default().insert(*from);
        }
    }

    let mut seen: HashSet<ClaimHash> = HashSet::new();
    let mut components = Vec::new();
    for &start in nodes.keys() {
        if seen.contains(&start) {
            continue;
        }
        let mut component = BTreeSet::new();
        let mut stack = vec![start];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            component.insert(id);
            for &n in undirected.get(&id).into_iter().flatten() {
                if !seen.contains(&n) {
                    stack.push(n);
                }
            }
        }
        components.push(component);
    }
    components
}

fn build_component(
    nodes: &HashMap<ClaimHash, ClaimView>,
    roles: &HashMap<ClaimHash, Role>,
    edges: &HashMap<ClaimHash, Vec<ClaimHash>>,
    ids: BTreeSet<ClaimHash>,
) -> Component {
    // ancestors[id] = every claim reachable from id by following outgoing
    // references — its causal history. A claim dominates another (for
    // frontier purposes) iff the other is in its ancestor set.
    let mut ancestors: HashMap<ClaimHash, HashSet<ClaimHash>> = HashMap::new();
    for &id in &ids {
        let mut seen = HashSet::new();
        let mut stack = edges.get(&id).cloned().unwrap_or_default();
        while let Some(n) = stack.pop() {
            if seen.insert(n) {
                stack.extend(edges.get(&n).cloned().unwrap_or_default());
            }
        }
        ancestors.insert(id, seen);
    }

    let root_authors: HashSet<LogId> = ids
        .iter()
        .filter(|id| roles.get(*id) == Some(&Role::Root))
        .filter_map(|id| nodes.get(id))
        .map(|c| c.author)
        .collect();

    // field name -> every root/edit claim in this component that sets it
    // and is entitled to (roots always are; edits only from a root author).
    let mut candidates: BTreeMap<String, Vec<ClaimHash>> = BTreeMap::new();
    for &id in &ids {
        let (Some(role), Some(claim)) = (roles.get(&id), nodes.get(&id)) else {
            continue;
        };
        match role {
            Role::Comment => continue,
            Role::Edit if !root_authors.contains(&claim.author) => continue,
            Role::Root | Role::Edit => {}
        }
        for key in claim.body.keys() {
            if key == "at" || key == "of" || key == "type" {
                continue; // metadata, not a field of the thing itself
            }
            candidates.entry(key.clone()).or_default().push(id);
        }
    }

    let mut fields = BTreeMap::new();
    for (field, claim_ids) in candidates {
        let dominated: HashSet<ClaimHash> = claim_ids
            .iter()
            .flat_map(|id| {
                ancestors
                    .get(id)
                    .into_iter()
                    .flatten()
                    .filter(|a| claim_ids.contains(a))
            })
            .copied()
            .collect();
        // Every contribution, oldest first — the field's full history, not
        // just what's currently winning. `at` is self-reported (display
        // only), so the tie-break by claim hash is what keeps this
        // deterministic when two contributions claim the same time.
        let mut history: Vec<FieldContribution> = claim_ids
            .iter()
            .filter_map(|id| contribution(nodes, &field, *id))
            .collect();
        history.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.claim.cmp(&b.claim)));
        let frontier: Vec<FieldContribution> = history
            .iter()
            .filter(|c| !dominated.contains(&c.claim))
            .cloned()
            .collect();
        fields.insert(field, FieldState { frontier, history });
    }

    let comments: Vec<Comment> = ids
        .iter()
        .filter(|id| roles.get(*id) == Some(&Role::Comment))
        .filter_map(|id| {
            let claim = nodes.get(id)?;
            let fields = Fields::of(&claim.body);
            Some(Comment {
                claim: *id,
                author: claim.author,
                text: fields.text("text")?.to_string(),
                at: fields.int("at").unwrap_or(claim.received_at),
            })
        })
        .collect();

    Component {
        id: *ids.iter().min().expect("component is never empty"),
        claims: ids,
        fields,
        comments,
    }
}

/// One claim's contribution to one named field, if it actually sets it.
fn contribution(
    nodes: &HashMap<ClaimHash, ClaimView>,
    field: &str,
    id: ClaimHash,
) -> Option<FieldContribution> {
    let claim = nodes.get(&id)?;
    let value = claim.body.get(field)?.clone();
    let at = Fields::of(&claim.body).int("at").unwrap_or(claim.received_at);
    Some(FieldContribution {
        claim: id,
        author: claim.author,
        value,
        at,
    })
}
