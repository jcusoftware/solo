//! Deterministic IR node expansion and HTML5 element serialization.

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value;
use sws_ir::{Node, SiteDocument};

use crate::escape::esc_html;

const MAX_COMPONENT_DEPTH: usize = 64;
const MAX_NODE_DEPTH: usize = 128;
const MAX_EXPANDED_NODES: usize = 50_000;
const MAX_EXPANDED_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// Upper bound on collected evaluation warnings so expansion bombs cannot
/// allocate unbounded diagnostic vectors. Count-based, therefore deterministic.
const MAX_EVAL_WARNINGS: usize = 1_000;

pub(crate) struct ExpandedBody {
    pub(crate) nodes: Vec<ExpandedNode>,
    pub(crate) limit_exceeded: bool,
    pub(crate) warnings: Vec<EvalWarning>,
}

/// A non-fatal evaluation finding collected during template expansion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvalWarning {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

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

/// A value bound in an evaluation scope.
///
/// [`ScopeVal::Shared`] keeps values behind an `Rc` so repeated resolution
/// (e.g. a collection read by many Loop evaluations) never re-copies them.
/// [`ScopeVal::ItemRef`] binds a Loop item by index into a shared collection
/// so instantiating a loop body never deep-clones the item (issue #13).
enum ScopeVal {
    Shared(Rc<Value>),
    ItemRef {
        collection: Rc<Value>,
        index: usize,
    },
}

struct EvalContext<'a> {
    doc: &'a SiteDocument,
    scopes: Vec<BTreeMap<String, ScopeVal>>,
    slot_stack: Vec<BTreeMap<String, Vec<ExpandedNode>>>,
    depth: usize,
    node_depth: usize,
    expanded_nodes: usize,
    text_bytes: usize,
    limit_exceeded: bool,
    warnings: Vec<EvalWarning>,
}

impl<'a> EvalContext<'a> {
    fn new(doc: &'a SiteDocument) -> Self {
        Self {
            doc,
            scopes: vec![doc
                .data
                .iter()
                .map(|(key, val)| (key.clone(), ScopeVal::Shared(Rc::new(val.clone()))))
                .collect()],
            slot_stack: Vec::new(),
            depth: 0,
            node_depth: 0,
            expanded_nodes: 0,
            text_bytes: 0,
            limit_exceeded: false,
            warnings: Vec::new(),
        }
    }

    /// Record an evaluation warning, capped at [`MAX_EVAL_WARNINGS`] entries.
    fn warn(&mut self, code: &'static str, message: String) {
        if self.warnings.len() < MAX_EVAL_WARNINGS {
            self.warnings.push(EvalWarning { code, message });
        }
    }

    fn record_node(&mut self) -> bool {
        if self.limit_exceeded || self.expanded_nodes >= MAX_EXPANDED_NODES {
            self.limit_exceeded = true;
            return false;
        }
        self.expanded_nodes += 1;
        true
    }

    fn record_text_bytes(&mut self, bytes: usize) -> bool {
        if self.limit_exceeded || self.text_bytes.saturating_add(bytes) > MAX_EXPANDED_TEXT_BYTES {
            self.limit_exceeded = true;
            return false;
        }
        self.text_bytes += bytes;
        true
    }

