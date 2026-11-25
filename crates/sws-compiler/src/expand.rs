//! Deterministic IR node expansion and HTML5 element serialization.

use std::collections::BTreeMap;

use serde_json::Value;
use sws_ir::{Node, SiteDocument};

use crate::escape::esc_html;

const MAX_COMPONENT_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExpandedNode {
    Element {
        tag: String,
        attrs: BTreeMap<String, String>,
        classes: Vec<String>,
        children: Vec<ExpandedNode>,
    },
    Text(String),
}

struct EvalContext<'a> {
    doc: &'a SiteDocument,
    scopes: Vec<BTreeMap<String, Value>>,
    slot_stack: Vec<BTreeMap<String, Vec<ExpandedNode>>>,
    depth: usize,
}

impl<'a> EvalContext<'a> {
    fn new(doc: &'a SiteDocument) -> Self {
        Self {
            doc,
            scopes: vec![doc.data.clone()],
            slot_stack: Vec::new(),
            depth: 0,
        }
    }

    fn resolve_expr(&self, expr: &str) -> Option<&Value> {
        let path = expr.trim();
        if path.is_empty() {
            return None;
        }
        let mut parts = path.split('.');
        let root_key = parts.next()?;
        for scope in self.scopes.iter().rev() {
            if let Some(mut cur) = scope.get(root_key) {
                let mut found = true;
                for part in parts.clone() {
                    if let Some(next) = cur.get(part) {
                        cur = next;
                    } else {
                        found = false;
                        break;
                    }
                }
                if found {
                    return Some(cur);
                }
            }
        }
        None
    }

    fn is_truthy(&self, expr: &str) -> bool {
        let trimmed = expr.trim();
        let (negated, inner) = if let Some(rest) = trimmed.strip_prefix('!') {
            (true, rest.trim())
        } else {
            (false, trimmed)
        };
        let val = match self.resolve_expr(inner) {
            None => false,
            Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
            Some(Value::String(s)) => !s.is_empty(),
            Some(Value::Array(a)) => !a.is_empty(),
            Some(Value::Object(m)) => !m.is_empty(),
        };
        if negated { !val } else { val }
    }

    fn interpolate(&self, input: &str) -> String {
        if !input.contains("{{") {
            return input.to_string();
        }
        let mut out = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after_open = &rest[start + 2..];
            if let Some(end) = after_open.find("}}") {
                let expr = &after_open[..end];
                if let Some(val) = self.resolve_expr(expr) {
                    out.push_str(&value_to_string(val));
                }
                rest = &after_open[end + 2..];
            } else {
                out.push_str("{{");
                rest = after_open;
            }
        }
        out.push_str(rest);
        out
    }

    fn interpolate_value(&self, val: &Value) -> Value {
        match val {
            Value::String(s) if s.contains("{{") => Value::String(self.interpolate(s)),
            _ => val.clone(),
        }
    }
}

