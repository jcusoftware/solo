//! Deterministic AST/document patching for [`SiteDocument`].

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{IrStyleRule, Node, Page, SiteDocument};
use crate::validate::is_valid_hex_color;

/// Errors returned when applying an [`IrPatch`] to a [`SiteDocument`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrError {
    PageNotFound {
        page_id: String,
    },
    NodeNotFound {
        page_id: String,
        node_id: String,
    },
    InvalidFontSlot {
        slot: String,
    },
    InvalidIndex {
        parent_id: Option<String>,
        index: usize,
        len: usize,
    },
    InvalidValue {
        field: String,
        message: String,
    },
}

impl fmt::Display for IrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PageNotFound { page_id } => {
                write!(f, "page '{page_id}' not found")
            }
            Self::NodeNotFound { page_id, node_id } => {
                write!(f, "node '{node_id}' not found on page '{page_id}'")
            }
            Self::InvalidFontSlot { slot } => {
                write!(
                    f,
                    "invalid font slot '{slot}'; expected 'body', 'heading', or 'mono'"
                )
            }
            Self::InvalidIndex {
                parent_id,
                index,
                len,
            } => match parent_id {
                Some(pid) => write!(
                    f,
                    "insertion index {index} out of bounds for parent '{pid}' (len {len})"
                ),
                None => write!(
                    f,
                    "insertion index {index} out of bounds for page body (len {len})"
                ),
            },
            Self::InvalidValue { field, message } => {
                write!(f, "invalid value for '{field}': {message}")
            }
        }
    }
}

impl std::error::Error for IrError {}

/// Declarative patch operation applied to a [`SiteDocument`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IrPatch {
    SetSiteMeta {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        locale: Option<String>,
        #[serde(default)]
        base_url: Option<String>,
    },
    SetColorToken {
        key: String,
        hex: String,
    },
    SetFontToken {
        slot: String,
        stack: String,
    },
    UpsertPage {
        page: Page,
    },
    RemovePage {
        id: String,
    },
    SetNodeText {
        page_id: String,
        node_id: String,
        text: String,
    },
    SetNodeAttr {
        page_id: String,
        node_id: String,
        attr: String,
        #[serde(default)]
        value: Option<String>,
    },
    InsertChildNode {
        page_id: String,
        #[serde(default)]
        parent_id: Option<String>,
        #[serde(default)]
        index: Option<usize>,
        node: Node,
    },
    RemoveNode {
        page_id: String,
        node_id: String,
    },
    UpsertStyleRule {
        rule: IrStyleRule,
    },
    SetDataValue {
        key: String,
        value: Value,
    },
}

/// Apply a single [`IrPatch`] mutation to a [`SiteDocument`].
pub fn apply_patch(doc: &mut SiteDocument, patch: &IrPatch) -> Result<(), IrError> {
    match patch {
        IrPatch::SetSiteMeta {
            name,
            locale,
            base_url,
        } => {
            if let Some(name) = name {
                if name.trim().is_empty() {
                    return Err(IrError::InvalidValue {
                        field: "name".to_string(),
                        message: "site name must not be empty".to_string(),
                    });
                }
                doc.name = name.clone();
            }
            if let Some(locale) = locale {
                if locale.trim().is_empty() {
                    return Err(IrError::InvalidValue {
                        field: "locale".to_string(),
                        message: "locale must not be empty".to_string(),
                    });
                }
                doc.locale = locale.clone();
            }
            if let Some(base_url) = base_url {
                if !base_url.starts_with("https://") {
                    return Err(IrError::InvalidValue {
                        field: "base_url".to_string(),
                        message: "base_url must start with https://".to_string(),
                    });
                }
                doc.base_url = base_url.clone();
            }
            Ok(())
        }
        IrPatch::SetColorToken { key, hex } => {
            if key.trim().is_empty() {
                return Err(IrError::InvalidValue {
                    field: "key".to_string(),
                    message: "color token key must not be empty".to_string(),
                });
            }
            if !is_valid_hex_color(hex) {
                return Err(IrError::InvalidValue {
                    field: "hex".to_string(),
                    message: format!("color '{hex}' must be formatted as #RRGGBB"),
                });
            }
            doc.tokens.colors.insert(key.clone(), hex.clone());
            Ok(())
        }
        IrPatch::SetFontToken { slot, stack } => {
            if stack.trim().is_empty() {
                return Err(IrError::InvalidValue {
                    field: "stack".to_string(),
                    message: "font stack must not be empty".to_string(),
                });
            }
            match slot.as_str() {
                "body" => doc.tokens.fonts.body = stack.clone(),
                "heading" => doc.tokens.fonts.heading = Some(stack.clone()),
                "mono" => doc.tokens.fonts.mono = Some(stack.clone()),
                _ => return Err(IrError::InvalidFontSlot { slot: slot.clone() }),
            }
            Ok(())
        }
        IrPatch::UpsertPage { page } => {
            if let Some(existing) = doc.pages.iter_mut().find(|p| p.id == page.id) {
                *existing = page.clone();
            } else {
                doc.pages.push(page.clone());
            }
            Ok(())
        }
        IrPatch::RemovePage { id } => {
            let Some(pos) = doc.pages.iter().position(|p| p.id == *id) else {
                return Err(IrError::PageNotFound {
                    page_id: id.clone(),
                });
            };
            doc.pages.remove(pos);
            Ok(())
        }
        IrPatch::SetNodeText {
            page_id,
            node_id,
            text,
        } => {
            let page = find_page_mut(doc, page_id)?;
            let Node::Element { children, .. } = find_element_mut(&mut page.body, node_id)
                .ok_or_else(|| IrError::NodeNotFound {
                    page_id: page_id.clone(),
                    node_id: node_id.clone(),
                })?
            else {
                unreachable!("find_element_mut only returns Node::Element");
            };
            *children = vec![Node::Text {
                value: text.clone(),
            }];
            Ok(())
        }
        IrPatch::SetNodeAttr {
            page_id,
            node_id,
            attr,
            value,
        } => {
            let page = find_page_mut(doc, page_id)?;
            let Node::Element { attrs, .. } = find_element_mut(&mut page.body, node_id)
                .ok_or_else(|| IrError::NodeNotFound {
                    page_id: page_id.clone(),
                    node_id: node_id.clone(),
                })?
            else {
                unreachable!("find_element_mut only returns Node::Element");
            };
            match value {
                Some(v) => {
                    attrs.insert(attr.clone(), v.clone());
                }
                None => {
                    attrs.remove(attr);
                }
            }
            Ok(())
        }
        IrPatch::InsertChildNode {
            page_id,
            parent_id,
            index,
            node,
        } => {
            let page = find_page_mut(doc, page_id)?;
            let target_list = match parent_id {
                None => &mut page.body,
                Some(pid) => {
                    let Node::Element { children, .. } = find_element_mut(&mut page.body, pid)
                        .ok_or_else(|| IrError::NodeNotFound {
                            page_id: page_id.clone(),
                            node_id: pid.clone(),
                        })?
                    else {
                        unreachable!("find_element_mut only returns Node::Element");
                    };
                    children
                }
            };
            let len = target_list.len();
            let idx = index.unwrap_or(len);
            if idx > len {
                return Err(IrError::InvalidIndex {
                    parent_id: parent_id.clone(),
                    index: idx,
                    len,
                });
            }
            target_list.insert(idx, node.clone());
            Ok(())
        }
        IrPatch::RemoveNode { page_id, node_id } => {
            let page = find_page_mut(doc, page_id)?;
            if !remove_element_by_id(&mut page.body, node_id) {
                return Err(IrError::NodeNotFound {
                    page_id: page_id.clone(),
                    node_id: node_id.clone(),
                });
            }
            Ok(())
        }
        IrPatch::UpsertStyleRule { rule } => {
            if let Some(existing) = doc.styles.iter_mut().find(|r| r.id == rule.id) {
                *existing = rule.clone();
            } else {
                doc.styles.push(rule.clone());
            }
            Ok(())
        }
        IrPatch::SetDataValue { key, value } => {
            doc.data.insert(key.clone(), value.clone());
            Ok(())
        }
    }
}

