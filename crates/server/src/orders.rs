// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Store order payloads -> the normalized order the packing slip prints — the port of
//! `app/orders.py`. Pure logic, no IO.
//!
//! The mapping is the part that differs per shop system, and the part most likely to need fixing
//! when a shop sends something odd, so it stays testable on its own.
//!
//! Amounts are passed through as the shop wrote them (the shop already did the arithmetic). The
//! one exception is a Shopify line amount: Shopify only sends a unit price, so qty x price is
//! computed in decimal here.
use crate::pyfmt::{get, py_str, repr, truthy};
use rust_decimal::{Decimal, RoundingStrategy};
use serde_json::{Value, json};

/// A packing slip, not a warehouse report — also caps the PDF size.
pub const MAX_LINES: usize = 1000;
/// Per-field character cap; the slip truncates to the column anyway.
const MAX_FIELD: usize = 200;

/// The normalized order. A field is `None`/empty exactly when the Python dict lacked the key.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Order {
    pub number: String,
    pub date: Option<String>,
    pub shop: Option<String>,
    pub customer: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub note: Option<String>,
    pub source_id: Option<String>,
    pub address: Vec<String>,
    pub lines: Vec<OrderLine>,
    /// [label, amount] pairs, amount non-empty.
    pub totals: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrderLine {
    pub qty: Option<String>,
    pub sku: Option<String>,
    pub name: Option<String>,
    pub total: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderError(pub String);

impl std::fmt::Display for OrderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OrderError {}

fn err<T>(msg: impl Into<String>) -> Result<T, OrderError> {
    Err(OrderError(msg.into()))
}

/// Python `str.strip()`: Unicode whitespace, which also counts the C0 separators U+001C..U+001F.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// `_s`: `None` -> "", else `str(v).strip()` cut to 200 characters.
fn s(v: &Value) -> String {
    if v.is_null() {
        return String::new();
    }
    py_strip(&py_str(v)).chars().take(MAX_FIELD).collect()
}

/// `str[:n]` on characters.
fn head(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

// --- decimals --------------------------------------------------------------------------------

/// A parsed `decimal.Decimal` literal: finite (sign, coefficient digits, exponent) or special.
#[derive(Debug, Clone, PartialEq)]
enum Parsed {
    Finite { neg: bool, coef: String, exp: i64 },
    Nan { neg: bool, payload: String },
    Inf { neg: bool },
}

/// Python `Decimal(text)` grammar: optional sign, digits with an optional point, optional
/// exponent, or inf/infinity/nan/snan; underscores are dropped anywhere.
// ponytail: ASCII digits only — Python also accepts other Unicode decimal digits.
fn parse_decimal(text: &str) -> Option<Parsed> {
    let t: String = py_strip(text).chars().filter(|c| *c != '_').collect();
    let (neg, rest) = match t.as_bytes().first()? {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t.as_str()),
    };
    let lower = rest.to_ascii_lowercase();
    if lower == "inf" || lower == "infinity" {
        return Some(Parsed::Inf { neg });
    }
    let nan_tail = lower
        .strip_prefix("nan")
        .or_else(|| lower.strip_prefix("snan"));
    if let Some(tail) = nan_tail {
        return tail
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| Parsed::Nan {
                neg,
                payload: tail.trim_start_matches('0').to_string(),
            });
    }
    let (mant, exp_part) = match lower.split_once('e') {
        Some((m, e)) => (m, Some(e)),
        None => (lower.as_str(), None),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let digits_only = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
    if int.len() + frac.len() == 0 || !digits_only(int) || !digits_only(frac) {
        return None;
    }
    let mut exp: i64 = 0;
    if let Some(e) = exp_part {
        let (eneg, digits) = match e.as_bytes().first()? {
            b'-' => (true, &e[1..]),
            b'+' => (false, &e[1..]),
            _ => (false, e),
        };
        if digits.is_empty() || !digits_only(digits) {
            return None;
        }
        let n: i64 = digits.parse().ok()?;
        exp = if eneg { -n } else { n };
    }
    let all = format!("{int}{frac}");
    let coef = all.trim_start_matches('0');
    let coef = if coef.is_empty() { "0" } else { coef };
    Some(Parsed::Finite {
        neg,
        coef: coef.to_string(),
        exp: exp.checked_sub(frac.len() as i64)?,
    })
}

/// A decimal value: what `_dec` yields.
#[derive(Debug, Clone)]
enum Dec {
    Fin(Decimal),
    Nan { neg: bool, payload: String },
    Inf { neg: bool },
}

const ZERO: Dec = Dec::Fin(Decimal::ZERO);

/// `_dec`: `Decimal(_s(v) or "0")`, 0 when it is not a number. `None` = a number `Decimal`
/// holds in Python but `rust_decimal` (28 digits, 96 bit) cannot.
// ponytail: values beyond rust_decimal's range (e.g. 1e40, 1e-30) are out of reach — the
// callers fall back to printing the shop's own text.
fn dec(v: &Value) -> Option<Dec> {
    let t = s(v);
    let t = if t.is_empty() { "0" } else { &t };
    match parse_decimal(t) {
        None => Some(ZERO),
        Some(Parsed::Nan { neg, payload }) => Some(Dec::Nan { neg, payload }),
        Some(Parsed::Inf { neg }) => Some(Dec::Inf { neg }),
        Some(Parsed::Finite { neg, coef, exp }) => {
            let mut m: i128 = coef.parse().ok()?;
            let scale = if exp <= 0 {
                u32::try_from(-exp).ok()?
            } else {
                if m != 0 {
                    m = m.checked_mul(10i128.checked_pow(u32::try_from(exp).ok()?)?)?;
                }
                0
            };
            if neg {
                m = -m;
            }
            Decimal::try_from_i128_with_scale(m, scale)
                .ok()
                .map(Dec::Fin)
        }
    }
}

/// Python `str(Decimal)` for a finite value (scientific form when the exponent is positive or
/// the adjusted exponent is below -6).
fn decimal_str(d: &Decimal) -> String {
    let m = d.mantissa();
    let coef = m.unsigned_abs().to_string();
    let exp = -(d.scale() as i64);
    let adjusted = exp + coef.len() as i64 - 1;
    let sign = if m < 0 || (m == 0 && d.is_sign_negative()) {
        "-"
    } else {
        ""
    };
    if exp <= 0 && adjusted >= -6 {
        if exp == 0 {
            return format!("{sign}{coef}");
        }
        let point = coef.len() as i64 + exp;
        return if point > 0 {
            let (a, b) = coef.split_at(point as usize);
            format!("{sign}{a}.{b}")
        } else {
            format!("{sign}0.{}{coef}", "0".repeat((-point) as usize))
        };
    }
    let (first, rest) = coef.split_at(1);
    let dot = if rest.is_empty() {
        String::new()
    } else {
        format!(".{rest}")
    };
    let e = if adjusted == 0 {
        String::new()
    } else {
        format!("E{adjusted:+}")
    };
    format!("{sign}{first}{dot}{e}")
}

fn dec_str(d: Dec) -> String {
    match d {
        Dec::Fin(d) => decimal_str(&d),
        Dec::Nan { neg, payload } => format!("{}NaN{payload}", if neg { "-" } else { "" }),
        Dec::Inf { neg } => if neg { "-Infinity" } else { "Infinity" }.into(),
    }
}

/// `int(qty or 0)`; `None` = Python's ValueError/TypeError (or beyond i64: out of reach).
fn int_qty(qty: &Value) -> Option<i64> {
    if !truthy(qty) {
        return Some(0);
    }
    match qty {
        Value::Bool(_) => Some(1),
        Value::Number(n) => n.as_i64().or_else(|| {
            n.as_f64()
                .filter(|f| f.abs() < 9.2e18)
                .map(|f| f.trunc() as i64)
        }),
        Value::String(t) => {
            let t = py_strip(t);
            let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
            // Python's int(): a single '_' only between two digits.
            let b = digits.as_bytes();
            let ok = !b.is_empty()
                && b.iter().enumerate().all(|(i, c)| {
                    c.is_ascii_digit()
                        || (*c == b'_'
                            && i > 0
                            && b[i - 1].is_ascii_digit()
                            && b.get(i + 1).is_some_and(u8::is_ascii_digit))
                });
            if !ok {
                return None;
            }
            let clean: String = t.chars().filter(|c| *c != '_').collect();
            clean.parse().ok()
        }
        _ => None,
    }
}

/// `_line_total`: `price x qty` to 2 places (half-even); the shop's own price text when qty or
/// price is not arithmetic.
fn line_total(price: &Value, qty: &Value) -> String {
    let fallback = || s(price);
    let (Some(p), Some(q)) = (dec(price), int_qty(qty)) else {
        return fallback();
    };
    match p {
        nan @ Dec::Nan { .. } => dec_str(nan),
        // Python raises InvalidOperation for Infinity x 0 (no response); here: NaN.
        Dec::Inf { .. } if q == 0 => "NaN".into(),
        Dec::Inf { neg } => dec_str(Dec::Inf {
            neg: neg != (q < 0),
        }),
        Dec::Fin(d) => match d.checked_mul(Decimal::from(q)) {
            Some(t) => format!(
                "{:.2}",
                t.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven)
            ),
            None => fallback(),
        },
    }
}

