//! Locally reviewed cart-quote bindings for providers whose checkout uses the
//! quote returned by cart creation rather than a second cart-read request.
//! Cache loss or expiry refuses checkout; it never falls back to model totals.
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tool_runtime_core::manifest::McpMoneyUnit;

#[path = "mcp_commerce_store.rs"]
mod store;

const QUOTE_TTL: Duration = Duration::from_secs(600);
const MAX_QUOTES: usize = 256;
const MAX_CONTEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct QuoteBinding {
    pub endpoint_aliases: BTreeSet<String>,
    pub id_pointer: String,
    pub total_pointers: Vec<String>,
    pub money_unit: McpMoneyUnit,
    pub success_pointer: String,
    pub mode_pointer: String,
    pub allowed_modes: BTreeSet<String>,
    pub free_mode: String,
    pub free_flag_pointer: String,
    pub context_arguments: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckoutBinding {
    pub endpoint_aliases: BTreeSet<String>,
    pub quote_tool: String,
    pub id_argument: String,
    pub mode_argument: String,
    pub success_pointer: String,
    /// Set only for a reviewed provider contract that atomically rejects a
    /// repriced, nonzero order. A zero quote alone never grants this exception.
    #[serde(default)]
    pub server_enforces_zero_price: bool,
}

fn pointer(p: &str) -> bool {
    p.starts_with('/') && p.len() <= 4096
}

impl QuoteBinding {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.endpoint_aliases.is_empty()
            || self.endpoint_aliases.len() > 32
            || self.allowed_modes.is_empty()
            || self.allowed_modes.len() > 16
            || !self.allowed_modes.contains(&self.free_mode)
            || self.total_pointers.is_empty()
            || self.total_pointers.len() > 16
            || self.context_arguments.is_empty()
            || self.context_arguments.len() > 64
            || [
                &self.id_pointer,
                &self.success_pointer,
                &self.mode_pointer,
                &self.free_flag_pointer,
            ]
            .into_iter()
            .chain(self.total_pointers.iter())
            .chain(self.context_arguments.iter())
            .any(|p| !pointer(p))
        {
            return Err(
                "MCP cart quotes require bounded aliases, modes and exact JSON pointers".into(),
            );
        }
        Ok(())
    }
    pub(crate) fn applies(&self, alias: Option<&str>) -> bool {
        alias.is_some_and(|a| self.endpoint_aliases.contains(a))
    }
}

impl CheckoutBinding {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.endpoint_aliases.is_empty()
            || self.endpoint_aliases.len() > 32
            || self.quote_tool.is_empty()
            || self.quote_tool.len() > 1024
            || [
                &self.id_argument,
                &self.mode_argument,
                &self.success_pointer,
            ]
            .into_iter()
            .any(|p| !pointer(p))
        {
            return Err(
                "MCP quote checkout requires bounded aliases, tool name and JSON pointers".into(),
            );
        }
        Ok(())
    }
    pub(crate) fn applies(&self, alias: Option<&str>) -> bool {
        alias.is_some_and(|a| self.endpoint_aliases.contains(a))
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct Quote {
    source: String,
    #[serde(
        serialize_with = "serialize_amount",
        deserialize_with = "deserialize_amount"
    )]
    amount_minor: Decimal,
    mode: String,
    free: bool,
    context: BTreeMap<String, Value>,
    #[serde(skip, default = "Instant::now")]
    created: Instant,
    created_unix_ms: u64,
    claimed: bool,
    #[serde(skip)]
    in_flight: bool,
    claim_id: u64,
}

fn serialize_amount<S: serde::Serializer>(
    amount: &Decimal,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&amount.to_string())
}
fn deserialize_amount<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Decimal, D::Error> {
    let text = String::deserialize(deserializer)?;
    text.parse().map_err(serde::de::Error::custom)
}

#[derive(Default)]
pub(crate) struct QuoteCache {
    entries: BTreeMap<(String, String), Quote>,
    next_claim: u64,
}

fn amount_minor(value: &Value, unit: McpMoneyUnit) -> Result<Decimal, String> {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Err("cart quote has no numeric total".into()),
    };
    let mut n: Decimal = text
        .parse()
        .map_err(|_| "cart quote has an invalid total")?;
    if unit == McpMoneyUnit::Major {
        n = n
            .checked_mul(Decimal::from(100))
            .ok_or("cart quote total overflows")?;
    }
    if n < Decimal::ZERO || !n.fract().is_zero() {
        return Err("cart quote total must be nonnegative whole minor units".into());
    }
    Ok(n)
}

