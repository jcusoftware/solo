//! The canonical, pure, atomic command reducer.
//!
//! [`apply_batch`] never mutates its input: it applies every command to a
//! working copy, re-checks the editability invariants and only then returns the
//! new document together with the exact inverse and the touched fields.

use std::collections::BTreeSet;

use super::command::{Command, CommandBatch, CommandError, ContainerRef, ErrorCode, Transaction};
use super::ids::{
    check_editable, child_lists, child_lists_mut, for_each_node, is_valid_uid, subtree_uids,
};
use crate::patch::{IrError, IrPatch, apply_patch};
use crate::types::{Node, SiteDocument};

type Applied = (Command, Vec<String>);

/// How strictly command inputs are validated.
///
/// `Strict` is for new input (user batches, redo): every value passes the
/// grammar checks of the legacy patch engine and the reducer. `Restore` is for
/// replaying a recorded inverse: it must be able to put back *any* value the
/// document already held (including legacy-shaped values such as an empty
/// optional font stack or an SVG `clipPath` tag), so the new-input grammar
/// checks are skipped. The structural and whole-document checks
/// ([`check_editable`], uid freshness, container resolution) always run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Strict,
    Restore,
}

/// Apply `batch` atomically to a copy of `doc`.
///
/// On success returns the new document and the [`Transaction`] (forward
/// commands, inverse in application order, sorted touched fields). On any error
/// the input is untouched by construction and the error names the failing
/// command index where one applies.
pub fn apply_batch(
    doc: &SiteDocument,
    batch: &CommandBatch,
) -> Result<(SiteDocument, Transaction), CommandError> {
    apply_batch_with(doc, batch, Mode::Strict)
}

/// Apply a recorded inverse (undo) batch: like [`apply_batch`] but the
/// new-input grammar checks are skipped so any previously held value can be
/// restored. The whole-document gate still runs after the batch and the input
/// is untouched on error. Use this (not [`apply_batch`]) to replay
/// `Transaction::inverse`.
pub fn apply_batch_restore(
    doc: &SiteDocument,
    batch: &CommandBatch,
) -> Result<(SiteDocument, Transaction), CommandError> {
    apply_batch_with(doc, batch, Mode::Restore)
}

fn apply_batch_with(
    doc: &SiteDocument,
    batch: &CommandBatch,
    mode: Mode,
) -> Result<(SiteDocument, Transaction), CommandError> {
    batch.validate()?;
    check_editable(doc)?;
    let mut work = doc.clone();
    let mut inverse = Vec::with_capacity(batch.commands.len());
    let mut touched = BTreeSet::new();
    for (idx, command) in batch.commands.iter().enumerate() {
        let (inv, fields) = apply_command(&mut work, command, mode).map_err(|e| e.at(idx))?;
        inverse.push(inv);
        touched.extend(fields);
    }
    inverse.reverse();
    check_editable(&work).map_err(|mut e| {
        e.message = format!("batch result is invalid: {}", e.message);
        e
    })?;
    if mode == Mode::Strict {
        verify_restorable(doc, &work, &inverse)?;
    }
    Ok((
        work,
        Transaction {
            version: batch.version,
            label: batch.label.clone(),
            coalesce_key: batch.coalesce_key.clone(),
            forward: batch.commands.clone(),
            inverse,
            touched: touched.into_iter().collect(),
        },
    ))
}

/// Safety net: replaying the recorded inverse on the result must reproduce the
/// input document exactly. Refuses the batch otherwise, so a transaction that
/// could not be undone is never produced.
fn verify_restorable(
    before: &SiteDocument,
    after: &SiteDocument,
    inverse: &[Command],
) -> Result<(), CommandError> {
    let unrestorable = |detail: String| {
        err(
            ErrorCode::InvalidValue,
            format!("edit refused: it could not be undone exactly ({detail})"),
        )
    };
    let mut probe = after.clone();
    for (idx, command) in inverse.iter().enumerate() {
        apply_command(&mut probe, command, Mode::Restore)
            .map_err(|e| unrestorable(format!("inverse command {idx}: {e}")))?;
    }
    check_editable(&probe).map_err(|e| unrestorable(e.to_string()))?;
    if probe != *before {
        return Err(unrestorable("inverse does not reproduce the input".into()));
    }
    Ok(())
}

