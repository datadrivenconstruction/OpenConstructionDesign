// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! What a server says about money: prices and context sizes in its model list, the balance left
//! on the account and on the key, and the errors of a spent key or an empty account. The reply
//! shapes of a chat-completions model gateway are read here and nowhere else; a server that reports
//! none of them simply gets no prices and no balance.

use crate::provider::ProviderError;
use serde_json::Value as J;

/// Default address of the model gateway preset: one key, many models.
pub const GATEWAY_URL: &str = "https://openrouter.ai/api/v1";

/// One entry of a model list. Prices are per million tokens in the server's billing currency.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub context_tokens: Option<u32>,
    /// The most the model may write in one reply (`top_provider.max_completion_tokens`).
    pub max_output_tokens: Option<u32>,
    pub price_in_per_mtok: Option<f64>,
    pub price_out_per_mtok: Option<f64>,
}

impl ModelInfo {
    pub fn named(id: impl Into<String>) -> Self {
        ModelInfo {
            id: id.into(),
            ..ModelInfo::default()
        }
    }
}

/// Money left, in the server's billing currency; `None` where the server did not say.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Balance {
    /// On the account: bought minus spent.
    pub account: Option<f64>,
    /// Under the key's own spending limit; `None` also for a key without one.
    pub key: Option<f64>,
}

impl Balance {
    pub fn is_empty(&self) -> bool {
        self.account.is_none() && self.key.is_none()
    }

    /// What can still be spent through this key: the smaller of the two that are known.
    pub fn spendable(&self) -> Option<f64> {
        match (self.account, self.key) {
            (Some(a), Some(k)) => Some(a.min(k)),
            (a, k) => a.or(k),
        }
    }
}

/// Paths below the base address that report the balance (account, key).
pub const CREDITS_PATH: &str = "credits";
pub const KEY_PATH: &str = "key";