impl QuoteCache {
    fn prune(&mut self, now: Instant) {
        self.entries
            .retain(|_, q| q.in_flight || now.saturating_duration_since(q.created) < QUOTE_TTL);
    }

    fn record(
        &mut self,
        scope: &str,
        source: &str,
        binding: &QuoteBinding,
        arguments: &Value,
        documents: &[Value],
        now: Instant,
    ) -> Result<String, String> {
        binding.validate()?;
        self.invalidate(scope, source)?;
        // Structured content wins; JSON text is only the fallback representation.
        let document = documents
            .iter()
            .find(|d| d.pointer(&binding.success_pointer).is_some())
            .ok_or("cart quote has no native success result")?;
        if document
            .pointer(&binding.success_pointer)
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err("cart creation did not succeed".into());
        }
        let id = document
            .pointer(&binding.id_pointer)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 1024)
            .ok_or("cart quote has no valid cart identifier")?;
        let totals = binding
            .total_pointers
            .iter()
            .filter_map(|p| document.pointer(p))
            .filter(|v| !v.is_null())
            .map(|v| amount_minor(v, binding.money_unit))
            .collect::<Result<Vec<_>, _>>()?;
        let amount = *totals
            .first()
            .ok_or("cart quote has no authoritative total")?;
        if totals.iter().any(|n| *n != amount) {
            return Err("cart quote has conflicting authoritative totals".into());
        }
        let mode = document
            .pointer(&binding.mode_pointer)
            .and_then(Value::as_str)
            .filter(|m| binding.allowed_modes.contains(*m))
            .ok_or("cart is not available for checkout")?;
        let free = mode == binding.free_mode;
        if free
            && (amount != Decimal::ZERO
                || document
                    .pointer(&binding.free_flag_pointer)
                    .is_some_and(|v| v.as_bool() != Some(true)))
            || !free && amount <= Decimal::ZERO
        {
            return Err("cart free/paid indicators disagree with its total".into());
        }
        let mut context = BTreeMap::new();
        for p in &binding.context_arguments {
            let value = arguments
                .pointer(p)
                .filter(|v| !v.is_null())
                .ok_or("cart quote is missing selection context")?;
            context.insert(p.clone(), value.clone());
        }
        if serde_json::to_vec(&context)
            .map_err(|_| "cart context cannot be encoded")?
            .len()
            > MAX_CONTEXT_BYTES
        {
            return Err("cart selection context exceeds local bounds".into());
        }
        self.prune(now);
        let key = (scope.to_owned(), id.to_owned());
        if self.entries.get(&key).is_some_and(|q| q.claimed) {
            return Err("cart quote was already used for checkout; verify the existing order before retrying".into());
        }
        if self.entries.len() >= MAX_QUOTES && !self.entries.contains_key(&key) {
            return Err("cart quote cache is full; retry after older quotes expire".into());
        }
        self.entries.insert(
            key,
            Quote {
                source: source.into(),
                amount_minor: amount,
                mode: mode.into(),
                free,
                context,
                created: now,
                created_unix_ms: store::now_ms()?,
                claimed: false,
                in_flight: false,
                claim_id: 0,
            },
        );
        Ok(id.into())
    }

    fn invalidate(&mut self, scope: &str, source: &str) -> Result<(), String> {
        if self
            .entries
            .iter()
            .any(|((s, _), q)| s == scope && q.source == source && q.in_flight)
        {
            return Err(
                "a checkout is already in flight for this account; do not replace its cart".into(),
            );
        }
        self.entries
            .retain(|(s, _), q| s != scope || q.source != source || q.claimed);
        Ok(())
    }

    fn claim(
        &mut self,
        scope: &str,
        binding: &CheckoutBinding,
        arguments: &Value,
        reviewed_minor: Decimal,
        now: Instant,
    ) -> Result<(Decimal, u64), String> {
        binding.validate()?;
        self.prune(now);
        let id = arguments
            .pointer(&binding.id_argument)
            .and_then(Value::as_str)
            .ok_or("checkout requires the quoted cart identifier")?;
        let q = self
            .entries
            .get_mut(&(scope.to_owned(), id.to_owned()))
            .ok_or(
            "checkout requires a fresh cart quote from this account; recreate and review the cart",
        )?;
        if q.claimed {
            return Err("cart quote was already used for checkout; verify the existing order before retrying".into());
        }
        if q.source != binding.quote_tool
            || arguments
                .pointer(&binding.mode_argument)
                .and_then(Value::as_str)
                != Some(q.mode.as_str())
            || q.context
                .iter()
                .any(|(p, v)| arguments.pointer(p) != Some(v))
        {
            return Err(
                "checkout selection or payment mode differs from the reviewed cart quote".into(),
            );
        }
        if reviewed_minor != q.amount_minor
            || reviewed_minor < Decimal::ZERO
            || reviewed_minor.is_zero() && (!q.free || !binding.server_enforces_zero_price)
        {
            return Err("checkout amount differs from the authoritative cart quote".into());
        }
        // A claim is single use, even after a timeout or a lost provider response.
        self.next_claim = self
            .next_claim
            .checked_add(1)
            .ok_or("cart quote claim counter exhausted")?;
        q.claimed = true;
        q.in_flight = true;
        q.claim_id = self.next_claim;
        Ok((q.amount_minor, q.claim_id))
    }
}