/// Like [`apply_batch`] but replaces `doc` on success (and only then).
pub fn apply_batch_in_place(
    doc: &mut SiteDocument,
    batch: &CommandBatch,
) -> Result<Transaction, CommandError> {
    let (next, tx) = apply_batch(doc, batch)?;
    *doc = next;
    Ok(tx)
}

fn err(code: ErrorCode, message: impl Into<String>) -> CommandError {
    CommandError::new(code, message)
}

fn not_found(what: &str, id: &str) -> CommandError {
    err(ErrorCode::NotFound, format!("{what} '{id}' not found")).with_paths(vec![id.to_string()])
}

fn from_ir(e: IrError) -> CommandError {
    match e {
        IrError::PageNotFound { page_id } => not_found("page", &page_id),
        IrError::NodeNotFound { node_id, .. } => not_found("node", &node_id),
        other => err(ErrorCode::InvalidValue, other.to_string()),
    }
}

/// Apply one `IrPatch` through the legacy validators (`Strict`) or assign the
/// value directly (`Restore`).
fn patch(doc: &mut SiteDocument, mode: Mode, op: IrPatch) -> Result<(), IrError> {
    if mode == Mode::Strict {
        return apply_patch(doc, &op);
    }
    match op {
        IrPatch::SetSiteMeta {
            name,
            locale,
            base_url,
        } => {
            if let Some(name) = name {
                doc.name = name;
            }
            if let Some(locale) = locale {
                doc.locale = locale;
            }
            if let Some(base_url) = base_url {
                doc.base_url = base_url;
            }
            Ok(())
        }
        IrPatch::SetColorToken { key, hex } => {
            doc.tokens.colors.insert(key, hex);
            Ok(())
        }
        IrPatch::SetFontToken { slot, stack } => match slot.as_str() {
            "body" => {
                doc.tokens.fonts.body = stack;
                Ok(())
            }
            "heading" => {
                doc.tokens.fonts.heading = Some(stack);
                Ok(())
            }
            "mono" => {
                doc.tokens.fonts.mono = Some(stack);
                Ok(())
            }
            _ => Err(IrError::InvalidFontSlot { slot }),
        },
        other => apply_patch(doc, &other),
    }
}