/// `sum(_dec(price) ...)` as `str(Decimal)`; the scale of the widest term is kept ("4.90"), a
/// zero term's included. `None` when a term or the sum is beyond `rust_decimal`'s range: the
/// caller drops the row rather than print a smaller number that looks valid.
fn sum_prices(prices: impl Iterator<Item = Value>) -> Option<String> {
    let mut total = Decimal::ZERO;
    let mut scale = 0;
    let mut nan: Option<Dec> = None;
    let (mut pos_inf, mut neg_inf) = (false, false);
    for p in prices {
        match dec(&p)? {
            n @ Dec::Nan { .. } => {
                nan.get_or_insert(n);
            }
            Dec::Inf { neg: true } => neg_inf = true,
            Dec::Inf { neg: false } => pos_inf = true,
            Dec::Fin(d) => {
                scale = scale.max(d.scale());
                total = total.checked_add(d)?;
            }
        }
    }
    // Python raises InvalidOperation for Infinity + -Infinity (no response); here: NaN.
    Some(match (nan, pos_inf, neg_inf) {
        (Some(n), ..) => dec_str(n),
        (_, true, true) => "NaN".into(),
        (_, true, _) => "Infinity".into(),
        (_, _, true) => "-Infinity".into(),
        _ => {
            // rust_decimal's x + 0 returns x unchanged, so the zero term's scale is restored here.
            total.rescale(scale);
            if total.scale() != scale {
                return None;
            }
            decimal_str(&total)
        }
    })
}