/// `data[]` or `models[]`, each with `id` (or `name`); prices per token as `pricing.prompt` and
/// `pricing.completion` (text or number), the context as `context_length`. Sorted by id.
pub fn parse_model_infos(doc: &J) -> Vec<ModelInfo> {
    let list = doc
        .get("data")
        .or_else(|| doc.get("models"))
        .and_then(J::as_array);
    let mut out: Vec<ModelInfo> = list
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m.get("id").or_else(|| m.get("name")).and_then(J::as_str)?;
            let price = |k: &str| per_mtok(m.get("pricing").and_then(|p| p.get(k)));
            Some(ModelInfo {
                id: id.to_string(),
                context_tokens: m
                    .get("context_length")
                    .and_then(J::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|&n| n > 0),
                max_output_tokens: m
                    .get("top_provider")
                    .and_then(|t| t.get("max_completion_tokens"))
                    .and_then(J::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|&n| n > 0),
                price_in_per_mtok: price("prompt"),
                price_out_per_mtok: price("completion"),
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// A per-token price as text or number, per million tokens; a negative one means "varies".
fn per_mtok(v: Option<&J>) -> Option<f64> {
    let v = match v? {
        J::String(s) => s.trim().parse::<f64>().ok()?,
        J::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (v.is_finite() && v >= 0.0).then(|| (v * 1e6 * 1e6).round() / 1e6)
}

/// The account balance from `{"data":{"total_credits":..,"total_usage":..}}`.
pub fn parse_credits(doc: &J) -> Option<f64> {
    let d = doc.get("data")?;
    let bought = d.get("total_credits")?.as_f64()?;
    let used = d.get("total_usage").and_then(J::as_f64).unwrap_or(0.0);
    Some(bought - used)
}

/// What is left under the key's own limit, from `{"data":{"limit_remaining":..}}`; a key
/// without a limit reports `null`.
pub fn parse_key_left(doc: &J) -> Option<f64> {
    doc.get("data")?.get("limit_remaining")?.as_f64()
}

/// A refusal for money, from the status and the error text: the key's own limit is spent, or
/// the account has nothing left. Checked before the status alone decides (a spent key comes as
/// 403, which otherwise reads as a rejected key).
pub fn funds_error(status: u16, body: &str) -> Option<ProviderError> {
    let t = body.to_ascii_lowercase();
    let detail = || body.trim().chars().take(200).collect::<String>();
    if t.contains("key limit exceeded") {
        return Some(ProviderError::KeyLimit(detail()));
    }
    let no_credit = status == 402
        || t.contains("insufficient credit")
        || t.contains("insufficient_quota")
        || t.contains("insufficient balance")
        || t.contains("insufficient funds")
        || t.contains("credit balance is too low");
    no_credit.then(|| ProviderError::NoCredit(detail()))
}

/// Cost of a run from token counts and per-million prices; `None` without both prices.
pub fn cost_of(
    tokens_in: u64,
    tokens_out: u64,
    price_in: Option<f64>,
    price_out: Option<f64>,
) -> Option<f64> {
    Some((tokens_in as f64 * price_in? + tokens_out as f64 * price_out?) / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_list_prices_are_per_token_and_become_per_million() {
        let doc = json!({"data": [
            {"id": "b/model", "context_length": 200000, "top_provider": {"max_completion_tokens": 32000},
             "pricing": {"prompt": "0.000003", "completion": "0.000015"}},
            {"id": "a/free", "pricing": {"prompt": "0", "completion": 0}},
            {"id": "c/varies", "pricing": {"prompt": "-1", "completion": "x"}},
            {"name": "d/plain"}
        ]});
        let m = parse_model_infos(&doc);
        let ids: Vec<&str> = m.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["a/free", "b/model", "c/varies", "d/plain"]);
        assert_eq!(m[1].price_in_per_mtok, Some(3.0));
        assert_eq!(m[1].price_out_per_mtok, Some(15.0));
        assert_eq!(m[1].context_tokens, Some(200_000));
        assert_eq!((m[1].max_output_tokens, m[0].max_output_tokens), (Some(32_000), None));
        assert_eq!(m[0].price_in_per_mtok, Some(0.0));
        assert_eq!(
            (m[2].price_in_per_mtok, m[2].price_out_per_mtok),
            (None, None)
        );
        assert_eq!(m[3], ModelInfo::named("d/plain"));
    }

    #[test]
    fn a_spent_key_and_an_empty_account_are_told_apart_from_a_bad_key() {
        assert!(matches!(
            funds_error(403, r#"{"error":{"message":"Key limit exceeded"}}"#),
            Some(ProviderError::KeyLimit(_))
        ));
        assert!(matches!(
            funds_error(402, r#"{"error":{"message":"Insufficient credits"}}"#),
            Some(ProviderError::NoCredit(_))
        ));
        assert!(matches!(
            funds_error(429, r#"{"error":{"type":"insufficient_quota"}}"#),
            Some(ProviderError::NoCredit(_))
        ));
        assert_eq!(funds_error(403, "forbidden"), None);
        assert_eq!(funds_error(401, "invalid api key"), None);
        assert_eq!(funds_error(429, "rate limit"), None);
        assert_eq!(funds_error(429, "Rate limit exceeded for this key"), None);
    }

    #[test]
    fn balance_reads_account_and_key_and_keeps_the_smaller() {
        let c = json!({"data": {"total_credits": 10.0, "total_usage": 7.5}});
        assert_eq!(parse_credits(&c), Some(2.5));
        assert_eq!(parse_credits(&json!({"error": "no"})), None);
        assert_eq!(
            parse_key_left(&json!({"data": {"limit_remaining": 1.25}})),
            Some(1.25)
        );
        assert_eq!(
            parse_key_left(&json!({"data": {"limit_remaining": null}})),
            None
        );
        let b = Balance {
            account: Some(2.5),
            key: Some(1.25),
        };
        assert_eq!(b.spendable(), Some(1.25));
        assert_eq!(
            Balance {
                account: Some(2.5),
                key: None
            }
            .spendable(),
            Some(2.5)
        );
        assert!(Balance::default().is_empty());
        assert_eq!(
            cost_of(1_000_000, 500_000, Some(3.0), Some(15.0)),
            Some(10.5)
        );
        assert_eq!(cost_of(1, 1, None, Some(1.0)), None);
    }
}