fn apply_command(
    doc: &mut SiteDocument,
    command: &Command,
    mode: Mode,
) -> Result<Applied, CommandError> {
    match command {
        Command::SetSiteMeta {
            name,
            locale,
            base_url,
        } => {
            if name.is_none() && locale.is_none() && base_url.is_none() {
                return Err(err(
                    ErrorCode::InvalidValue,
                    "set_site_meta needs at least one of name, locale, base_url",
                ));
            }
            let inverse = Command::SetSiteMeta {
                name: name.as_ref().map(|_| doc.name.clone()),
                locale: locale.as_ref().map(|_| doc.locale.clone()),
                base_url: base_url.as_ref().map(|_| doc.base_url.clone()),
            };
            patch(
                doc,
                mode,
                IrPatch::SetSiteMeta {
                    name: name.clone(),
                    locale: locale.clone(),
                    base_url: base_url.clone(),
                },
            )
            .map_err(from_ir)?;
            let mut touched = Vec::new();
            for (field, present) in [
                ("name", name.is_some()),
                ("locale", locale.is_some()),
                ("base_url", base_url.is_some()),
            ] {
                if present {
                    touched.push(format!("site/{field}"));
                }
            }
            Ok((inverse, touched))
        }
        Command::SetColorToken { key, hex } => {
            let previous = doc.tokens.colors.get(key).cloned();
            patch(
                doc,
                mode,
                IrPatch::SetColorToken {
                    key: key.clone(),
                    hex: hex.clone(),
                },
            )
            .map_err(from_ir)?;
            let inverse = match previous {
                Some(hex) => Command::SetColorToken {
                    key: key.clone(),
                    hex,
                },
                None => Command::RemoveColorToken { key: key.clone() },
            };
            Ok((inverse, vec![format!("tokens/colors/{key}")]))
        }
        Command::RemoveColorToken { key } => {
            let Some(previous) = doc.tokens.colors.remove(key) else {
                return Err(not_found("color token", key));
            };
            Ok((
                Command::SetColorToken {
                    key: key.clone(),
                    hex: previous,
                },
                vec![format!("tokens/colors/{key}")],
            ))
        }
        Command::SetFontToken { slot, stack } => {
            let inverse = match slot.as_str() {
                "body" => Command::SetFontToken {
                    slot: slot.clone(),
                    stack: doc.tokens.fonts.body.clone(),
                },
                "heading" => font_inverse(slot, doc.tokens.fonts.heading.clone()),
                "mono" => font_inverse(slot, doc.tokens.fonts.mono.clone()),
                _ => {
                    return Err(err(
                        ErrorCode::InvalidValue,
                        format!(
                            "invalid font slot '{slot}'; expected 'body', 'heading', or 'mono'"
                        ),
                    ));
                }
            };
            patch(
                doc,
                mode,
                IrPatch::SetFontToken {
                    slot: slot.clone(),
                    stack: stack.clone(),
                },
            )
            .map_err(from_ir)?;
            Ok((inverse, vec![format!("tokens/fonts/{slot}")]))
        }
        Command::ClearFontToken { slot } => {
            let previous = match slot.as_str() {
                "heading" => doc.tokens.fonts.heading.take(),
                "mono" => doc.tokens.fonts.mono.take(),
                _ => {
                    return Err(err(
                        ErrorCode::InvalidValue,
                        format!(
                            "font slot '{slot}' cannot be cleared; expected 'heading' or 'mono'"
                        ),
                    ));
                }
            };
            Ok((
                font_inverse(slot, previous),
                vec![format!("tokens/fonts/{slot}")],
            ))
        }
        Command::SetDataValue { key, value } => {
            let previous = doc.data.get(key).cloned();
            patch(
                doc,
                mode,
                IrPatch::SetDataValue {
                    key: key.clone(),
                    value: value.clone(),
                },
            )
            .map_err(from_ir)?;
            let inverse = match previous {
                Some(value) => Command::SetDataValue {
                    key: key.clone(),
                    value,
                },
                None => Command::RemoveDataValue { key: key.clone() },
            };
            Ok((inverse, vec![format!("data/{key}")]))
        }
        Command::RemoveDataValue { key } => {
            let Some(previous) = doc.data.remove(key) else {
                return Err(not_found("data key", key));
            };
            Ok((
                Command::SetDataValue {
                    key: key.clone(),
                    value: previous,
                },
                vec![format!("data/{key}")],
            ))
        }
        Command::UpsertStyleRule { rule } => {
            let previous = doc.styles.iter().find(|r| r.id == rule.id).cloned();
            patch(doc, mode, IrPatch::UpsertStyleRule { rule: rule.clone() }).map_err(from_ir)?;
            let mut touched = vec![format!("style:{}", rule.id)];
            let inverse = match previous {
                Some(rule) => Command::UpsertStyleRule { rule },
                None => {
                    touched.push("styles".to_string());
                    Command::RemoveStyleRule {
                        id: rule.id.clone(),
                    }
                }
            };
            Ok((inverse, touched))
        }
        Command::InsertStyleRule { rule, index } => {
            if doc.styles.iter().any(|r| r.id == rule.id) {
                return Err(err(
                    ErrorCode::AlreadyExists,
                    format!("style rule '{}' already exists", rule.id),
                )
                .with_paths(vec![rule.id.clone()]));
            }
            let at = checked_index(*index, doc.styles.len(), "styles")?;
            doc.styles.insert(at, rule.clone());
            Ok((
                Command::RemoveStyleRule {
                    id: rule.id.clone(),
                },
                vec![format!("style:{}", rule.id), "styles".to_string()],
            ))
        }
        Command::RemoveStyleRule { id } => {
            let Some(pos) = doc.styles.iter().position(|r| r.id == *id) else {
                return Err(not_found("style rule", id));
            };
            let rule = doc.styles.remove(pos);
            Ok((
                Command::InsertStyleRule {
                    rule,
                    index: Some(pos),
                },
                vec![format!("style:{id}"), "styles".to_string()],
            ))
        }
        Command::InsertPage { page, index } => {
            if doc.pages.iter().any(|p| p.id == page.id) {
                return Err(err(
                    ErrorCode::AlreadyExists,
                    format!("page '{}' already exists", page.id),
                )
                .with_paths(vec![page.id.clone()]));
            }
            check_fresh(doc, &page.body)?;
            let at = checked_index(*index, doc.pages.len(), "pages")?;
            doc.pages.insert(at, page.clone());
            let mut touched = vec![format!("page:{}", page.id), "pages".to_string()];
            touched.extend(page_node_fields(&page.body));
            Ok((
                Command::RemovePage {
                    id: page.id.clone(),
                },
                touched,
            ))
        }
        Command::UpsertPage { page } => {
            let mut touched = vec![format!("page:{}", page.id)];
            if let Some(pos) = doc.pages.iter().position(|p| p.id == page.id) {
                let old = doc.pages.remove(pos);
                check_fresh(doc, &page.body)?;
                touched.extend(page_node_fields(&old.body));
                touched.extend(page_node_fields(&page.body));
                doc.pages.insert(pos, page.clone());
                Ok((Command::UpsertPage { page: old }, touched))
            } else {
                check_fresh(doc, &page.body)?;
                touched.push("pages".to_string());
                touched.extend(page_node_fields(&page.body));
                doc.pages.push(page.clone());
                Ok((
                    Command::RemovePage {
                        id: page.id.clone(),
                    },
                    touched,
                ))
            }
        }
        Command::RemovePage { id } => {
            let Some(pos) = doc.pages.iter().position(|p| p.id == *id) else {
                return Err(not_found("page", id));
            };
            let page = doc.pages.remove(pos);
            let mut touched = vec![format!("page:{id}"), "pages".to_string()];
            touched.extend(page_node_fields(&page.body));
            Ok((
                Command::InsertPage {
                    page,
                    index: Some(pos),
                },
                touched,
            ))
        }
        Command::MovePage { id, index } => {
            let Some(pos) = doc.pages.iter().position(|p| p.id == *id) else {
                return Err(not_found("page", id));
            };
            let page = doc.pages.remove(pos);
            if *index > doc.pages.len() {
                return Err(err(
                    ErrorCode::InvalidIndex,
                    format!("page index {index} out of bounds (max {})", doc.pages.len()),
                ));
            }
            doc.pages.insert(*index, page);
            Ok((
                Command::MovePage {
                    id: id.clone(),
                    index: pos,
                },
                vec!["pages".to_string()],
            ))
        }
        Command::InsertNode {
            container,
            index,
            node,
        } => {
            let container = normalize_container(doc, container)?;
            check_fresh(doc, std::slice::from_ref(node))?;
            let list = container_mut(doc, &container)?;
            let at = checked_index(*index, list.len(), "container")?;
            list.insert(at, node.clone());
            let mut touched = vec![container_path(&container)];
            touched.extend(subtree_uids(node).into_iter().map(|u| format!("node:{u}")));
            let uid = node.uid().unwrap_or_default().to_string();
            Ok((Command::RemoveNode { uid }, touched))
        }
        Command::RemoveNode { uid } => {
            let (container, pos) = locate(doc, uid).ok_or_else(|| not_found("node", uid))?;
            let list = container_mut(doc, &container)?;
            let node = list.remove(pos);
            let mut touched = vec![container_path(&container)];
            touched.extend(subtree_uids(&node).into_iter().map(|u| format!("node:{u}")));
            Ok((
                Command::InsertNode {
                    container,
                    index: Some(pos),
                    node,
                },
                touched,
            ))
        }
        Command::MoveNode {
            uid,
            container,
            index,
        } => {
            let (old_container, old_pos) =
                locate(doc, uid).ok_or_else(|| not_found("node", uid))?;
            let dest = normalize_container(doc, container)?;
            if let ContainerRef::Node { uid: owner, .. } = &dest {
                let subtree = find_node(doc, uid).map(subtree_uids).unwrap_or_default();
                if subtree.iter().any(|u| u == owner) {
                    return Err(err(
                        ErrorCode::MoveIntoSelf,
                        format!("cannot move node '{uid}' into its own subtree"),
                    )
                    .with_paths(vec![uid.clone(), owner.clone()]));
                }
            }
            let node = container_mut(doc, &old_container)?.remove(old_pos);
            let list = container_mut(doc, &dest)?;
            let at = checked_index(*index, list.len(), "container")?;
            list.insert(at, node);
            Ok((
                Command::MoveNode {
                    uid: uid.clone(),
                    container: old_container.clone(),
                    index: Some(old_pos),
                },
                vec![
                    container_path(&old_container),
                    container_path(&dest),
                    format!("node:{uid}"),
                ],
            ))
        }
        Command::SetAttr { uid, attr, value } => {
            if mode == Mode::Strict && !is_valid_attr_name(attr) {
                return Err(err(
                    ErrorCode::InvalidValue,
                    format!("invalid attribute name '{attr}'"),
                ));
            }
            let node = find_node_mut(doc, uid).ok_or_else(|| not_found("node", uid))?;
            let Node::Element { attrs, .. } = node else {
                return Err(wrong_kind(uid, "element"));
            };
            let previous = match value {
                Some(v) => attrs.insert(attr.clone(), v.clone()),
                None => attrs.remove(attr),
            };
            Ok((
                Command::SetAttr {
                    uid: uid.clone(),
                    attr: attr.clone(),
                    value: previous,
                },
                vec![format!("node:{uid}/attrs/{attr}")],
            ))
        }
        Command::SetText { uid, value } => {
            let node = find_node_mut(doc, uid).ok_or_else(|| not_found("node", uid))?;
            let Node::Text { value: text, .. } = node else {
                return Err(wrong_kind(uid, "text"));
            };
            let previous = std::mem::replace(text, value.clone());
            Ok((
                Command::SetText {
                    uid: uid.clone(),
                    value: previous,
                },
                vec![format!("node:{uid}/text")],
            ))
        }
        Command::SetTag { uid, tag } => {
            if mode == Mode::Strict && !is_valid_tag(tag) {
                return Err(err(
                    ErrorCode::InvalidValue,
                    format!("invalid element tag '{tag}'"),
                ));
            }
            let node = find_node_mut(doc, uid).ok_or_else(|| not_found("node", uid))?;
            let Node::Element { tag: current, .. } = node else {
                return Err(wrong_kind(uid, "element"));
            };
            let previous = std::mem::replace(current, tag.clone());
            Ok((
                Command::SetTag {
                    uid: uid.clone(),
                    tag: previous,
                },
                vec![format!("node:{uid}/tag")],
            ))
        }
        Command::SetClasses { uid, classes } => {
            let node = find_node_mut(doc, uid).ok_or_else(|| not_found("node", uid))?;
            let Node::Element {
                classes: current, ..
            } = node
            else {
                return Err(wrong_kind(uid, "element"));
            };
            let previous = std::mem::replace(current, classes.clone());
            Ok((
                Command::SetClasses {
                    uid: uid.clone(),
                    classes: previous,
                },
                vec![format!("node:{uid}/classes")],
            ))
        }
        Command::SetStyleRefs { uid, style_refs } => {
            let node = find_node_mut(doc, uid).ok_or_else(|| not_found("node", uid))?;
            let Node::Element {
                style_refs: current,
                ..
            } = node
            else {
                return Err(wrong_kind(uid, "element"));
            };
            let previous = std::mem::replace(current, style_refs.clone());
            Ok((
                Command::SetStyleRefs {
                    uid: uid.clone(),
                    style_refs: previous,
                },
                vec![format!("node:{uid}/style_refs")],
            ))
        }
    }
}