/// `_money`: '19.80' -> '19.80 EUR'. Empty/zero -> `None`, so no '0.00' rows get printed; a
/// value that is not a number is printed verbatim.
fn money(value: &Value, currency: &str) -> Option<String> {
    let text = s(value);
    if text.is_empty() {
        return None;
    }
    if let Some(Parsed::Finite { coef, .. }) = parse_decimal(&text)
        && coef == "0"
    {
        return None;
    }
    Some(py_strip(&format!("{text} {currency}")).to_string())
}

fn money_v(v: &Value, currency: &str) -> Value {
    money(v, currency).map_or(Value::Null, Value::String)
}

fn text(t: String) -> Value {
    Value::String(t)
}

// --- shop payloads -----------------------------------------------------------------------------

/// `p.get(key) or {}`. A sub-object that is not an object reads as `{}` (Python crashed).
fn obj<'a>(p: &'a Value, key: &str) -> &'a Value {
    let v = get(p, key);
    if v.is_object() { v } else { &Value::Null }
}

/// `p.get(key) or []`, objects only. A list or item of the wrong type reads as absent
/// (Python crashed on those).
fn objects<'a>(p: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    get(p, key)
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v.is_object())
}

fn address(parts: [String; 2], city_line: String, country: String) -> Value {
    let all = parts
        .into_iter()
        .chain([city_line, country])
        .filter(|p| !p.is_empty());
    Value::Array(all.map(Value::String).collect())
}

/// `"zip city"` plus `", province"` unless the province repeats the city.
fn city_line(zip: String, city: String, region: String) -> String {
    let line = [zip, city.clone()]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !region.is_empty() && region != city {
        return format!("{line}, {region}")
            .trim_start_matches([',', ' '])
            .to_string();
    }
    line
}

