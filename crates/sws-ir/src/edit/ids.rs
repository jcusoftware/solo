//! Stable node IDs (`uid`): grammar, traversal, uniqueness checks, editability
//! gate and the explicit deterministic upgrade for legacy documents.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::command::{CommandError, ErrorCode};
use crate::types::{Node, SiteDocument};
use crate::validate::validate_ir;

/// `metadata` key carrying the stable-ID marker.
pub const MARKER_KEY: &str = "sws.ir";

/// The only supported value of `metadata["sws.ir"].uid_scheme`.
pub const UID_SCHEME: u64 = 1;

const MAX_UID_LEN: usize = 64;
const MAX_REPORTED_PATHS: usize = 20;

/// `^[a-z0-9][a-z0-9._-]{0,63}$`
#[must_use]
pub fn is_valid_uid(uid: &str) -> bool {
    let bytes = uid.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_UID_LEN {
        return false;
    }
    let first_ok = bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit();
    first_ok
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

impl Node {
    /// The node's stable editor ID, if assigned.
    #[must_use]
    pub fn uid(&self) -> Option<&str> {
        match self {
            Self::Element { uid, .. }
            | Self::Text { uid, .. }
            | Self::SlotRef { uid, .. }
            | Self::ComponentInstance { uid, .. }
            | Self::Conditional { uid, .. }
            | Self::Loop { uid, .. } => uid.as_deref(),
        }
    }

    /// Set or clear the stable editor ID.
    pub fn set_uid(&mut self, value: Option<String>) {
        match self {
            Self::Element { uid, .. }
            | Self::Text { uid, .. }
            | Self::SlotRef { uid, .. }
            | Self::ComponentInstance { uid, .. }
            | Self::Conditional { uid, .. }
            | Self::Loop { uid, .. } => *uid = value,
        }
    }

    fn variant_label(&self) -> &'static str {
        match self {
            Self::Element { .. } => "element",
            Self::Text { .. } => "text",
            Self::SlotRef { .. } => "slot",
            Self::ComponentInstance { .. } => "instance",
            Self::Conditional { .. } => "if",
            Self::Loop { .. } => "loop",
        }
    }
}

/// Child lists of a node in the fixed traversal order, with their slot names.
#[must_use]
pub fn child_lists(node: &Node) -> Vec<(String, &Vec<Node>)> {
    match node {
        Node::Element { children, .. } => vec![("children".to_string(), children)],
        Node::Text { .. } => Vec::new(),
        Node::SlotRef { fallback, .. } => vec![("fallback".to_string(), fallback)],
        Node::ComponentInstance { slots, .. } => slots
            .iter()
            .map(|(name, list)| (format!("slots.{name}"), list))
            .collect(),
        Node::Conditional {
            then_nodes,
            else_nodes,
            ..
        } => vec![
            ("then_nodes".to_string(), then_nodes),
            ("else_nodes".to_string(), else_nodes),
        ],
        Node::Loop { body, .. } => vec![("body".to_string(), body)],
    }
}

/// Mutable child lists in the same order as [`child_lists`].
pub fn child_lists_mut(node: &mut Node) -> Vec<(String, &mut Vec<Node>)> {
    match node {
        Node::Element { children, .. } => vec![("children".to_string(), children)],
        Node::Text { .. } => Vec::new(),
        Node::SlotRef { fallback, .. } => vec![("fallback".to_string(), fallback)],
        Node::ComponentInstance { slots, .. } => slots
            .iter_mut()
            .map(|(name, list)| (format!("slots.{name}"), list))
            .collect(),
        Node::Conditional {
            then_nodes,
            else_nodes,
            ..
        } => vec![
            ("then_nodes".to_string(), then_nodes),
            ("else_nodes".to_string(), else_nodes),
        ],
        Node::Loop { body, .. } => vec![("body".to_string(), body)],
    }
}

/// Visit every node in document order (pages in array order, then components
/// in key order; depth-first pre-order) with its index path.
pub fn for_each_node<'a>(doc: &'a SiteDocument, f: &mut impl FnMut(&'a Node, &str)) {
    for (idx, page) in doc.pages.iter().enumerate() {
        walk(&page.body, &format!("pages[{idx}].body"), f);
    }
    for (name, comp) in &doc.components {
        walk(&comp.template, &format!("components.{name}.template"), f);
    }
}

fn walk<'a>(nodes: &'a [Node], base: &str, f: &mut impl FnMut(&'a Node, &str)) {
    for (idx, node) in nodes.iter().enumerate() {
        let path = format!("{base}[{idx}]");
        f(node, &path);
        for (slot, list) in child_lists(node) {
            walk(list, &format!("{path}.{slot}"), f);
        }
    }
}

/// All `uid`s in a subtree (pre-order).
#[must_use]
pub fn subtree_uids(node: &Node) -> Vec<String> {
    let mut out = Vec::new();
    collect_uids(std::slice::from_ref(node), &mut out);
    out
}