fn font_inverse(slot: &str, previous: Option<String>) -> Command {
    match previous {
        Some(stack) => Command::SetFontToken {
            slot: slot.to_string(),
            stack,
        },
        None => Command::ClearFontToken {
            slot: slot.to_string(),
        },
    }
}

fn wrong_kind(uid: &str, expected: &str) -> CommandError {
    err(
        ErrorCode::WrongNodeKind,
        format!("node '{uid}' is not a {expected} node"),
    )
    .with_paths(vec![uid.to_string()])
}

fn checked_index(index: Option<usize>, len: usize, what: &str) -> Result<usize, CommandError> {
    let at = index.unwrap_or(len);
    if at > len {
        return Err(err(
            ErrorCode::InvalidIndex,
            format!("index {at} out of bounds for {what} (len {len})"),
        ));
    }
    Ok(at)
}

fn is_valid_attr_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.'))
}

/// Same character set the compiler accepts for element tags
/// (`sws-compiler` `is_safe_tag_name`: ASCII alphanumerics, `-`, `_`), with a
/// leading letter so SVG tags such as `clipPath` and `linearGradient` work.
fn is_valid_tag(tag: &str) -> bool {
    let mut bytes = tag.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

fn page_node_fields(body: &[Node]) -> Vec<String> {
    let mut out = Vec::new();
    for node in body {
        out.extend(subtree_uids(node).into_iter().map(|u| format!("node:{u}")));
    }
    out
}

/// Check that every node of `nodes` has a valid, intra-unique `uid` that is
/// not already used anywhere in `doc`.
fn check_fresh(doc: &SiteDocument, nodes: &[Node]) -> Result<(), CommandError> {
    let mut existing = BTreeSet::new();
    for_each_node(doc, &mut |n, _| {
        if let Some(uid) = n.uid() {
            existing.insert(uid.to_string());
        }
    });
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&Node> = nodes.iter().rev().collect();
    while let Some(node) = stack.pop() {
        let Some(uid) = node.uid() else {
            return Err(err(
                ErrorCode::MissingUid,
                "every inserted node must carry a uid",
            ));
        };
        if !is_valid_uid(uid) {
            return Err(err(
                ErrorCode::InvalidUid,
                format!("uid '{uid}' must match ^[a-z0-9][a-z0-9._-]{{0,63}}$"),
            )
            .with_paths(vec![uid.to_string()]));
        }
        if existing.contains(uid) || !seen.insert(uid.to_string()) {
            return Err(err(
                ErrorCode::DuplicateUid,
                format!("uid '{uid}' already exists in the document"),
            )
            .with_paths(vec![uid.to_string()]));
        }
        for (_, list) in child_lists(node).into_iter().rev() {
            stack.extend(list.iter().rev());
        }
    }
    Ok(())
}

fn find_node<'a>(doc: &'a SiteDocument, uid: &str) -> Option<&'a Node> {
    let mut found = None;
    for_each_node(doc, &mut |n, _| {
        if found.is_none() && n.uid() == Some(uid) {
            found = Some(n);
        }
    });
    found
}