    fn resolve_expr(&self, expr: &str) -> Option<&Value> {
        let path = expr.trim();
        if path.is_empty() {
            return None;
        }
        let mut parts = path.split('.');
        let root_key = parts.next()?;
        for scope in self.scopes.iter().rev() {
            if let Some(scope_val) = scope.get(root_key) {
                let mut cur: &Value = match scope_val {
                    ScopeVal::Shared(val) => val,
                    ScopeVal::ItemRef { collection, index } => {
                        match collection.as_array().and_then(|items| items.get(*index)) {
                            Some(item) => item,
                            // Unreachable: ItemRef indices are always valid
                            // when created; fall through to the next scope.
                            None => continue,
                        }
                    }
                };
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

    /// Resolve an expression to an owned shared handle for Loop collections.
    ///
    /// A root-level scope entry (the common case, e.g. `items`) is returned as
    /// a cheap `Rc` clone with no deep copy. Dotted paths and loop items fall
    /// back to [`EvalContext::resolve_expr`] plus a single deep clone, matching
    /// the previous whole-collection clone per evaluation.
    fn resolve_collection(&self, expr: &str) -> Option<Rc<Value>> {
        let path = expr.trim();
        if path.is_empty() {
            return None;
        }
        let mut parts = path.split('.');
        let root_key = parts.next()?;
        if parts.next().is_none() {
            for scope in self.scopes.iter().rev() {
                if let Some(scope_val) = scope.get(root_key) {
                    if let ScopeVal::Shared(val) = scope_val {
                        return Some(Rc::clone(val));
                    }
                    break;
                }
            }
        }
        self.resolve_expr(expr).map(|val| Rc::new(val.clone()))
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

    fn interpolate(&mut self, input: &str) -> String {
        if self.limit_exceeded {
            return String::new();
        }
        if !input.contains("{{") {
            if !self.record_text_bytes(input.len()) {
                return String::new();
            }
            return input.to_string();
        }
        let mut out = String::with_capacity(input.len().min(4096));
        let mut rest = input;
        while let Some(start) = rest.find("{{") {
            let prefix = &rest[..start];
            if !self.record_text_bytes(prefix.len()) {
                return String::new();
            }
            out.push_str(prefix);
            let after_open = &rest[start + 2..];
            if let Some(end) = after_open.find("}}") {
                let expr = &after_open[..end];
                if let Some(rendered) = self.resolve_expr(expr).map(value_to_string) {
                    if !self.record_text_bytes(rendered.len()) {
                        return String::new();
                    }
                    out.push_str(&rendered);
                } else {
                    // Unresolved `{{expr}}`: the token is dropped, but surface a
                    // warning so silent content loss is diagnosable.
                    self.warn(
                        "unresolved_expression",
                        format!(
                            "template expression '{{{{ {} }}}}' did not resolve and was dropped",
                            expr.trim()
                        ),
                    );
                }
                rest = &after_open[end + 2..];
            } else {
                if !self.record_text_bytes(2) {
                    return String::new();
                }
                out.push_str("{{");
                rest = after_open;
            }
        }
        if !self.record_text_bytes(rest.len()) {
            return String::new();
        }
        out.push_str(rest);
        out
    }

    fn interpolate_value(&mut self, val: &Value) -> Value {
        match val {
            Value::String(s) if s.contains("{{") => Value::String(self.interpolate(s)),
            _ => val.clone(),
        }
    }
}

fn count_expanded_tree(nodes: &[ExpandedNode]) -> (usize, usize) {
    let mut count = 0usize;
    let mut bytes = 0usize;
    let mut stack: Vec<&ExpandedNode> = nodes.iter().collect();
    while let Some(node) = stack.pop() {
        count = count.saturating_add(1);
        match node {
            ExpandedNode::Text(t) => {
                bytes = bytes.saturating_add(t.len());
            }
            ExpandedNode::Element {
                tag,
                attrs,
                classes,
                children,
            } => {
                bytes = bytes.saturating_add(tag.len());
                for (k, v) in attrs {
                    bytes = bytes.saturating_add(k.len()).saturating_add(v.len());
                }
                for c in classes {
                    bytes = bytes.saturating_add(c.len());
                }
                for child in children {
                    stack.push(child);
                }
            }
        }
        if count > MAX_EXPANDED_NODES || bytes > MAX_EXPANDED_TEXT_BYTES {
            return (count, bytes);
        }
    }
    (count, bytes)
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

pub(crate) fn expand_page_body(doc: &SiteDocument, nodes: &[Node]) -> ExpandedBody {
    let mut ctx = EvalContext::new(doc);
    let expanded = expand_nodes(&mut ctx, nodes);
    ExpandedBody {
        nodes: expanded,
        limit_exceeded: ctx.limit_exceeded,
        warnings: ctx.warnings,
    }
}

fn expand_nodes(ctx: &mut EvalContext<'_>, nodes: &[Node]) -> Vec<ExpandedNode> {
    if ctx.limit_exceeded || ctx.node_depth >= MAX_NODE_DEPTH {
        ctx.limit_exceeded = true;
        return Vec::new();
    }
    ctx.node_depth += 1;
    let mut out = Vec::with_capacity(nodes.len().min(64));
    for node in nodes {
        if ctx.limit_exceeded {
            break;
        }
        expand_single(ctx, node, &mut out);
    }
    ctx.node_depth -= 1;
    out
}

fn expand_single(ctx: &mut EvalContext<'_>, node: &Node, out: &mut Vec<ExpandedNode>) {
    if !ctx.record_node() {
        return;
    }
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
            // Resolve the collection once as a shared handle (issue #13):
            // root-level entries cost only an `Rc` clone and items are bound
            // into the loop scope by index, so instantiating the body never
            // deep-copies the collection. Item counts still go through
            // `record_node` below.
            let collection = ctx.resolve_collection(collection_expr);
            let items: &[Value] = match collection.as_deref() {
                Some(Value::Array(items)) => items.as_slice(),
                other => {
                    // Unresolved or non-array collection: the loop renders
                    // nothing, but warn (with a non-empty body) so the silent
                    // skip is diagnosable.
                    if !body.is_empty() {
                        let found = match other {
                            Some(Value::Null) => "null",
                            Some(_) => "a non-array value",
                            None => "not defined",
                        };
                        ctx.warn(
                            "unresolvable_loop_collection",
                            format!(
                                "loop collection '{collection_expr}' resolved to {found}; loop body with {} node(s) was skipped",
                                body.len()
                            ),
                        );
                    }
                    &[]
                }
            };
            for index in 0..items.len() {
                if !ctx.record_node() {
                    break;
                }
                let mut loop_scope = BTreeMap::new();
                if let Some(collection) = collection.as_ref() {
                    loop_scope.insert(
                        item_var.clone(),
                        ScopeVal::ItemRef {
                            collection: Rc::clone(collection),
                            index,
                        },
                    );
                }
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
                ctx.limit_exceeded = true;
                return;
            }
            let Some(def) = ctx.doc.components.get(component) else {
                return;
            };

            let mut evaluated_slots = BTreeMap::new();
            for (slot_name, slot_nodes) in slots {
                if ctx.limit_exceeded {
                    return;
                }
                evaluated_slots.insert(slot_name.clone(), expand_nodes(ctx, slot_nodes));
            }
            if ctx.limit_exceeded {
                return;
            }

            let mut prop_scope = BTreeMap::new();
            for (prop_name, prop_spec) in &def.props {
                if ctx.limit_exceeded {
                    return;
                }
                if let Some(val) = props.get(prop_name) {
                    let evaluated = ctx.interpolate_value(val);
                    prop_scope.insert(prop_name.clone(), ScopeVal::Shared(Rc::new(evaluated)));
                } else if let Some(default_val) = &prop_spec.default {
                    prop_scope.insert(
                        prop_name.clone(),
                        ScopeVal::Shared(Rc::new(default_val.clone())),
                    );
                }
            }
            for (k, v) in props {
                if ctx.limit_exceeded {
                    return;
                }
                if !prop_scope.contains_key(k) {
                    let evaluated = ctx.interpolate_value(v);
                    prop_scope.insert(k.clone(), ScopeVal::Shared(Rc::new(evaluated)));
                }
            }
            if ctx.limit_exceeded {
                return;
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
            let slot_metrics = ctx
                .slot_stack
                .last()
                .and_then(|map| map.get(name))
                .filter(|nodes| !nodes.is_empty())
                .map(|nodes| count_expanded_tree(nodes));
            if let Some((cloned_nodes, cloned_bytes)) = slot_metrics {
                if ctx.expanded_nodes.saturating_add(cloned_nodes) > MAX_EXPANDED_NODES
                    || ctx.text_bytes.saturating_add(cloned_bytes) > MAX_EXPANDED_TEXT_BYTES
                {
                    ctx.limit_exceeded = true;
                    return;
                }
                ctx.expanded_nodes += cloned_nodes;
                ctx.text_bytes += cloned_bytes;
                if let Some(nodes) = ctx
                    .slot_stack
                    .last()
                    .and_then(|map| map.get(name))
                    .cloned()
                {
                    out.extend(nodes);
                }
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

pub(crate) fn is_safe_tag_name(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(crate) fn is_forbidden_body_tag(tag_lower: &str) -> bool {
    matches!(
        tag_lower,
        "script"
            | "style"
            | "iframe"
            | "object"
            | "embed"
            | "applet"
            | "base"
            | "meta"
            | "link"
            | "frame"
            | "frameset"
            | "animate"
            | "animatemotion"
            | "animatetransform"
            | "set"
            | "foreignobject"
            | "handler"
            | "listener"
            | "maction"
            | "head"
            | "html"
            | "body"
            | "title"
            | "xmp"
            | "plaintext"
            | "noembed"
            | "noframes"
            | "portal"
            | "fencedframe"
    )
}

pub(crate) fn is_forbidden_attr(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == "style" || lower == "srcdoc" {
        return true;
    }
    let bytes = lower.as_bytes();
    bytes.len() > 2 && &bytes[..2] == b"on" && bytes[2].is_ascii_alphabetic()
}

pub(crate) fn is_url_attr(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "href"
            | "src"
            | "action"
            | "formaction"
            | "poster"
            | "cite"
            | "data"
            | "xlink:href"
            | "background"
            | "longdesc"
            | "usemap"
            | "dynsrc"
            | "lowsrc"
    )
}

pub(crate) fn is_url_list_attr(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "srcset" | "imagesrcset" | "ping"
    )
}

pub(crate) fn is_safe_url_attr_value(attr: &str, val: &str) -> bool {
    if val.chars().any(char::is_control) {
        return false;
    }
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return true;
    }
    if trimmed.starts_with("//") {
        return false;
    }
    if trimmed.starts_with('#') || trimmed.starts_with('/') || trimmed.starts_with('?') {
        return !trimmed.contains('\\');
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        return !authority.is_empty()
            && !trimmed.contains('\\')
            && !trimmed.chars().any(char::is_whitespace);
    }
    if attr.eq_ignore_ascii_case("href")
        && (lower.starts_with("mailto:") || lower.starts_with("tel:"))
    {
        return !trimmed.chars().any(char::is_control);
    }
    // Reject any other explicit URI scheme (e.g. javascript:, vbscript:, data:, blob:, file:).
    let before_delim = trimmed.split(['/', '?', '#']).next().unwrap_or("");
    !before_delim.contains(':') && !trimmed.contains('\\')
}

pub(crate) fn is_safe_url_list_attr_value(attr: &str, val: &str) -> bool {
    if val.chars().any(char::is_control) {
        return false;
    }
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return true;
    }
    if attr.eq_ignore_ascii_case("ping") {
        return trimmed
            .split_whitespace()
            .all(|tok| is_safe_url_attr_value("src", tok));
    }
    // srcset / imagesrcset: comma-separated `<url> [descriptor]` candidates
    trimmed.split(',').all(|cand| {
        let cand = cand.trim();
        let Some(url_tok) = cand.split_whitespace().next() else {
            return false;
        };
        is_safe_url_attr_value("src", url_tok)
    })
}

pub(crate) fn is_safe_script_src(src: &str) -> bool {
    if src.is_empty()
        || src
            .bytes()
            .any(|b| b <= 0x20 || b == 0x7f || b == b'\\')
    {
        return false;
    }
    if src.starts_with('/') {
        return !src.starts_with("//");
    }
    sws_ir::is_valid_https_url(src)
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
            if !is_safe_tag_name(tag) || is_forbidden_body_tag(&tag.to_ascii_lowercase()) {
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
                if k == "id"
                    || k.eq_ignore_ascii_case("class")
                    || !is_safe_tag_name(k)
                    || is_forbidden_attr(k)
                    || (is_url_attr(k) && !is_safe_url_attr_value(k, v))
                    || (is_url_list_attr(k) && !is_safe_url_list_attr_value(k, v))
                {
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