fn collect_uids(nodes: &[Node], out: &mut Vec<String>) {
    for node in nodes {
        if let Some(uid) = node.uid() {
            out.push(uid.to_string());
        }
        for (_, list) in child_lists(node) {
            collect_uids(list, out);
        }
    }
}

fn marker_scheme(doc: &SiteDocument) -> Option<Option<u64>> {
    doc.metadata
        .get(MARKER_KEY)
        .map(|m| m.get("uid_scheme").and_then(Value::as_u64))
}

fn capped(mut paths: Vec<String>) -> Vec<String> {
    paths.truncate(MAX_REPORTED_PATHS);
    paths
}

/// Verify every node carries a valid `uid` and that all are unique across the
/// whole document. Pure uid check (does not need the marker).
pub fn check_uids(doc: &SiteDocument) -> Result<(), CommandError> {
    let mut missing = Vec::new();
    let mut invalid = Vec::new();
    let mut duplicate = Vec::new();
    let mut seen: BTreeMap<&str, String> = BTreeMap::new();
    for_each_node(doc, &mut |node, path| match node.uid() {
        None => missing.push(path.to_string()),
        Some(uid) if !is_valid_uid(uid) => invalid.push(format!("{path} ({uid})")),
        Some(uid) => {
            if let Some(first) = seen.get(uid) {
                duplicate.push(format!("{uid}: {first} and {path}"));
            } else {
                seen.insert(uid, path.to_string());
            }
        }
    });
    if !missing.is_empty() {
        return Err(CommandError::new(
            ErrorCode::MissingUid,
            format!(
                "{} node(s) have no uid; re-run the explicit stable-ID upgrade",
                missing.len()
            ),
        )
        .with_paths(capped(missing)));
    }
    if !invalid.is_empty() {
        return Err(CommandError::new(
            ErrorCode::InvalidUid,
            "uid must match ^[a-z0-9][a-z0-9._-]{0,63}$",
        )
        .with_paths(capped(invalid)));
    }
    if !duplicate.is_empty() {
        return Err(CommandError::new(
            ErrorCode::DuplicateUid,
            "uid values must be unique across the whole document",
        )
        .with_paths(capped(duplicate)));
    }
    Ok(())
}

/// The editability gate. A document may be edited only when it carries the
/// `sws.ir` marker, every node has a unique valid `uid`, editing-mode ID
/// uniqueness holds (styles, breakpoints) and `validate_ir` is clean.
pub fn check_editable(doc: &SiteDocument) -> Result<(), CommandError> {
    match marker_scheme(doc) {
        None => {
            return Err(CommandError::new(
                ErrorCode::NotUpgraded,
                "document has no stable IDs; run the explicit upgrade (assign stable ids) first",
            ));
        }
        Some(Some(UID_SCHEME)) => {}
        Some(_) => {
            return Err(CommandError::new(
                ErrorCode::UnsupportedUidScheme,
                format!("metadata[\"{MARKER_KEY}\"].uid_scheme must be {UID_SCHEME}"),
            ));
        }
    }
    check_uids(doc)?;

    let mut dup = Vec::new();
    let mut seen = BTreeSet::new();
    for (idx, rule) in doc.styles.iter().enumerate() {
        if !seen.insert(rule.id.as_str()) {
            dup.push(format!("styles[{idx}] ({})", rule.id));
        }
    }
    seen.clear();
    for (idx, bp) in doc.breakpoints.iter().enumerate() {
        if !seen.insert(bp.id.as_str()) {
            dup.push(format!("breakpoints[{idx}] ({})", bp.id));
        }
    }
    if !dup.is_empty() {
        return Err(CommandError::new(
            ErrorCode::DuplicateId,
            "style and breakpoint ids must be unique to edit a document",
        )
        .with_paths(capped(dup)));
    }

    let errors = validate_ir(doc);
    if !errors.is_empty() {
        return Err(CommandError::new(
            ErrorCode::InvalidDocument,
            format!("document fails IR validation ({} error(s))", errors.len()),
        )
        .with_paths(capped(
            errors
                .iter()
                .map(|e| format!("{}: {}", e.path, e.message))
                .collect(),
        )));
    }
    Ok(())
}

/// One ID assigned (or re-assigned) by the upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignedId {
    pub uid: String,
    pub path: String,
    /// `new`, `duplicate_uid` or `invalid_uid`.
    pub reason: String,
}

/// Reviewable result of [`assign_stable_ids`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeReport {
    pub uid_scheme: u64,
    pub assigned: Vec<AssignedId>,
    /// Nodes that already carried a valid unique `uid` and were left alone.
    pub kept: usize,
    pub marker_written: bool,
    /// `metadata["venue_manifest"]` is present: it is a stale copy once the IR is edited.
    pub venue_manifest_stale: bool,
}

fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in input.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(lower);
        } else {
            pending_dash = true;
        }
    }
    out
}

fn trim_uid(mut s: String, max: usize) -> String {
    s.truncate(max);
    while s.ends_with(['-', '.']) {
        s.pop();
    }
    s
}