fn find_node_mut<'a>(doc: &'a mut SiteDocument, uid: &str) -> Option<&'a mut Node> {
    for page in &mut doc.pages {
        if let Some(found) = find_in(&mut page.body, uid) {
            return Some(found);
        }
    }
    for comp in doc.components.values_mut() {
        if let Some(found) = find_in(&mut comp.template, uid) {
            return Some(found);
        }
    }
    None
}

fn find_in<'a>(nodes: &'a mut [Node], uid: &str) -> Option<&'a mut Node> {
    for node in nodes {
        if node.uid() == Some(uid) {
            return Some(node);
        }
        for (_, list) in child_lists_mut(node) {
            if let Some(found) = find_in(list, uid) {
                return Some(found);
            }
        }
    }
    None
}

/// Find the container list and index holding the node `uid`.
fn locate(doc: &SiteDocument, uid: &str) -> Option<(ContainerRef, usize)> {
    for page in &doc.pages {
        let owner = ContainerRef::PageBody {
            page_id: page.id.clone(),
        };
        if let Some(found) = locate_in(&page.body, &owner, uid) {
            return Some(found);
        }
    }
    for (name, comp) in &doc.components {
        let owner = ContainerRef::ComponentTemplate { name: name.clone() };
        if let Some(found) = locate_in(&comp.template, &owner, uid) {
            return Some(found);
        }
    }
    None
}