fn find_page_mut<'a>(doc: &'a mut SiteDocument, page_id: &str) -> Result<&'a mut Page, IrError> {
    doc.pages
        .iter_mut()
        .find(|p| p.id == page_id)
        .ok_or_else(|| IrError::PageNotFound {
            page_id: page_id.to_string(),
        })
}

fn find_element_mut<'a>(nodes: &'a mut [Node], target_id: &str) -> Option<&'a mut Node> {
    for node in nodes {
        if matches!(node, Node::Element { id: Some(el_id), .. } if el_id == target_id) {
            return Some(node);
        }
        match node {
            Node::Element { children, .. } => {
                if let Some(found) = find_element_mut(children, target_id) {
                    return Some(found);
                }
            }
            Node::Text { .. } => {}
            Node::SlotRef { fallback, .. } => {
                if let Some(found) = find_element_mut(fallback, target_id) {
                    return Some(found);
                }
            }
            Node::ComponentInstance { slots, .. } => {
                for slot_nodes in slots.values_mut() {
                    if let Some(found) = find_element_mut(slot_nodes, target_id) {
                        return Some(found);
                    }
                }
            }
            Node::Conditional {
                then_nodes,
                else_nodes,
                ..
            } => {
                if let Some(found) = find_element_mut(then_nodes, target_id) {
                    return Some(found);
                }
                if let Some(found) = find_element_mut(else_nodes, target_id) {
                    return Some(found);
                }
            }
            Node::Loop { body, .. } => {
                if let Some(found) = find_element_mut(body, target_id) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn remove_element_by_id(nodes: &mut Vec<Node>, target_id: &str) -> bool {
    if let Some(pos) = nodes.iter().position(|n| {
        matches!(
            n,
            Node::Element { id: Some(el_id), .. } if el_id == target_id
        )
    }) {
        nodes.remove(pos);
        return true;
    }

    for node in nodes.iter_mut() {
        let removed = match node {
            Node::Element { children, .. } => remove_element_by_id(children, target_id),
            Node::Text { .. } => false,
            Node::SlotRef { fallback, .. } => remove_element_by_id(fallback, target_id),
            Node::ComponentInstance { slots, .. } => slots
                .values_mut()
                .any(|slot_nodes| remove_element_by_id(slot_nodes, target_id)),
            Node::Conditional {
                then_nodes,
                else_nodes,
                ..
            } => {
                remove_element_by_id(then_nodes, target_id)
                    || remove_element_by_id(else_nodes, target_id)
            }
            Node::Loop { body, .. } => remove_element_by_id(body, target_id),
        };
        if removed {
            return true;
        }
    }

    false
}