fn label_for(node: &Node) -> String {
    let from_node = match node {
        Node::Element {
            id,
            tag,
            attrs,
            classes,
            ..
        } => attrs
            .get("id")
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .or_else(|| id.clone().filter(|v| !v.trim().is_empty()))
            .or_else(|| classes.first().cloned())
            .or_else(|| Some(tag.clone())),
        _ => None,
    };
    let slug = from_node.map(|s| slugify(&s)).unwrap_or_default();
    if slug.is_empty() {
        node.variant_label().to_string()
    } else {
        slug
    }
}

struct Assigner {
    taken: BTreeSet<String>,
    kept_seen: BTreeSet<String>,
    next_suffix: BTreeMap<String, usize>,
    assigned: Vec<AssignedId>,
    kept: usize,
}

impl Assigner {
    fn fresh(&mut self, base: &str) -> String {
        let base = if base.is_empty() { "node" } else { base };
        let plain = trim_uid(base.to_string(), MAX_UID_LEN);
        if !self.taken.contains(&plain) {
            self.taken.insert(plain.clone());
            return plain;
        }
        let counter = self.next_suffix.entry(plain.clone()).or_insert(2);
        loop {
            let suffix = format!("-{counter}");
            let candidate = format!(
                "{}{suffix}",
                trim_uid(base.to_string(), MAX_UID_LEN - suffix.len())
            );
            *counter += 1;
            if !self.taken.contains(&candidate) {
                self.taken.insert(candidate.clone());
                return candidate;
            }
        }
    }

    fn visit(&mut self, nodes: &mut [Node], base: &str, scope: &str) {
        for (idx, node) in nodes.iter_mut().enumerate() {
            let path = format!("{base}[{idx}]");
            let reason = match node.uid() {
                Some(uid) if is_valid_uid(uid) => {
                    if self.kept_seen.insert(uid.to_string()) {
                        self.kept += 1;
                        None
                    } else {
                        Some("duplicate_uid")
                    }
                }
                Some(_) => Some("invalid_uid"),
                None => Some("new"),
            };
            if let Some(reason) = reason {
                let uid = self.fresh(&format!("{scope}.{}", label_for(node)));
                node.set_uid(Some(uid.clone()));
                self.assigned.push(AssignedId {
                    uid,
                    path: path.clone(),
                    reason: reason.to_string(),
                });
            }
            for (slot, list) in child_lists_mut(node) {
                self.visit(list, &format!("{path}.{slot}"), scope);
            }
        }
    }
}

/// Explicit, deterministic, idempotent stable-ID upgrade.
///
/// Adds `uid` to every node that lacks a valid unique one (document order:
/// pages in array order, depth-first pre-order, then components in key order),
/// writes `metadata["sws.ir"] = {"uid_scheme": 1}` and changes nothing else.
/// Candidate IDs are `{page-id-slug}.{label}` (`component-{name}` scope for
/// templates), de-duplicated with `-2`, `-3`, ... in traversal order.
pub fn assign_stable_ids(
    doc: &SiteDocument,
) -> Result<(SiteDocument, UpgradeReport), CommandError> {
    if let Some(scheme) = marker_scheme(doc)
        && scheme != Some(UID_SCHEME)
    {
        return Err(CommandError::new(
            ErrorCode::UnsupportedUidScheme,
            format!("metadata[\"{MARKER_KEY}\"].uid_scheme must be {UID_SCHEME}"),
        ));
    }
    let mut out = doc.clone();

    let mut taken = BTreeSet::new();
    for_each_node(doc, &mut |node, _| {
        if let Some(uid) = node.uid()
            && is_valid_uid(uid)
        {
            taken.insert(uid.to_string());
        }
    });
    let mut assigner = Assigner {
        taken,
        kept_seen: BTreeSet::new(),
        next_suffix: BTreeMap::new(),
        assigned: Vec::new(),
        kept: 0,
    };

    for (idx, page) in out.pages.iter_mut().enumerate() {
        let scope = match slugify(&page.id) {
            s if s.is_empty() => "page".to_string(),
            s => trim_uid(s, 30),
        };
        assigner.visit(&mut page.body, &format!("pages[{idx}].body"), &scope);
    }
    for (name, comp) in &mut out.components {
        let scope = trim_uid(format!("component-{}", slugify(name)), 30);
        assigner.visit(
            &mut comp.template,
            &format!("components.{name}.template"),
            &scope,
        );
    }

    let marker = json!({ "uid_scheme": UID_SCHEME });
    let marker_written = out.metadata.get(MARKER_KEY) != Some(&marker);
    out.metadata.insert(MARKER_KEY.to_string(), marker);

    let report = UpgradeReport {
        uid_scheme: UID_SCHEME,
        assigned: assigner.assigned,
        kept: assigner.kept,
        marker_written,
        venue_manifest_stale: out.metadata.contains_key("venue_manifest"),
    };
    Ok((out, report))
}