fn locate_in(nodes: &[Node], owner: &ContainerRef, uid: &str) -> Option<(ContainerRef, usize)> {
    for (idx, node) in nodes.iter().enumerate() {
        if node.uid() == Some(uid) {
            return Some((owner.clone(), idx));
        }
        let Some(node_uid) = node.uid() else { continue };
        for (slot, list) in child_lists(node) {
            let child_owner = ContainerRef::Node {
                uid: node_uid.to_string(),
                slot: Some(slot),
            };
            if let Some(found) = locate_in(list, &child_owner, uid) {
                return Some(found);
            }
        }
    }
    None
}

fn default_slot(node: &Node) -> Option<&'static str> {
    match node {
        Node::Element { .. } => Some("children"),
        Node::SlotRef { .. } => Some("fallback"),
        Node::Loop { .. } => Some("body"),
        _ => None,
    }
}

/// Resolve the default slot and verify the container exists.
fn normalize_container(
    doc: &SiteDocument,
    container: &ContainerRef,
) -> Result<ContainerRef, CommandError> {
    match container {
        ContainerRef::PageBody { page_id } => {
            if doc.pages.iter().any(|p| p.id == *page_id) {
                Ok(container.clone())
            } else {
                Err(not_found("page", page_id))
            }
        }
        ContainerRef::ComponentTemplate { name } => {
            if doc.components.contains_key(name) {
                Ok(container.clone())
            } else {
                Err(not_found("component", name))
            }
        }
        ContainerRef::Node { uid, slot } => {
            let node = find_node(doc, uid).ok_or_else(|| not_found("node", uid))?;
            let slot = match slot {
                Some(s) => s.clone(),
                None => default_slot(node)
                    .ok_or_else(|| {
                        err(
                            ErrorCode::InvalidContainer,
                            format!("node '{uid}' needs an explicit slot"),
                        )
                    })?
                    .to_string(),
            };
            if !child_lists(node).iter().any(|(name, _)| *name == slot) {
                return Err(err(
                    ErrorCode::InvalidContainer,
                    format!("node '{uid}' has no child list '{slot}'"),
                )
                .with_paths(vec![uid.clone()]));
            }
            Ok(ContainerRef::Node {
                uid: uid.clone(),
                slot: Some(slot),
            })
        }
    }
}