fn value_to_string(val: &Value) -> String {
    match val {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

pub(crate) fn expand_page_body(doc: &SiteDocument, nodes: &[Node]) -> Vec<ExpandedNode> {
    let mut ctx = EvalContext::new(doc);
    expand_nodes(&mut ctx, nodes)
}

fn expand_nodes(ctx: &mut EvalContext<'_>, nodes: &[Node]) -> Vec<ExpandedNode> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        expand_single(ctx, node, &mut out);
    }
    out
}

fn expand_single(ctx: &mut EvalContext<'_>, node: &Node, out: &mut Vec<ExpandedNode>) {
    match node {
        Node::Text { value } => {
            out.push(ExpandedNode::Text(ctx.interpolate(value)));
        }
        Node::Element {
            id,
            tag,
            attrs,
            classes,
            style_refs: _,
            children,
        } => {
            let mut merged_attrs = BTreeMap::new();
            let mut merged_classes: Vec<String> = Vec::new();
            let mut push_class = |cls: &str| {
                let c = cls.trim();
                if !c.is_empty() && !merged_classes.iter().any(|existing| existing == c) {
                    merged_classes.push(c.to_string());
                }
            };

            for cls in classes {
                for part in ctx.interpolate(cls).split_whitespace() {
                    push_class(part);
                }
            }

            for (k, v) in attrs {
                let interpolated = ctx.interpolate(v);
                if k.eq_ignore_ascii_case("class") {
                    for part in interpolated.split_whitespace() {
                        push_class(part);
                    }
                } else if k.eq_ignore_ascii_case("id") {
                    merged_attrs.insert("id".to_string(), interpolated);
                } else {
                    merged_attrs.insert(k.clone(), interpolated);
                }
            }

            if let Some(id_val) = id {
                let interpolated_id = ctx.interpolate(id_val);
                if !interpolated_id.is_empty() {
                    merged_attrs.insert("id".to_string(), interpolated_id);
                }
            }

            let expanded_children = expand_nodes(ctx, children);
            out.push(ExpandedNode::Element {
                tag: tag.clone(),
                attrs: merged_attrs,
                classes: merged_classes,
                children: expanded_children,
            });
        }
        Node::Conditional {
            expr,
            then_nodes,
            else_nodes,
        } => {
            let branch = if ctx.is_truthy(expr) {
                then_nodes
            } else {
                else_nodes
            };
            out.extend(expand_nodes(ctx, branch));
        }
        Node::Loop {
            item_var,
            collection_expr,
            body,
        } => {
            let items = ctx
                .resolve_expr(collection_expr)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for item in items {
                let mut loop_scope = BTreeMap::new();
                loop_scope.insert(item_var.clone(), item);
                ctx.scopes.push(loop_scope);
                out.extend(expand_nodes(ctx, body));
                ctx.scopes.pop();
            }
        }
        Node::ComponentInstance {
            component,
            props,
            slots,
        } => {
            if ctx.depth >= MAX_COMPONENT_DEPTH {
                return;
            }
            let Some(def) = ctx.doc.components.get(component) else {
                return;
            };

            let mut evaluated_slots = BTreeMap::new();
            for (slot_name, slot_nodes) in slots {
                evaluated_slots.insert(slot_name.clone(), expand_nodes(ctx, slot_nodes));
            }

            let mut prop_scope = BTreeMap::new();
            for (prop_name, prop_spec) in &def.props {
                if let Some(val) = props.get(prop_name) {
                    prop_scope.insert(prop_name.clone(), ctx.interpolate_value(val));
                } else if let Some(default_val) = &prop_spec.default {
                    prop_scope.insert(prop_name.clone(), default_val.clone());
                }
            }
            for (k, v) in props {
                prop_scope
                    .entry(k.clone())
                    .or_insert_with(|| ctx.interpolate_value(v));
            }

            ctx.scopes.push(prop_scope);
            ctx.slot_stack.push(evaluated_slots);
            ctx.depth += 1;
            out.extend(expand_nodes(ctx, &def.template));
            ctx.depth -= 1;
            ctx.slot_stack.pop();
            ctx.scopes.pop();
        }
        Node::SlotRef { name, fallback } => {
            let slot_content = ctx
                .slot_stack
                .last()
                .and_then(|map| map.get(name))
                .filter(|nodes| !nodes.is_empty())
                .cloned();
            if let Some(nodes) = slot_content {
                out.extend(nodes);
            } else {
                out.extend(expand_nodes(ctx, fallback));
            }
        }
    }
}

fn is_void_element(tag: &str) -> bool {
    matches!(
        tag.to_ascii_lowercase().as_str(),
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_boolean_attr(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "required"
            | "disabled"
            | "checked"
            | "readonly"
            | "multiple"
            | "selected"
            | "hidden"
            | "novalidate"
            | "async"
            | "defer"
    )
}

fn is_safe_tag_name(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(crate) fn render_expanded_html(nodes: &[ExpandedNode]) -> String {
    let mut out = String::new();
    for node in nodes {
        write_expanded_node(node, &mut out);
    }
    out
}

fn write_expanded_node(node: &ExpandedNode, out: &mut String) {
    match node {
        ExpandedNode::Text(text) => out.push_str(&esc_html(text)),
        ExpandedNode::Element {
            tag,
            attrs,
            classes,
            children,
        } => {
            if !is_safe_tag_name(tag) {
                return;
            }
            out.push('<');
            out.push_str(tag);

            if let Some(id_val) = attrs.get("id") {
                out.push_str(" id=\"");
                out.push_str(&esc_html(id_val));
                out.push('"');
            }
            if !classes.is_empty() {
                out.push_str(" class=\"");
                out.push_str(&esc_html(&classes.join(" ")));
                out.push('"');
            }
            for (k, v) in attrs {
                if k == "id" || k.eq_ignore_ascii_case("class") || !is_safe_tag_name(k) {
                    continue;
                }
                if is_boolean_attr(k) {
                    if v.eq_ignore_ascii_case("false") {
                        continue;
                    }
                    if v.is_empty()
                        || v.eq_ignore_ascii_case("true")
                        || v.eq_ignore_ascii_case(k)
                    {
                        out.push(' ');
                        out.push_str(k);
                        continue;
                    }
                }
                out.push(' ');
                out.push_str(k);
                out.push_str("=\"");
                out.push_str(&esc_html(v));
                out.push('"');
            }

            out.push('>');
            if is_void_element(tag) {
                return;
            }
            for child in children {
                write_expanded_node(child, out);
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
    }
}
