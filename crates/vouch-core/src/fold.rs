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
use crate::store::{ClaimStore, StoredClaim};
use crate::value::{ClaimHash, Fields, Value};

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
    pub frontier: Vec<FieldContribution>,
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
    store: &ClaimStore,
    root_type: &str,
    edit_type: &str,
    comment_type: &str,
    accept: &dyn Fn(&StoredClaim) -> bool,
) -> Vec<Component> {
    let mut nodes: HashMap<ClaimHash, StoredClaim> = HashMap::new();
    let mut roles: HashMap<ClaimHash, Role> = HashMap::new();
    for (claim_type, role) in [
        (root_type, Role::Root),
        (edit_type, Role::Edit),
        (comment_type, Role::Comment),
    ] {
        for claim in store.by_type(claim_type).into_iter().filter(|c| accept(c)) {
            let id = claim.signed.id();
            roles.insert(id, role);
            nodes.insert(id, claim);
        }
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
                .map(|(_, r)| r.hash)
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
    nodes: &HashMap<ClaimHash, StoredClaim>,
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
    nodes: &HashMap<ClaimHash, StoredClaim>,
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
        .map(|c| c.header.log_id)
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
            Role::Edit if !root_authors.contains(&claim.header.log_id) => continue,
            Role::Root | Role::Edit => {}
        }
        let Some(Value::Map(map)) = &claim.body else {
            continue; // tombstoned or bodiless: contributes to no field
        };
        for key in map.keys() {
            if key == "at" || key == "of" {
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
        let frontier: Vec<FieldContribution> = claim_ids
            .into_iter()
            .filter(|id| !dominated.contains(id))
            .filter_map(|id| {
                let claim = nodes.get(&id)?;
                let Value::Map(map) = claim.body.as_ref()? else {
                    return None;
                };
                let value = map.get(&field)?.clone();
                let at = Fields::of(map).int("at").unwrap_or(claim.received_at);
                Some(FieldContribution {
                    claim: id,
                    author: claim.header.log_id,
                    value,
                    at,
                })
            })
            .collect();
        fields.insert(field, FieldState { frontier });
    }

    let comments: Vec<Comment> = ids
        .iter()
        .filter(|id| roles.get(*id) == Some(&Role::Comment))
        .filter_map(|id| {
            let claim = nodes.get(id)?;
            let Value::Map(map) = claim.body.as_ref()? else {
                return None;
            };
            let fields = Fields::of(map);
            Some(Comment {
                claim: *id,
                author: claim.header.log_id,
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