fn from_shopify(p: &Value) -> Value {
    let ship = obj(p, "shipping_address");
    let cust = obj(p, "customer");
    let mut name = s(get(ship, "name"));
    if name.is_empty() {
        let full = format!(
            "{} {}",
            s(get(cust, "first_name")),
            s(get(cust, "last_name"))
        );
        name = s(&Value::String(full));
    }
    let currency = s(get(p, "currency"));
    let city = city_line(
        s(get(ship, "zip")),
        s(get(ship, "city")),
        s(get(ship, "province")),
    );

    let lines: Vec<Value> = objects(p, "line_items")
        .map(|it| {
            let mut title = s(get(it, "title"));
            let variant = s(get(it, "variant_title"));
            if !variant.is_empty() && variant != "Default Title" {
                title = format!("{title} ({variant})");
            }
            let total = line_total(get(it, "price"), get(it, "quantity"));
            json!({"qty": get(it, "quantity"), "sku": get(it, "sku"), "name": title,
                   "total": money_v(&text(total), &currency)})
        })
        .collect();

    let shipping = sum_prices(objects(p, "shipping_lines").map(|l| get(l, "price").clone()));
    let totals: Vec<Value> = [
        ("Subtotal", get(p, "subtotal_price").clone()),
        ("Shipping", text(shipping.unwrap_or_default())),
        ("Tax", get(p, "total_tax").clone()),
        ("Total", get(p, "total_price").clone()),
    ]
    .into_iter()
    .map(|(label, v)| json!([label, money_v(&v, &currency)]))
    .collect();

    let name_field = s(get(p, "name"));
    let number = if name_field.is_empty() {
        format!("#{}", s(get(p, "order_number")))
    } else {
        name_field
    };
    let email = s(get(p, "email"));
    let phone = s(get(ship, "phone"));
    json!({
        "number": number,
        "date": head(&s(get(p, "created_at")), 10),
        "customer": name,
        "address": address([s(get(ship, "address1")), s(get(ship, "address2"))], city,
                           s(get(ship, "country"))),
        "email": if email.is_empty() { s(get(cust, "email")) } else { email },
        "phone": if phone.is_empty() { s(get(cust, "phone")) } else { phone },
        "note": s(get(p, "note")),
        "source_id": s(get(p, "id")),
        "lines": lines,
        "totals": totals,
    })
}

fn from_woocommerce(p: &Value) -> Value {
    let ship = obj(p, "shipping");
    let bill = obj(p, "billing");
    let who = if !s(get(ship, "first_name")).is_empty() || !s(get(ship, "last_name")).is_empty() {
        ship
    } else {
        bill
    };
    let currency = s(get(p, "currency"));
    let city = city_line(
        s(get(who, "postcode")),
        s(get(who, "city")),
        s(get(who, "state")),
    );

    let totals: Vec<Value> = [
        ("Discount", "discount_total"),
        ("Shipping", "shipping_total"),
        ("Tax", "total_tax"),
        ("Total", "total"),
    ]
    .into_iter()
    .map(|(label, key)| json!([label, money_v(get(p, key), &currency)]))
    .collect();
    let lines: Vec<Value> = objects(p, "line_items")
        .map(|it| {
            json!({"qty": get(it, "quantity"), "sku": get(it, "sku"), "name": get(it, "name"),
                   "total": money_v(get(it, "total"), &currency)})
        })
        .collect();

    let number = s(get(p, "number"));
    let customer = format!("{} {}", s(get(who, "first_name")), s(get(who, "last_name")));
    json!({
        "number": if number.is_empty() { s(get(p, "id")) } else { number },
        "date": head(&s(get(p, "date_created")), 10),
        "customer": s(&Value::String(customer)),
        "address": address([s(get(who, "address_1")), s(get(who, "address_2"))], city,
                           s(get(who, "country"))),
        "email": s(get(bill, "email")),
        "phone": s(get(bill, "phone")),
        "note": s(get(p, "customer_note")),
        "source_id": s(get(p, "id")),
        "lines": lines,
        "totals": totals,
    })
}

