//! JSON Schema (Draft 2020-12) generator for [`crate::SiteDocument`].

use serde_json::{Value, json};

/// Return a Draft 2020-12 JSON Schema describing [`crate::SiteDocument`].
#[must_use]
pub fn site_document_json_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://solo-website-system.example/schemas/site-document.v1.json",
        "title": "SiteDocument",
        "description": "Deterministic intermediate representation for a Solo Website System (SWS) site.",
        "type": "object",
        "additionalProperties": false,
        "required": ["slug", "name", "base_url", "pages"],
        "properties": root_properties(),
        "$defs": {
            "DesignTokens": def_design_tokens(),
            "FontTokens": def_font_tokens(),
            "ContrastPair": def_contrast_pair(),
            "Breakpoint": def_breakpoint(),
            "IrStyleRule": def_style_rule(),
            "ComponentDef": def_component_def(),
            "PropSpec": def_prop_spec(),
            "PropKind": def_prop_kind(),
            "FormSchema": def_form_schema(),
            "FormFieldSchema": def_form_field_schema(),
            "FormFieldKind": def_form_field_kind(),
            "FormFieldOption": def_form_field_option(),
            "Page": def_page(),
            "RoutePattern": def_route_pattern(),
            "JsonLdBlock": def_json_ld_block(),
            "SeoMeta": def_seo_meta(),
            "ScriptRef": def_script_ref(),
            "Node": def_node()
        }
    })
}

fn root_properties() -> Value {
    json!({
        "schema_version": {
            "type": "integer",
            "minimum": 1,
            "default": 1
        },
        "slug": {
            "type": "string",
            "pattern": "^[a-z0-9]+(?:-[a-z0-9]+)*$"
        },
        "name": {
            "type": "string",
            "minLength": 1
        },
        "locale": {
            "type": "string",
            "minLength": 1,
            "default": "en-US"
        },
        "base_url": {
            "type": "string",
            "format": "uri",
            "pattern": "^https://[^\\s/]+(/.*)?$"
        },
        "tokens": { "$ref": "#/$defs/DesignTokens" },
        "breakpoints": {
            "type": "array",
            "items": { "$ref": "#/$defs/Breakpoint" }
        },
        "styles": {
            "type": "array",
            "items": { "$ref": "#/$defs/IrStyleRule" }
        },
        "components": {
            "type": "object",
            "additionalProperties": { "$ref": "#/$defs/ComponentDef" }
        },
        "forms": {
            "type": "object",
            "additionalProperties": { "$ref": "#/$defs/FormSchema" }
        },
        "pages": {
            "type": "array",
            "minItems": 1,
            "items": { "$ref": "#/$defs/Page" }
        },
        "seo": { "$ref": "#/$defs/SeoMeta" },
        "scripts": {
            "type": "array",
            "items": { "$ref": "#/$defs/ScriptRef" }
        },
        "data": {
            "type": "object",
            "additionalProperties": true
        },
        "metadata": {
            "type": "object",
            "additionalProperties": true
        }
    })
}

fn def_design_tokens() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "colors": {
                "type": "object",
                "additionalProperties": {
                    "type": "string",
                    "pattern": "^#[0-9a-fA-F]{6}$"
                }
            },
            "fonts": { "$ref": "#/$defs/FontTokens" },
            "spacing": {
                "type": "object",
                "additionalProperties": { "type": "string" }
            },
            "radii": {
                "type": "object",
                "additionalProperties": { "type": "string" }
            },
            "contrast_pairs": {
                "type": "array",
                "items": { "$ref": "#/$defs/ContrastPair" }
            }
        }
    })
}

fn def_font_tokens() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["body"],
        "properties": {
            "body": { "type": "string", "minLength": 1 },
            "heading": { "type": ["string", "null"] },
            "mono": { "type": ["string", "null"] }
        }
    })
}

fn def_contrast_pair() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["foreground", "background", "min_ratio"],
        "properties": {
            "foreground": { "type": "string", "minLength": 1 },
            "background": { "type": "string", "minLength": 1 },
            "min_ratio": { "type": "number", "exclusiveMinimum": 0 },
            "usage": { "type": ["string", "null"] }
        }
    })
}

fn def_breakpoint() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "media_query"],
        "properties": {
            "id": { "type": "string", "minLength": 1 },
            "media_query": { "type": "string", "minLength": 1 },
            "min_width": { "type": ["string", "null"] }
        }
    })
}

fn def_style_rule() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "selector"],
        "properties": {
            "id": { "type": "string", "minLength": 1 },
            "selector": { "type": "string", "minLength": 1 },
            "breakpoint": { "type": ["string", "null"] },
            "pseudo": { "type": ["string", "null"] },
            "declarations": {
                "type": "object",
                "additionalProperties": { "type": "string" }
            }
        }
    })
}

fn def_component_def() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name"],
        "properties": {
            "name": { "type": "string", "minLength": 1 },
            "props": {
                "type": "object",
                "additionalProperties": { "$ref": "#/$defs/PropSpec" }
            },
            "slots": {
                "type": "array",
                "items": { "type": "string" }
            },
            "template": {
                "type": "array",
                "items": { "$ref": "#/$defs/Node" }
            }
        }
    })
}

fn def_prop_spec() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kind": { "$ref": "#/$defs/PropKind" },
            "required": { "type": "boolean" },
            "default": {},
            "description": { "type": ["string", "null"] }
        }
    })
}

fn def_prop_kind() -> Value {
    json!({
        "type": "string",
        "enum": ["string", "number", "boolean", "array", "object", "json"]
    })
}