pub(crate) struct QuoteUpdate {
    store: store::StoreLease,
    scope: String,
    source: String,
}

pub(crate) fn begin_quote_update(
    storage: &std::path::Path,
    account: &str,
    scope: &str,
    source: &str,
) -> Result<QuoteUpdate, String> {
    let mut store = store::StoreLease::open(storage, account)?;
    // An operation may mutate an existing cart without returning a new quote.
    // Drop every unused quote in this exact session/policy scope beforehand.
    store
        .cache
        .entries
        .retain(|(s, _), q| s != scope || q.claimed);
    store.save()?;
    Ok(QuoteUpdate {
        store,
        scope: scope.into(),
        source: source.into(),
    })
}

pub(crate) fn record_quote(
    update: &mut QuoteUpdate,
    binding: &QuoteBinding,
    arguments: &Value,
    documents: &[Value],
) -> Result<String, String> {
    let id = update.store.cache.record(
        &update.scope,
        &update.source,
        binding,
        arguments,
        documents,
        Instant::now(),
    )?;
    update.store.save()?;
    Ok(id)
}

pub(crate) struct QuoteClaim {
    pub amount_minor: Decimal,
    store: store::StoreLease,
    scope: String,
    id: String,
    claim_id: u64,
}

pub(crate) fn claim_quote(
    storage: &std::path::Path,
    account: &str,
    scope: &str,
    binding: &CheckoutBinding,
    arguments: &Value,
    reviewed_minor: Decimal,
) -> Result<QuoteClaim, String> {
    let mut store = store::StoreLease::open(storage, account)?;
    let (amount_minor, claim_id) =
        store
            .cache
            .claim(scope, binding, arguments, reviewed_minor, Instant::now())?;
    // Persist single-use before the final request can leave this process.
    store.save()?;
    Ok(QuoteClaim {
        amount_minor,
        store,
        scope: scope.into(),
        claim_id,
        id: arguments
            .pointer(&binding.id_argument)
            .and_then(Value::as_str)
            .ok_or("checkout cart id is missing")?
            .into(),
    })
}

/// Only for a proved local refusal, before a request could leave the process.
pub(crate) fn release_claim(claim: &mut QuoteClaim) {
    if let Some(q) = claim
        .store
        .cache
        .entries
        .get_mut(&(claim.scope.clone(), claim.id.clone()))
    {
        if q.claim_id == claim.claim_id {
            q.claimed = false;
            q.in_flight = false;
        }
    }
    if let Err(error) = claim.store.save() {
        tracing::warn!(%error,"local checkout refusal could not release its claim; recreate and review the cart before retrying");
    }
}

pub(crate) fn documents(result: &magician_mcp_client::McpToolCallResult) -> Vec<Value> {
    let mut docs = Vec::new();
    if let Some(value) = &result.structured_content {
        docs.push(value.clone());
    }
    for content in &result.content {
        if let Some(text) = content.get("text").and_then(Value::as_str) {
            if let Ok(value) = serde_json::from_str(text) {
                docs.push(value);
            }
        }
    }
    docs
}