/// Map a store payload (`fmt`) — or validate an already-normalized order (`fmt` null) — into
/// the order the packing slip renders. `fmt` is the raw `format` value: "shopify" or
/// "woocommerce" map that shop's JSON, anything else is `unknown order format: {fmt!r} (...)`.
pub fn normalize_order(payload: &Value, fmt: &Value) -> Result<Order, OrderError> {
    let mapped;
    let payload = if fmt.is_null() {
        payload
    } else {
        let mapper: fn(&Value) -> Value = match fmt.as_str() {
            Some("shopify") => from_shopify,
            Some("woocommerce") => from_woocommerce,
            _ => {
                return err(format!(
                    "unknown order format: {} (expected one of shopify, woocommerce)",
                    repr(fmt)
                ));
            }
        };
        if !payload.is_object() {
            return err("order must be an object");
        }
        mapped = mapper(payload);
        &mapped
    };
    if !payload.is_object() {
        return err("order must be an object");
    }

    let mut out = Order::default();
    let field = |k: &str| Some(s(get(payload, k))).filter(|v| !v.is_empty());
    out.date = field("date");
    out.shop = field("shop");
    out.customer = field("customer");
    out.email = field("email");
    out.phone = field("phone");
    out.note = field("note");
    out.source_id = field("source_id");
    let Some(number) = field("number") else {
        return err("order needs a 'number'");
    };
    out.number = number;

    let address = get(payload, "address");
    if !address.is_null() && !address.is_array() {
        return err("'address' must be a list of lines");
    }
    out.address = address
        .as_array()
        .into_iter()
        .flatten()
        .take(10)
        .map(s)
        .filter(|a| !a.is_empty())
        .collect();

    let lines = get(payload, "lines");
    if !lines.is_null() && !lines.is_array() {
        return err("'lines' must be a list");
    }
    let lines = lines.as_array().map_or(&[][..], Vec::as_slice);
    if lines.len() > MAX_LINES {
        return err(format!(
            "too many line items ({}, max {MAX_LINES})",
            lines.len()
        ));
    }
    out.lines = lines
        .iter()
        .filter(|it| it.is_object())
        .map(|it| {
            let f = |k: &str| Some(s(get(it, k))).filter(|v| !v.is_empty());
            OrderLine {
                qty: f("qty"),
                sku: f("sku"),
                name: f("name"),
                total: f("total"),
            }
        })
        .collect();

    let totals = get(payload, "totals");
    if !totals.is_null() && !totals.is_array() {
        return err("'totals' must be a list of [label, amount] pairs");
    }
    out.totals = totals
        .as_array()
        .into_iter()
        .flatten()
        .take(20)
        .filter_map(|t| match t.as_array().map(Vec::as_slice) {
            Some([label, amount]) => Some((s(label), s(amount))).filter(|(_, a)| !a.is_empty()),
            _ => None,
        })
        .collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shopify() -> Value {
        json!({
            "id": 450789469, "order_number": 1001, "name": "#1001",
            "created_at": "2026-07-25T10:22:51-04:00", "currency": "EUR",
            "subtotal_price": "29.70", "total_tax": "0.00", "total_price": "34.60",
            "note": "leave at the door", "email": "jane@example.com",
            "customer": {"first_name": "Jane", "last_name": "Doe"},
            "shipping_address": {"name": "Jane Doe", "address1": "Musterweg 1", "address2": "",
                                 "zip": "12345", "city": "Berlin", "country": "Germany",
                                 "phone": "+49 30 123456"},
            "shipping_lines": [{"title": "Standard", "price": "4.90"}],
            "line_items": [
                {"quantity": 2, "sku": "A-1", "title": "Widget", "variant_title": "blue",
                 "price": "9.90"},
                {"quantity": 1, "sku": "B-2", "title": "Gadget",
                 "variant_title": "Default Title", "price": "9.90"}],
        })
    }

    fn woo() -> Value {
        json!({
            "id": 727, "number": "727", "date_created": "2026-07-25T10:22:51",
            "currency": "EUR", "total": "34.60", "shipping_total": "4.90",
            "total_tax": "0.00", "discount_total": "0.00", "customer_note": "leave at the door",
            "billing": {"email": "jane@example.com", "phone": "+49 30 123456"},
            "shipping": {"first_name": "Jane", "last_name": "Doe", "address_1": "Musterweg 1",
                         "address_2": "", "postcode": "12345", "city": "Berlin",
                         "country": "DE"},
            "line_items": [{"name": "Widget", "quantity": 2, "sku": "A-1", "total": "19.80"},
                           {"name": "Gadget", "quantity": 1, "sku": "B-2", "total": "9.90"}],
        })
    }

    fn norm(p: &Value, fmt: &str) -> Order {
        normalize_order(p, &json!(fmt)).unwrap()
    }

    fn totals(o: &Order) -> Vec<(&str, &str)> {
        o.totals
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect()
    }

    #[test]
    fn shopify_order_maps_to_the_slip_shape() {
        let o = norm(&shopify(), "shopify");
        assert_eq!(o.number, "#1001");
        assert_eq!(o.date.as_deref(), Some("2026-07-25"));
        assert_eq!(o.customer.as_deref(), Some("Jane Doe"));
        assert_eq!(o.address, ["Musterweg 1", "12345 Berlin", "Germany"]);
        assert_eq!(o.email.as_deref(), Some("jane@example.com"));
        assert_eq!(o.phone.as_deref(), Some("+49 30 123456"));
        assert_eq!(o.note.as_deref(), Some("leave at the door"));
        // variant is appended, except Shopify's "Default Title" placeholder
        let names: Vec<_> = o.lines.iter().map(|l| l.name.as_deref().unwrap()).collect();
        assert_eq!(names, ["Widget (blue)", "Gadget"]);
        // Shopify only sends a unit price, so the line amount is qty x price.
        assert_eq!(
            o.lines[0],
            OrderLine {
                qty: Some("2".into()),
                sku: Some("A-1".into()),
                name: Some("Widget (blue)".into()),
                total: Some("19.80 EUR".into())
            }
        );
        assert_eq!(
            totals(&o),
            [
                ("Subtotal", "29.70 EUR"),
                ("Shipping", "4.90 EUR"),
                ("Total", "34.60 EUR")
            ]
        );
        assert_eq!(o.source_id.as_deref(), Some("450789469"));
    }

    #[test]
    fn woocommerce_order_maps_to_the_slip_shape() {
        let o = norm(&woo(), "woocommerce");
        assert_eq!(o.number, "727");
        assert_eq!(o.date.as_deref(), Some("2026-07-25"));
        assert_eq!(o.customer.as_deref(), Some("Jane Doe"));
        assert_eq!(o.address, ["Musterweg 1", "12345 Berlin", "DE"]);
        assert_eq!(
            o.lines[0],
            OrderLine {
                qty: Some("2".into()),
                sku: Some("A-1".into()),
                name: Some("Widget".into()),
                total: Some("19.80 EUR".into())
            }
        );
        assert_eq!(
            totals(&o),
            [("Shipping", "4.90 EUR"), ("Total", "34.60 EUR")]
        );
        assert_eq!(o.source_id.as_deref(), Some("727"));
    }

    #[test]
    fn zero_and_missing_amounts_are_dropped_not_printed_as_zero() {
        let mut p = woo();
        p["shipping_total"] = json!("0.00");
        p["total_tax"] = json!("2.10");
        let o = norm(&p, "woocommerce");
        let labels: Vec<_> = o.totals.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["Tax", "Total"]); // no 0.00 shipping row
    }

    #[test]
    fn already_normalized_orders_pass_through_validated() {
        let p = json!({"number": 55, "lines": [{"qty": 1, "name": "Thing"}],
                       "totals": [["Total", "1.00"]], "extra": "ignored"});
        let o = normalize_order(&p, &Value::Null).unwrap();
        assert_eq!(o.number, "55");
        assert_eq!(
            o.lines,
            [OrderLine {
                qty: Some("1".into()),
                name: Some("Thing".into()),
                ..Default::default()
            }]
        );
        assert_eq!(totals(&o), [("Total", "1.00")]);
    }

    fn msg(p: &Value, fmt: &Value) -> String {
        normalize_order(p, fmt).unwrap_err().0
    }

    #[test]
    fn rejects_junk() {
        let null = Value::Null;
        assert_eq!(msg(&json!("not a dict"), &null), "order must be an object");
        assert_eq!(msg(&json!({}), &null), "order needs a 'number'");
        assert_eq!(
            msg(&json!({"number": "1", "lines": {}}), &null),
            "'lines' must be a list"
        );
        assert_eq!(
            msg(&shopify(), &json!("magento")),
            "unknown order format: 'magento' (expected one of shopify, woocommerce)"
        );
        assert_eq!(
            msg(&json!({}), &json!("")),
            "unknown order format: '' (expected one of shopify, woocommerce)"
        );
        assert_eq!(
            msg(&json!({}), &json!(5)),
            "unknown order format: 5 (expected one of shopify, woocommerce)"
        );
        assert_eq!(
            msg(&json!([1]), &json!("shopify")),
            "order must be an object"
        );
        assert_eq!(
            msg(&json!({"number": "1", "address": "x"}), &null),
            "'address' must be a list of lines"
        );
        assert_eq!(
            msg(&json!({"number": "1", "totals": 3}), &null),
            "'totals' must be a list of [label, amount] pairs"
        );
    }

    #[test]
    fn line_count_is_capped() {
        let lines: Vec<_> = (0..1001)
            .map(|i| json!({"qty": 1, "name": format!("x{i}")}))
            .collect();
        let huge = json!({"number": "1", "lines": lines});
        assert_eq!(
            msg(&huge, &Value::Null),
            "too many line items (1001, max 1000)"
        );
    }

    #[test]
    fn shopify_without_shipping_address_falls_back_to_the_customer() {
        let mut p = shopify();
        p.as_object_mut().unwrap().remove("shipping_address");
        let o = norm(&p, "shopify");
        assert_eq!(o.customer.as_deref(), Some("Jane Doe"));
        assert!(o.address.is_empty());
    }

    // --- edge cases beyond the Python tests; expected values come from the Python original ---

    #[test]
    fn caps_and_trimming() {
        let long = "é".repeat(300);
        let p = json!({"number": format!("  {long}  "), "address": (0..12).map(|i| format!("a{i}")).collect::<Vec<_>>(),
                       "totals": (0..25).map(|i| json!([format!("l{i}"), "1"])).collect::<Vec<_>>()});
        let o = normalize_order(&p, &Value::Null).unwrap();
        assert_eq!(o.number.chars().count(), 200);
        assert_eq!(o.address.len(), 10);
        assert_eq!(o.totals.len(), 20);
        // malformed totals pairs and empty amounts are skipped, non-object lines too
        let p = json!({"number": "1", "totals": [["a"], ["a", "b", "c"], ["a", ""], ["a", null], "x", ["ok", 2.5]],
                       "lines": [1, "x", {}, {"qty": 0}]});
        let o = normalize_order(&p, &Value::Null).unwrap();
        assert_eq!(totals(&o), [("ok", "2.5")]);
        assert_eq!(o.lines.len(), 2);
        assert_eq!(
            o.lines[1],
            OrderLine {
                qty: Some("0".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn money_forms() {
        assert_eq!(money(&json!(""), "EUR"), None);
        assert_eq!(money(&json!("0"), "EUR"), None);
        assert_eq!(money(&json!("-0.00"), "EUR"), None);
        assert_eq!(money(&json!("0e5"), "EUR"), None);
        assert_eq!(money(&json!("0_0.0"), "EUR"), None);
        assert_eq!(money(&json!("12.5"), ""), Some("12.5".into()));
        assert_eq!(money(&json!("1e3"), "EUR"), Some("1e3 EUR".into()));
        assert_eq!(money(&json!("free"), "EUR"), Some("free EUR".into()));
        assert_eq!(money(&json!("NaN"), "EUR"), Some("NaN EUR".into()));
        assert_eq!(
            money(&json!("-Infinity"), "EUR"),
            Some("-Infinity EUR".into())
        );
        assert_eq!(money(&json!("1e"), "EUR"), Some("1e EUR".into()));
        assert_eq!(money(&json!("5."), "EUR"), Some("5. EUR".into()));
    }

    #[test]
    fn line_totals() {
        let t = |p: Value, q: Value| line_total(&p, &q);
        assert_eq!(t(json!("9.90"), json!(2)), "19.80");
        assert_eq!(t(json!("0.125"), json!(1)), "0.12"); // half-even
        assert_eq!(t(json!("0.135"), json!(1)), "0.14");
        assert_eq!(t(json!("2.675"), json!("3")), "8.02");
        assert_eq!(t(json!(9.5), json!(2.9)), "19.00"); // float qty truncates
        assert_eq!(t(json!("5"), json!(true)), "5.00");
        assert_eq!(t(json!("5"), json!(null)), "0.00");
        assert_eq!(t(json!(null), json!(3)), "0.00");
        assert_eq!(t(json!("abc"), json!(3)), "0.00"); // _dec -> 0
        assert_eq!(t(json!("1e1"), json!(2)), "20.00");
        assert_eq!(t(json!("5"), json!(" 3 ")), "15.00");
        assert_eq!(t(json!("-2.50"), json!(-2)), "5.00");
        assert_eq!(t(json!("1.5"), json!("2.5")), "1.5"); // int("2.5") fails -> the price text
        assert_eq!(t(json!("1.5"), json!([1])), "1.5");
        assert_eq!(t(json!("1.5"), json!("x")), "1.5");
        assert_eq!(t(json!("NaN"), json!(2)), "NaN");
        assert_eq!(t(json!("Infinity"), json!(2)), "Infinity");
        assert_eq!(t(json!("-inf"), json!(2)), "-Infinity");
        assert_eq!(t(json!("5"), json!(1.0e30)), "5"); // beyond i64: the price text
    }

    #[test]
    fn shipping_sum_keeps_the_scale_of_a_zero_term() {
        let v = |xs: &[Value]| sum_prices(xs.iter().cloned());
        assert_eq!(v(&[json!("4.9"), json!("0.00")]), Some("4.90".into()));
        assert_eq!(v(&[json!(4.9), json!("0.00")]), Some("4.90".into()));
        assert_eq!(v(&[json!("5"), json!("0.000")]), Some("5.000".into()));
        assert_eq!(v(&[json!("0.00"), json!("5")]), Some("5.00".into()));
        assert_eq!(v(&[json!("10"), json!("0.0")]), Some("10.0".into()));
        assert_eq!(
            v(&[json!("4.9"), json!("0.000"), json!("1.1")]),
            Some("6.000".into())
        );
    }

    #[test]
    fn shipping_sum_out_of_range_is_dropped_not_understated() {
        let v = |xs: &[Value]| sum_prices(xs.iter().cloned());
        assert_eq!(v(&[json!("988508"), json!("78E4680")]), None);
        assert_eq!(v(&[json!("5e28"), json!("5e28")]), None);
        let mut p = shopify();
        p["shipping_lines"] = json!([{"price": "988508"}, {"price": "78E4680"}]);
        let o = norm(&p, "shopify");
        assert!(!o.totals.iter().any(|(l, _)| l == "Shipping"));
    }

    #[test]
    fn int_qty_underscores_only_between_digits() {
        let q = |x: &str| int_qty(&json!(x));
        assert_eq!(q("1_0"), Some(10));
        assert_eq!(q("-1_000"), Some(-1000));
        for bad in ["1__0", "_1", "1_", "+_1", "9_", "_862", "-_1", "1_ 0"] {
            assert_eq!(q(bad), None, "{bad}");
        }
        assert_eq!(line_total(&json!("2"), &json!("_862")), "2");
        assert_eq!(line_total(&json!("5"), &json!("1__0")), "5");
    }

    #[test]
    fn long_json_floats_keep_pythons_repr() {
        // needs serde_json's float_roundtrip: 17+ digit floats parse correctly rounded
        let v: Value = serde_json::from_str(r#"{"n": 9.83761507836480295e11}"#).unwrap();
        assert_eq!(s(&v["n"]), "983761507836.4803");
        let v: Value = serde_json::from_str(r#"{"n": 4.591538835074704103e1}"#).unwrap();
        assert_eq!(s(&v["n"]), "45.915388350747044");
    }

    #[test]
    fn nan_keeps_sign_and_payload() {
        let t = |p: &str, q: i64| line_total(&json!(p), &json!(q));
        assert_eq!(t("-NaN", 2), "-NaN");
        assert_eq!(t("NaN12", 2), "NaN12");
        assert_eq!(t("nan007", 2), "NaN7");
        assert_eq!(t("-nan0", 2), "-NaN");
        let v = |xs: &[Value]| sum_prices(xs.iter().cloned());
        assert_eq!(v(&[json!("-NaN")]), Some("-NaN".into()));
        assert_eq!(v(&[json!("NaN5")]), Some("NaN5".into()));
        assert_eq!(
            v(&[json!("1"), json!("-nan5"), json!("nan3")]),
            Some("-NaN5".into())
        );
    }

    #[test]
    fn shipping_sum_keeps_the_widest_scale() {
        let v = |xs: &[Value]| sum_prices(xs.iter().cloned());
        assert_eq!(v(&[]), Some("0".into()));
        assert_eq!(v(&[json!("4.90")]), Some("4.90".into()));
        assert_eq!(v(&[json!("4.9"), json!("1.25")]), Some("6.15".into()));
        assert_eq!(
            v(&[json!("4.90"), json!("abc"), json!(null)]),
            Some("4.90".into())
        );
        assert_eq!(v(&[json!("0.0000001")]), Some("1E-7".into()));
        assert_eq!(v(&[json!("1e2")]), Some("100".into()));
        assert_eq!(v(&[json!(3)]), Some("3".into()));
        assert_eq!(v(&[json!("NaN")]), Some("NaN".into()));
        let mut p = shopify();
        p["shipping_lines"] = json!([{"price": "0.00"}, {"price": "0"}, 5, "x"]);
        let o = norm(&p, "shopify");
        assert!(!o.totals.iter().any(|(l, _)| l == "Shipping"));
    }

    #[test]
    fn non_object_sub_objects_read_as_empty() {
        let p = json!({"id": 1, "shipping_address": "oops", "customer": [1], "line_items": "x",
                       "shipping_lines": 3});
        let o = norm(&p, "shopify");
        assert_eq!(o.number, "#");
        assert!(o.address.is_empty() && o.lines.is_empty());
    }

    #[test]
    fn province_and_city_line() {
        let mut p = shopify();
        p["shipping_address"]["province"] = json!("Brandenburg");
        assert_eq!(norm(&p, "shopify").address[1], "12345 Berlin, Brandenburg");
        p["shipping_address"]["province"] = json!("Berlin");
        assert_eq!(norm(&p, "shopify").address[1], "12345 Berlin");
        p["shipping_address"]["zip"] = json!("");
        p["shipping_address"]["city"] = json!("");
        p["shipping_address"]["province"] = json!("Bayern");
        assert_eq!(norm(&p, "shopify").address[1], "Bayern");
    }

    #[test]
    fn woocommerce_falls_back_to_billing_name() {
        let p = json!({"id": 9, "billing": {"first_name": "Bob", "last_name": "B", "city": "Köln",
                       "state": "NRW"}, "shipping": {"city": "Elsewhere"}});
        let o = norm(&p, "woocommerce");
        assert_eq!(o.number, "9");
        assert_eq!(o.customer.as_deref(), Some("Bob B"));
        assert_eq!(o.address, ["Köln, NRW"]);
    }
}