/// Resolve a *normalized* container to its list.
fn container_mut<'a>(
    doc: &'a mut SiteDocument,
    container: &ContainerRef,
) -> Result<&'a mut Vec<Node>, CommandError> {
    match container {
        ContainerRef::PageBody { page_id } => doc
            .pages
            .iter_mut()
            .find(|p| p.id == *page_id)
            .map(|p| &mut p.body)
            .ok_or_else(|| not_found("page", page_id)),
        ContainerRef::ComponentTemplate { name } => doc
            .components
            .get_mut(name)
            .map(|c| &mut c.template)
            .ok_or_else(|| not_found("component", name)),
        ContainerRef::Node { uid, slot } => {
            let slot = slot.clone().unwrap_or_default();
            let node = find_node_mut(doc, uid).ok_or_else(|| not_found("node", uid))?;
            child_lists_mut(node)
                .into_iter()
                .find(|(name, _)| *name == slot)
                .map(|(_, list)| list)
                .ok_or_else(|| {
                    err(
                        ErrorCode::InvalidContainer,
                        format!("node '{uid}' has no child list '{slot}'"),
                    )
                })
        }
    }
}

fn container_path(container: &ContainerRef) -> String {
    match container {
        ContainerRef::PageBody { page_id } => format!("page:{page_id}/body"),
        ContainerRef::ComponentTemplate { name } => format!("component:{name}/template"),
        ContainerRef::Node { uid, slot } => {
            format!("node:{uid}/{}", slot.as_deref().unwrap_or("children"))
        }
    }
}