/// None means the remote may have acted but its outcome cannot be proved.
pub(crate) fn checkout_succeeded(binding: &CheckoutBinding, documents: &[Value]) -> Option<bool> {
    documents
        .iter()
        .find_map(|d| d.pointer(&binding.success_pointer))
        .and_then(Value::as_bool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bindings() -> (QuoteBinding, CheckoutBinding) {
        let value: Value = tool_runtime_core::manifest_parser::parse_skill_magician_extension(
            include_str!("../../../../../skillshub/swiggy-mcp/SKILL.md"),
            "mcp_bindings",
        )
        .unwrap()
        .unwrap();
        (
            serde_json::from_value(value["tools"]["create_event_cart"]["commerce_quote"].clone())
                .unwrap(),
            serde_json::from_value(
                value["tools"]["place_event_order"]["commerce_checkout"].clone(),
            )
            .unwrap(),
        )
    }

    fn selection() -> Value {
        json!({"venueId":"venue-1","showId":42,"ticketId":"ticket-1",
        "showTime":1999999999,"guestCount":2,"latitude":12.9,"longitude":77.6})
    }
    fn native(total: Value, mode: &str) -> Value {
        json!({"success":true,"data":{"cartKey":"cart-1",
        "checkoutMode":mode,"cart":{"bill":{"billToPay":total,"skipPayment":mode=="FREE"}}}})
    }
    fn final_arguments(mode: &str) -> Value {
        let mut v = selection();
        v["cartKey"] = json!("cart-1");
        v["paymentMethod"] = json!(mode);
        v
    }

    #[test]
    fn paid_quote_matches_scope_selection_mode_and_amount_and_is_single_use() {
        let (q, b) = bindings();
        let now = Instant::now();
        let mut cache = QuoteCache::default();
        cache
            .record(
                "account-a",
                "create_event_cart",
                &q,
                &selection(),
                &[native(json!(125.50), "UPI")],
                now,
            )
            .unwrap();
        let mut args = final_arguments("UPI");
        assert!(cache
            .claim("account-b", &b, &args, Decimal::from(12550), now)
            .is_err());
        args["guestCount"] = json!(3);
        assert!(cache
            .claim("account-a", &b, &args, Decimal::from(12550), now)
            .is_err());
        args["guestCount"] = json!(2);
        args["paymentMethod"] = json!("FREE");
        assert!(cache
            .claim("account-a", &b, &args, Decimal::from(12550), now)
            .is_err());
        args["paymentMethod"] = json!("UPI");
        assert!(cache
            .claim("account-a", &b, &args, Decimal::from(12549), now)
            .is_err());
        assert_eq!(
            cache
                .claim("account-a", &b, &args, Decimal::from(12550), now)
                .unwrap()
                .0,
            Decimal::from(12550)
        );
        assert!(cache
            .claim("account-a", &b, &args, Decimal::from(12550), now)
            .is_err());
        assert!(cache
            .record(
                "account-a",
                "create_event_cart",
                &q,
                &selection(),
                &[native(json!(125.50), "UPI")],
                now
            )
            .is_err());
    }

    #[test]
    fn free_checkout_requires_native_free_mode_zero_total_and_unchanged_selection() {
        let (q, b) = bindings();
        let now = Instant::now();
        let mut cache = QuoteCache::default();
        assert!(cache
            .record(
                "a",
                "create_event_cart",
                &q,
                &selection(),
                &[native(json!(1), "FREE")],
                now
            )
            .is_err());
        assert!(cache
            .record(
                "a",
                "create_event_cart",
                &q,
                &selection(),
                &[native(json!(0), "UPI")],
                now
            )
            .is_err());
        let mut contradictory = native(json!(0), "FREE");
        contradictory["data"]["cart"]["bill"]["skipPayment"] = json!(false);
        assert!(cache
            .record(
                "a",
                "create_event_cart",
                &q,
                &selection(),
                &[contradictory],
                now
            )
            .is_err());
        let mut free = native(json!(0), "FREE");
        // The provider documents this flag as optional; FREE + exact native zero
        // still uses its atomically zero-enforced final checkout contract.
        free["data"]["cart"]["bill"]
            .as_object_mut()
            .unwrap()
            .remove("skipPayment");
        cache
            .record("a", "create_event_cart", &q, &selection(), &[free], now)
            .unwrap();
        let mut no_zero_guarantee = b.clone();
        no_zero_guarantee.server_enforces_zero_price = false;
        assert!(cache
            .claim(
                "a",
                &no_zero_guarantee,
                &final_arguments("FREE"),
                Decimal::ZERO,
                now
            )
            .is_err());
        assert_eq!(
            cache
                .claim("a", &b, &final_arguments("FREE"), Decimal::ZERO, now)
                .unwrap()
                .0,
            Decimal::ZERO
        );
    }

    #[test]
    fn expired_unavailable_or_replaced_quotes_cannot_checkout() {
        let (q, b) = bindings();
        let now = Instant::now();
        let mut cache = QuoteCache::default();
        cache
            .record(
                "a",
                "create_event_cart",
                &q,
                &selection(),
                &[native(json!(20), "UPI")],
                now,
            )
            .unwrap();
        assert!(cache
            .claim(
                "a",
                &b,
                &final_arguments("UPI"),
                Decimal::from(2000),
                now + QUOTE_TTL
            )
            .is_err());
        cache
            .record(
                "a",
                "create_event_cart",
                &q,
                &selection(),
                &[native(json!(20), "UPI")],
                now,
            )
            .unwrap();
        assert!(cache
            .record(
                "a",
                "create_event_cart",
                &q,
                &selection(),
                &[native(json!(20), "UNAVAILABLE")],
                now
            )
            .is_err());
        assert!(cache
            .claim("a", &b, &final_arguments("UPI"), Decimal::from(2000), now)
            .is_err());
        assert!(cache
            .record(
                "a",
                "create_event_cart",
                &q,
                &selection(),
                &[json!({"success":false})],
                now
            )
            .is_err());
    }

    #[test]
    fn totals_are_exact_and_conflicting_or_subminor_quotes_are_refused() {
        let (q, _) = bindings();
        let now = Instant::now();
        let mut cache = QuoteCache::default();
        assert!(cache
            .record(
                "a",
                "create_event_cart",
                &q,
                &selection(),
                &[native(json!(-1), "UPI")],
                now
            )
            .is_err());
        assert!(cache
            .record(
                "a",
                "create_event_cart",
                &q,
                &selection(),
                &[native(json!(0.001), "UPI")],
                now
            )
            .is_err());
        let mut conflicting = native(json!(20), "UPI");
        conflicting["data"]["cart"]["bill"]["dineoutPricingInfo"] = json!({"billToPay":21});
        assert!(cache
            .record(
                "a",
                "create_event_cart",
                &q,
                &selection(),
                &[conflicting],
                now
            )
            .is_err());
        assert_eq!(
            amount_minor(&json!("123.45"), McpMoneyUnit::Major).unwrap(),
            Decimal::from(12345)
        );
        assert_eq!(
            amount_minor(&json!(12345), McpMoneyUnit::Minor).unwrap(),
            Decimal::from(12345)
        );
    }

    #[test]
    fn checkout_result_does_not_treat_missing_or_failed_native_success_as_success() {
        let (_, b) = bindings();
        assert_eq!(
            checkout_succeeded(&b, &[json!({"success":true})]),
            Some(true)
        );
        assert_eq!(
            checkout_succeeded(&b, &[json!({"success":false})]),
            Some(false)
        );
        assert_eq!(checkout_succeeded(&b, &[json!({"status":"maybe"})]), None);
    }

    #[test]
    fn cache_and_context_remain_bounded() {
        let (q, _) = bindings();
        let now = Instant::now();
        let mut cache = QuoteCache::default();
        for i in 0..MAX_QUOTES {
            cache
                .record(
                    &format!("scope-{i}"),
                    "create_event_cart",
                    &q,
                    &selection(),
                    &[native(json!(20), "UPI")],
                    now,
                )
                .unwrap();
        }
        assert!(cache
            .record(
                "extra",
                "create_event_cart",
                &q,
                &selection(),
                &[native(json!(20), "UPI")],
                now
            )
            .is_err());
        assert_eq!(cache.entries.len(), MAX_QUOTES);
        let mut huge = selection();
        huge["venueId"] = json!("x".repeat(MAX_CONTEXT_BYTES + 1));
        assert!(cache
            .record(
                "big",
                "create_event_cart",
                &q,
                &huge,
                &[native(json!(20), "UPI")],
                now
            )
            .is_err());
        cache.prune(now + QUOTE_TTL);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn durable_quotes_survive_a_worker_handoff_and_serialize_cart_operations() {
        let (q, b) = bindings();
        let root = tempfile::tempdir().unwrap();
        let account = "a".repeat(64);
        let scope = "worker-handoff-fixture";
        let mut update =
            begin_quote_update(root.path(), &account, scope, "create_event_cart").unwrap();
        assert!(begin_quote_update(root.path(), &account, scope, "create_event_cart").is_err());
        record_quote(&mut update, &q, &selection(), &[native(json!(20), "UPI")]).unwrap();
        assert!(claim_quote(
            root.path(),
            &account,
            scope,
            &b,
            &final_arguments("UPI"),
            Decimal::from(2000)
        )
        .is_err());
        drop(update);
        // Reload from the host-owned file, as a different stateless worker does.
        let mut first = claim_quote(
            root.path(),
            &account,
            scope,
            &b,
            &final_arguments("UPI"),
            Decimal::from(2000),
        )
        .unwrap();
        assert!(begin_quote_update(root.path(), &account, scope, "create_event_cart").is_err());
        release_claim(&mut first);
        assert!(claim_quote(
            root.path(),
            &account,
            scope,
            &b,
            &final_arguments("UPI"),
            Decimal::from(2000)
        )
        .is_err());
        drop(first);
        let second = claim_quote(
            root.path(),
            &account,
            scope,
            &b,
            &final_arguments("UPI"),
            Decimal::from(2000),
        )
        .unwrap();
        drop(second);
        // A timeout/crash after dispatch cannot turn the same cart into a new order.
        assert!(claim_quote(
            root.path(),
            &account,
            scope,
            &b,
            &final_arguments("UPI"),
            Decimal::from(2000)
        )
        .is_err());
        drop(begin_quote_update(root.path(), &account, scope, "create_event_cart").unwrap());
    }

    #[test]
    fn corrupted_or_symlinked_host_state_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let account = "b".repeat(64);
        drop(begin_quote_update(root.path(), &account, "fixture", "create_event_cart").unwrap());
        let state = root
            .path()
            .join(format!("system/mcp-commerce/{account}.json"));
        std::fs::write(&state, b"{broken").unwrap();
        assert!(begin_quote_update(root.path(), &account, "fixture", "create_event_cart").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::{fs::symlink, fs::PermissionsExt};
            assert_eq!(
                std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
                0o600
            );
            std::fs::remove_file(&state).unwrap();
            symlink(root.path().join("missing-target"), &state).unwrap();
            assert!(
                begin_quote_update(root.path(), &account, "fixture", "create_event_cart").is_err()
            );
        }
    }

    #[test]
    fn new_or_unclassified_mutations_invalidate_previously_prepared_carts() {
        let (q, b) = bindings();
        let root = tempfile::tempdir().unwrap();
        let account = "d".repeat(64);
        let scope = "future-cart-mutation";
        let mut create =
            begin_quote_update(root.path(), &account, scope, "create_event_cart").unwrap();
        record_quote(&mut create, &q, &selection(), &[native(json!(20), "UPI")]).unwrap();
        drop(create);
        drop(begin_quote_update(root.path(), &account, scope, "new_cart_mutator").unwrap());
        assert!(claim_quote(
            root.path(),
            &account,
            scope,
            &b,
            &final_arguments("UPI"),
            Decimal::from(2000)
        )
        .is_err());
    }

    #[test]
    fn account_lease_is_exclusive_in_another_process() {
        let root = tempfile::tempdir().unwrap();
        let account = "c".repeat(64);
        let update =
            begin_quote_update(root.path(), &account, "fixture", "create_event_cart").unwrap();
        let test_module = module_path!().split_once("::").unwrap().1;
        let test_name = format!("{test_module}::lease_child_probe");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name.as_str(), "--nocapture"])
            .env("MAGICIAN_QUOTE_LEASE_TEST_ROOT", root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        drop(update);
        drop(begin_quote_update(root.path(), &account, "fixture", "create_event_cart").unwrap());
    }

    #[test]
    fn lease_child_probe() {
        if let Some(root) = std::env::var_os("MAGICIAN_QUOTE_LEASE_TEST_ROOT") {
            assert!(begin_quote_update(
                std::path::Path::new(&root),
                &"c".repeat(64),
                "fixture",
                "create_event_cart"
            )
            .is_err());
        }
    }
}
