// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Golden files written by the original Python (`fixtures/packing_slip/make_golden.py`): the
//! Rust renderer must reproduce the PDFs byte for byte, and `normalize_order` must reproduce the
//! normalized orders (and error texts) of the Python for shop payloads and odd inputs.
use printpapi_server::orders::{Order, OrderLine, normalize_order};
use printpapi_server::packing_slip::render_packing_slip;
use serde_json::Value;
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packing_slip");
    let path = dir.join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn json(name: &str) -> Value {
    serde_json::from_slice(&fixture(name)).unwrap()
}

/// The normalized dict as the Python wrote it -> `Order`, field by field (no re-normalizing).
fn order_from_json(v: &Value) -> Order {
    let text = |v: &Value, k: &str| v.get(k).map(|s| s.as_str().unwrap().to_string());
    let list = |k: &str| v[k].as_array().unwrap().iter();
    Order {
        number: text(v, "number").unwrap(),
        date: text(v, "date"),
        shop: text(v, "shop"),
        customer: text(v, "customer"),
        email: text(v, "email"),
        phone: text(v, "phone"),
        note: text(v, "note"),
        source_id: text(v, "source_id"),
        address: list("address")
            .map(|a| a.as_str().unwrap().into())
            .collect(),
        lines: list("lines")
            .map(|l| OrderLine {
                qty: text(l, "qty"),
                sku: text(l, "sku"),
                name: text(l, "name"),
                total: text(l, "total"),
            })
            .collect(),
        totals: list("totals")
            .map(|t| (t[0].as_str().unwrap().into(), t[1].as_str().unwrap().into()))
            .collect(),
    }
}

fn assert_same_pdf(case: &str, order: &Order) {
    let got = render_packing_slip(order);
    let want = fixture(&format!("{case}.pdf"));
    assert!(
        got == want,
        "{case}: PDF differs from the Python output ({} vs {} bytes)",
        got.len(),
        want.len()
    );
}

#[test]
fn direct_orders_render_byte_identical_pdfs() {
    for case in [
        "minimal",
        "full",
        "multi_page",
        "long_names",
        "non_latin1",
        "parens_backslashes",
        "wide_amounts",
        "totals_page_break",
    ] {
        assert_same_pdf(case, &order_from_json(&json(&format!("{case}.json"))));
    }
}

#[test]
fn shop_payloads_normalize_and_render_like_the_python() {
    for case in ["shopify", "woocommerce"] {
        let raw = json(&format!("{case}.raw.json"));
        let got = normalize_order(&raw["payload"], &raw["format"]).unwrap();
        assert_eq!(
            got,
            order_from_json(&json(&format!("{case}.json"))),
            "{case}"
        );
        assert_same_pdf(case, &got);
    }
}

#[test]
fn normalize_order_matches_the_python_on_odd_inputs() {
    let cases = json("orders_cases.json");
    let cases = cases.as_array().unwrap();
    assert!(cases.len() > 30);
    for (i, case) in cases.iter().enumerate() {
        let got = normalize_order(&case["payload"], &case["format"]);
        let label = format!("case {i}: {} / {}", case["format"], case["payload"]);
        match (case.get("error"), case.get("expected")) {
            (Some(msg), _) => assert_eq!(got.unwrap_err().0, msg.as_str().unwrap(), "{label}"),
            (None, Some(want)) => {
                let got = got.unwrap_or_else(|e| panic!("{label}: {e}"));
                assert_eq!(got, order_from_json(want), "{label}");
            }
            _ => unreachable!(),
        }
    }
}
