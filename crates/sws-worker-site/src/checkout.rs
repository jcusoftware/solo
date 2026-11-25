//! Demo checkout policy (sandbox Stripe only). The transport is injected so
//! host tests never call Stripe; only the wasm entrypoint wires real fetches.

use std::{collections::BTreeMap, future::Future};

use serde_json::Value;
use url::{Url, form_urlencoded};

pub const MAX_BODY: usize = 4096;
const MAX_TOTAL: u64 = 10_000;
const MAX_ITEM_QUANTITY: u64 = 5;
const MAX_CART_QUANTITY: u64 = 10;

pub struct Selection {
    pub token: String,
    mode: &'static str,
    lines: Vec<(String, u64, u64)>,
    total: u64,
}

pub fn select(body: &[u8], catalog: &Value) -> Option<Selection> {
    if body.len() > MAX_BODY || !body.is_ascii() {
        return None;
    }
    // Require well-formed form encoding before the URL decoder (which is lossy).
    if body.iter().enumerate().any(|(i, b)| {
        *b == b'%'
            && !body
                .get(i + 1..i + 3)
                .is_some_and(|p| p.iter().all(u8::is_ascii_hexdigit))
    }) || body.split(|b| *b == b'&').any(|part| !part.contains(&b'='))
    {
        return None;
    }
    let mut fields = BTreeMap::new();
    for (name, value) in form_urlencoded::parse(body) {
        if fields.len() >= 7
            || fields
                .insert(name.into_owned(), value.into_owned())
                .is_some()
        {
            return None;
        }
    }
    let token = fields.remove("cf-turnstile-response")?;
    if !identifier(&token, 2048, |b| matches!(b, b'-' | b'_' | b'.')) {
        return None;
    }
    let mode = fields.remove("mode")?;
    let mut lines = Vec::new();
    let mode = match mode.as_str() {
        "subscription" if fields.is_empty() => {
            let sub = &catalog["subscription"];
            lines.push((sub["id"].as_str()?.to_owned(), 1, cents(&sub["cents"])?));
            "subscription"
        }
        "payment" => {
            let items = catalog["items"].as_object()?;
            if !(2..=5).contains(&items.len()) {
                return None;
            }
            for (field, qty) in fields {
                let id = field.strip_prefix("qty-")?;
                let unit = cents(items.get(id)?)?;
                if qty.len() != 1 || !qty.as_bytes()[0].is_ascii_digit() {
                    return None;
                }
                let qty = qty
                    .parse::<u64>()
                    .ok()
                    .filter(|q| *q <= MAX_ITEM_QUANTITY)?;
                if qty > 0 {
                    lines.push((id.to_owned(), qty, unit));
                }
            }
            "payment"
        }
        _ => return None,
    };
    let quantity: u64 = lines.iter().map(|(_, qty, _)| qty).sum();
    let total = lines.iter().map(|(_, qty, unit)| qty * unit).sum();
    (quantity > 0 && quantity <= MAX_CART_QUANTITY && total <= MAX_TOTAL).then_some(Selection {
        token,
        mode,
        lines,
        total,
    })
}

fn cents(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| (50..=MAX_TOTAL).contains(n))
}

fn identifier(s: &str, max: usize, extra: impl Fn(u8) -> bool) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || extra(b))
}

/// Only sandbox (test-mode) Stripe secret keys are ever accepted.
pub fn sandbox_key(key: &str) -> bool {
    key.strip_prefix("sk_test_")
        .or_else(|| key.strip_prefix("rk_test_"))
        .is_some_and(|id| identifier(id, 256, |_| false))
}

/// The return origin must be a bare trusted origin: https, or http on
/// loopback for local testing, with no path, query, credentials or fragment.
pub fn return_origin(raw: &str) -> Option<Url> {
    let url = Url::parse(raw).ok()?;
    let origin = url.origin().ascii_serialization();
    if (raw != origin && raw != format!("{origin}/"))
        || !(url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))))
    {
        return None;
    }
    Some(url)
}