fn def_form_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id"],
        "properties": {
            "id": { "type": "string", "minLength": 1 },
            "action": { "type": "string" },
            "method": { "type": "string", "default": "POST" },
            "turnstile_action": { "type": "string" },
            "fields": {
                "type": "array",
                "items": { "$ref": "#/$defs/FormFieldSchema" }
            }
        }
    })
}

fn def_form_field_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "kind"],
        "properties": {
            "name": { "type": "string", "minLength": 1 },
            "label": { "type": "string" },
            "kind": { "$ref": "#/$defs/FormFieldKind" },
            "required": { "type": "boolean" },
            "placeholder": { "type": ["string", "null"] },
            "help_text": { "type": ["string", "null"] },
            "default_value": { "type": ["string", "null"] },
            "options": {
                "type": "array",
                "items": { "$ref": "#/$defs/FormFieldOption" }
            },
            "max_chars": { "type": ["integer", "null"], "minimum": 0 },
            "min": { "type": ["integer", "null"] },
            "max": { "type": ["integer", "null"] },
            "autocomplete": { "type": ["string", "null"] }
        }
    })
}

fn def_form_field_kind() -> Value {
    json!({
        "type": "string",
        "enum": [
            "text",
            "textarea",
            "email",
            "phone",
            "date",
            "integer",
            "select",
            "checkbox",
            "hidden",
            "route"
        ]
    })
}

fn def_form_field_option() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["value", "label"],
        "properties": {
            "value": { "type": "string" },
            "label": { "type": "string" }
        }
    })
}

fn def_page() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "route", "title"],
        "properties": {
            "id": { "type": "string", "minLength": 1 },
            "route": { "$ref": "#/$defs/RoutePattern" },
            "title": { "type": "string" },
            "description": { "type": ["string", "null"] },
            "canonical": { "type": ["string", "null"] },
            "noindex": { "type": "boolean" },
            "is_not_found": { "type": "boolean" },
            "json_ld": {
                "type": "array",
                "items": { "$ref": "#/$defs/JsonLdBlock" }
            },
            "scripts": {
                "type": "array",
                "items": { "$ref": "#/$defs/ScriptRef" }
            },
            "body": {
                "type": "array",
                "items": { "$ref": "#/$defs/Node" }
            }
        }
    })
}

fn def_route_pattern() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "path"],
                "properties": {
                    "kind": { "const": "static" },
                    "path": { "type": "string", "pattern": "^/" }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind", "pattern", "params"],
                "properties": {
                    "kind": { "const": "dynamic" },
                    "pattern": { "type": "string", "pattern": "^/" },
                    "params": {
                        "type": "array",
                        "minItems": 1,
                        "items": { "type": "string", "minLength": 1 }
                    }
                }
            }
        ]
    })
}

fn def_json_ld_block() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["data"],
        "properties": {
            "id": { "type": ["string", "null"] },
            "data": {}
        }
    })
}

fn def_seo_meta() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "title_template": { "type": ["string", "null"] },
            "default_description": { "type": ["string", "null"] },
            "robots_allow": {
                "type": "array",
                "items": { "type": "string" }
            },
            "robots_disallow": {
                "type": "array",
                "items": { "type": "string" }
            },
            "sitemap_routes": {
                "type": "array",
                "items": { "type": "string" }
            }
        }
    })
}

fn def_script_ref() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["src"],
        "properties": {
            "src": { "type": "string", "minLength": 1 },
            "async_load": { "type": "boolean" },
            "defer": { "type": "boolean" }
        }
    })
}

fn def_node() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "tag"],
                "properties": {
                    "type": { "const": "element" },
                    "uid": { "type": "string", "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$" },
                    "id": { "type": ["string", "null"] },
                    "tag": { "type": "string", "minLength": 1 },
                    "attrs": {
                        "type": "object",
                        "additionalProperties": { "type": "string" }
                    },
                    "classes": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "style_refs": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "children": {
                        "type": "array",
                        "items": { "$ref": "#/$defs/Node" }
                    }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "value"],
                "properties": {
                    "type": { "const": "text" },
                    "uid": { "type": "string", "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$" },
                    "value": { "type": "string" }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "name"],
                "properties": {
                    "type": { "const": "slot_ref" },
                    "uid": { "type": "string", "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$" },
                    "name": { "type": "string", "minLength": 1 },
                    "fallback": {
                        "type": "array",
                        "items": { "$ref": "#/$defs/Node" }
                    }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "component"],
                "properties": {
                    "type": { "const": "component_instance" },
                    "uid": { "type": "string", "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$" },
                    "component": { "type": "string", "minLength": 1 },
                    "props": {
                        "type": "object",
                        "additionalProperties": true
                    },
                    "slots": {
                        "type": "object",
                        "additionalProperties": {
                            "type": "array",
                            "items": { "$ref": "#/$defs/Node" }
                        }
                    }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "expr"],
                "properties": {
                    "type": { "const": "conditional" },
                    "uid": { "type": "string", "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$" },
                    "expr": { "type": "string", "minLength": 1 },
                    "then_nodes": {
                        "type": "array",
                        "items": { "$ref": "#/$defs/Node" }
                    },
                    "else_nodes": {
                        "type": "array",
                        "items": { "$ref": "#/$defs/Node" }
                    }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "item_var", "collection_expr"],
                "properties": {
                    "type": { "const": "loop" },
                    "uid": { "type": "string", "pattern": "^[a-z0-9][a-z0-9._-]{0,63}$" },
                    "item_var": { "type": "string", "minLength": 1 },
                    "collection_expr": { "type": "string", "minLength": 1 },
                    "body": {
                        "type": "array",
                        "items": { "$ref": "#/$defs/Node" }
                    }
                }
            }
        ]
    })
}