fn price_matches(price: &Value, id: &str, unit: u64, monthly: bool) -> bool {
    price["object"] == "price"
        && price["id"] == id
        && price["livemode"] == false
        && price["active"] == true
        && price["currency"] == "usd"
        && price["unit_amount"].as_u64() == Some(unit)
        && price["billing_scheme"] == "per_unit"
        && price.get("custom_unit_amount").is_some_and(Value::is_null)
        && price.get("transform_quantity").is_some_and(Value::is_null)
        && if monthly {
            price["type"] == "recurring"
                && price["recurring"]["interval"] == "month"
                && price["recurring"]["interval_count"] == 1
                && price["recurring"]["usage_type"] == "licensed"
        } else {
            price["type"] == "one_time" && price.get("recurring").is_some_and(Value::is_null)
        }
}

fn hosted_url(session: &Value) -> Option<String> {
    let id = session["id"].as_str()?;
    if !id
        .strip_prefix("cs_test_")
        .is_some_and(|id| identifier(id, 256, |_| false))
    {
        return None;
    }
    let raw = session["url"].as_str()?;
    let base = format!("https://checkout.stripe.com/c/pay/{id}");
    if raw == base
        || raw
            .strip_prefix(&format!("{base}#"))
            .is_some_and(|fragment| {
                identifier(fragment, 4096, |b| {
                    matches!(b, b'%' | b'-' | b'_' | b'=' | b'+' | b'/')
                })
            })
    {
        Some(raw.to_owned())
    } else {
        None
    }
}

/// Retrieve each selected Price before creating a session. Validate both API
/// response shapes before trusting any amount or navigation target.
pub async fn create<F, Fut>(
    selection: &Selection,
    ids: &Value,
    origin: &Url,
    mut api: F,
) -> Option<String>
where
    F: FnMut(String, Option<String>) -> Fut,
    Fut: Future<Output = Option<Value>>,
{
    let success = origin.join("/takeout#returned").ok()?;
    let cancel = origin
        .join(if selection.mode == "subscription" {
            "/takeout#subscription-heading"
        } else {
            "/takeout#pickup-heading"
        })
        .ok()?;
    let mut pairs = vec![
        ("mode".to_owned(), selection.mode.to_owned()),
        ("success_url".into(), success.to_string()),
        ("cancel_url".into(), cancel.to_string()),
        ("payment_method_types[0]".into(), "card".into()),
        ("automatic_tax[enabled]".into(), "false".into()),
        ("adaptive_pricing[enabled]".into(), "false".into()),
        ("expand[0]".into(), "line_items".into()),
    ];
    let monthly = selection.mode == "subscription";
    let mut expected = BTreeMap::new();
    for (i, (sku, qty, unit)) in selection.lines.iter().enumerate() {
        let id = ids[sku].as_str()?;
        if !id
            .strip_prefix("price_")
            .is_some_and(|id| identifier(id, 128, |_| false))
            || expected.insert(id, (*qty, *unit)).is_some()
        {
            return None;
        }
        let price = api(format!("/v1/prices/{id}"), None).await?;
        if !price_matches(&price, id, *unit, monthly) {
            return None;
        }
        pairs.push((format!("line_items[{i}][price]"), id.to_owned()));
        pairs.push((format!("line_items[{i}][quantity]"), qty.to_string()));
    }
    let body = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&pairs)
        .finish();
    let session = api("/v1/checkout/sessions".into(), Some(body)).await?;
    if session["object"] != "checkout.session"
        || session["livemode"] != false
        || session["mode"] != selection.mode
        || session["status"] != "open"
        || session["ui_mode"] != "hosted"
        || session["currency"] != "usd"
        || session["success_url"] != success.as_str()
        || session["cancel_url"] != cancel.as_str()
        || session["amount_subtotal"].as_u64() != Some(selection.total)
        || session["amount_total"].as_u64() != Some(selection.total)
        || session["line_items"]["has_more"] != false
    {
        return None;
    }
    for line in session["line_items"]["data"].as_array()? {
        let price = &line["price"];
        let id = price["id"].as_str()?;
        let (qty, unit) = expected.remove(id)?;
        if !price_matches(price, id, unit, monthly)
            || line["quantity"].as_u64() != Some(qty)
            || line["currency"] != "usd"
            || line["amount_total"].as_u64() != Some(qty * unit)
        {
            return None;
        }
    }
    expected.is_empty().then(|| hosted_url(&session)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        future::ready,
        pin::pin,
        task::{Context, Poll, Waker},
    };

    fn catalog() -> Value {
        json!({"items":{"pie":1200,"salad":900,"bread":400},"subscription":{"id":"beans","cents":1600}})
    }

    #[test]
    fn strictly_validates_cart_and_subscription() {
        let c = catalog();
        let parse = |post: &[u8]| select(post, &c);
        let valid =
            parse(b"mode=payment&qty-pie=2&qty-salad=1&qty-bread=0&cf-turnstile-response=dummy")
                .unwrap();
        assert_eq!(valid.total, 3300);
        assert_eq!(valid.lines.len(), 2);
        let sub = parse(b"mode=subscription&cf-turnstile-response=dummy").unwrap();
        assert_eq!(sub.lines, vec![("beans".into(), 1, 1600)]);
        for post in [
            "mode=subscription&qty-pie=0",
            "mode=subscription&mode=subscription",
            "mode=subscription&success_url=https%3A%2F%2Fexample.test",
            "mode=payment",
            "mode=payment&qty-pie=0",
            "mode=payment&qty-pie=6",
            "mode=payment&qty-pie=2.5",
            "mode=payment&qty-pie=1&qty-%70ie=1",
            "mode=payment&qty-pie=1&qty-nope=0",
            "mode=payment&qty-pie=2%XX",
            "mode=payment&qty-pie=5&qty-salad=5",
            "mode=payment&qty-pie=1&qty-salad=5&qty-bread=5",
            "mode=payment&qty-pie=1&cf-turnstile-response=second",
        ] {
            let encoded = format!("{post}&cf-turnstile-response=dummy");
            assert!(parse(encoded.as_bytes()).is_none(), "{post}");
        }
        assert!(parse(b"mode=subscription").is_none());
        assert!(parse(&vec![b'a'; MAX_BODY + 1]).is_none());
    }

    #[test]
    fn only_test_keys_and_bare_trusted_origins() {
        assert!(sandbox_key("sk_test_dummy"));
        assert!(sandbox_key("rk_test_dummy"));
        for key in [
            "sk_live_dummy",
            "pk_test_dummy",
            "rk_test_",
            "sk_test_x\n",
            "sk_test_x:x",
        ] {
            assert!(!sandbox_key(key));
        }
        assert!(return_origin("http://127.0.0.1:8787").is_some());
        assert!(return_origin("https://venue.example").is_some());
        for url in [
            "http://venue.example",
            "https://user@venue.example",
            "https://venue.example/abc",
            "https://venue.example/?x=y",
            "//venue.example",
        ] {
            assert!(return_origin(url).is_none());
        }
    }

    fn price(id: &str, unit: u64, monthly: bool) -> Value {
        json!({"object":"price","id":id,"livemode":false,"active":true,"currency":"usd",
            "unit_amount":unit,"billing_scheme":"per_unit","custom_unit_amount":null,"transform_quantity":null,
            "type":if monthly {"recurring"} else {"one_time"},
            "recurring":if monthly {json!({"interval":"month","interval_count":1,"usage_type":"licensed"})} else {Value::Null}})
    }

    #[test]
    fn mocked_stripe_sessions_validate_fixed_modes_and_fail_closed() {
        for monthly in [true, false] {
            let body: &[u8] = if monthly {
                b"mode=subscription&cf-turnstile-response=dummy"
            } else {
                b"mode=payment&qty-pie=2&qty-salad=1&cf-turnstile-response=dummy"
            };
            let selection = select(body, &catalog()).unwrap();
            let ids = json!({"pie":"price_Pie","salad":"price_Salad","beans":"price_Beans"});
            let origin = return_origin("https://venue.example").unwrap();
            let url = "https://checkout.stripe.com/c/pay/cs_test_Dummy#fidkd-demo%3D";
            let lines: Vec<_> = selection
                .lines
                .iter()
                .map(|(sku, qty, unit)| {
                    json!({
                        "price":price(ids[sku].as_str().unwrap(), *unit, monthly), "quantity":qty,
                        "currency":"usd", "amount_total":qty*unit,
                    })
                })
                .collect();
            let session = json!({"object":"checkout.session","id":"cs_test_Dummy","livemode":false,
                "mode":selection.mode,"status":"open","ui_mode":"hosted","currency":"usd",
                "success_url":"https://venue.example/takeout#returned",
                "cancel_url":if monthly {"https://venue.example/takeout#subscription-heading"} else {"https://venue.example/takeout#pickup-heading"},
                "amount_subtotal":selection.total,"amount_total":selection.total,
                "line_items":{"has_more":false,"data":lines},"url":url});
            for fault in [
                "none",
                "network",
                "live-price",
                "price-amount",
                "price-cadence",
                "live-session",
                "session-mode",
                "amount",
                "line",
                "url",
                "foreign-return",
            ] {
                let mut created = false;
                let result = {
                    let future = pin!(create(&selection, &ids, &origin, |path, post| {
                        if let Some(post) = post {
                            created = true;
                            assert_eq!(path, "/v1/checkout/sessions");
                            let fields: BTreeMap<_, _> =
                                form_urlencoded::parse(post.as_bytes()).collect();
                            assert_eq!(fields["mode"], selection.mode);
                            assert_eq!(
                                fields["success_url"],
                                session["success_url"].as_str().unwrap()
                            );
                            assert_eq!(fields.len(), 7 + 2 * selection.lines.len());
                            for (i, (sku, qty, _)) in selection.lines.iter().enumerate() {
                                assert_eq!(
                                    fields[format!("line_items[{i}][price]").as_str()],
                                    ids[sku].as_str().unwrap()
                                );
                                assert_eq!(
                                    fields[format!("line_items[{i}][quantity]").as_str()],
                                    qty.to_string()
                                );
                            }
                            let mut resp = session.clone();
                            match fault {
                                "live-session" => resp["livemode"] = json!(true),
                                "session-mode" => resp["mode"] = json!("setup"),
                                "amount" => resp["amount_total"] = json!(0),
                                "line" => resp["line_items"]["data"][0]["quantity"] = json!(9),
                                "url" => {
                                    resp["url"] = json!(
                                        "https://checkout.stripe.com.evil.example/c/pay/cs_test_Dummy"
                                    )
                                }
                                "foreign-return" => {
                                    resp["success_url"] = json!("https://evil.example")
                                }
                                _ => (),
                            }
                            ready(Some(resp))
                        } else {
                            let id = path.strip_prefix("/v1/prices/").unwrap();
                            let mut resp = lines
                                .iter()
                                .find(|line| line["price"]["id"] == id)
                                .unwrap()["price"]
                                .clone();
                            match fault {
                                "network" => return ready(None),
                                "live-price" => resp["livemode"] = json!(true),
                                "price-amount" => resp["unit_amount"] = json!(50),
                                "price-cadence" => {
                                    resp["recurring"] = json!({"interval":"year","interval_count":1,"usage_type":"licensed"})
                                }
                                _ => (),
                            }
                            ready(Some(resp))
                        }
                    }));
                    match future.poll(&mut Context::from_waker(Waker::noop())) {
                        Poll::Ready(result) => result,
                        Poll::Pending => panic!("mock transport unexpectedly pending"),
                    }
                };
                assert_eq!(
                    result.as_deref(),
                    (fault == "none").then_some(url),
                    "{fault}"
                );
                assert_eq!(
                    created,
                    !fault.starts_with("price-") && !matches!(fault, "network" | "live-price")
                );
            }
        }
    }
}
