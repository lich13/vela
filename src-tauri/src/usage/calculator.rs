use super::model::{decimal, failure};
use crate::storage::Result;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    pub base_price: String,
    pub model_ratio: String,
    pub output_ratio: String,
    pub cache_read_ratio: String,
    pub cache_write_5m_ratio: String,
    pub cache_write_1h_ratio: String,
    pub group_ratio: String,
    #[serde(default)]
    pub rates: Option<[String; 5]>,
    pub tokens: [String; 5],
}

#[derive(Debug, Serialize)]
pub struct Calculation {
    pub rates: [String; 5],
    pub costs: [String; 5],
    pub total: String,
}

pub fn calculate(input: Input) -> Result<Calculation> {
    let parse = |value: &str| decimal(value).ok_or_else(|| failure("价格和倍率须为非负数"));
    let invalid = || failure("换算数值超过可计算范围");
    let parse_tokens = |value: &str| -> Result<u64> {
        let trimmed = value.trim();
        if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
            return Err(failure("Token 数须为有效的非负整数"));
        }
        trimmed
            .parse::<u64>()
            .map_err(|_| failure("Token 数须为有效的非负整数"))
    };
    let base = parse(&input.base_price)?
        .checked_mul(parse(&input.model_ratio)?)
        .ok_or_else(invalid)?;
    let ratios = [
        Decimal::ONE,
        parse(&input.output_ratio)?,
        parse(&input.cache_read_ratio)?,
        parse(&input.cache_write_5m_ratio)?,
        parse(&input.cache_write_1h_ratio)?,
    ];
    let group = parse(&input.group_ratio)?;
    let explicit_rates = match &input.rates {
        Some(raw) => Some([
            parse(&raw[0])?,
            parse(&raw[1])?,
            parse(&raw[2])?,
            parse(&raw[3])?,
            parse(&raw[4])?,
        ]),
        None => None,
    };
    let mut rates = std::array::from_fn(|_| String::new());
    let mut costs = rates.clone();
    let mut total = Decimal::ZERO;
    for (index, ratio) in ratios.into_iter().enumerate() {
        let count = parse_tokens(&input.tokens[index])?;
        let rate = match explicit_rates {
            Some(explicit) => explicit[index],
            None => base.checked_mul(ratio).ok_or_else(invalid)?,
        };
        let cost = rate
            .checked_mul(Decimal::from(count))
            .and_then(|v| v.checked_div(Decimal::from(1_000_000)))
            .and_then(|v| v.checked_mul(group))
            .ok_or_else(invalid)?;
        rates[index] = rate.normalize().to_string();
        costs[index] = cost.normalize().to_string();
        total = total.checked_add(cost).ok_or_else(invalid)?;
    }
    Ok(Calculation {
        rates,
        costs,
        total: total.normalize().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Input {
        Input {
            base_price: "2".into(),
            model_ratio: "4".into(),
            output_ratio: "5".into(),
            cache_read_ratio: "0.025".into(),
            cache_write_5m_ratio: "1.25".into(),
            cache_write_1h_ratio: "2".into(),
            group_ratio: "1".into(),
            rates: None,
            tokens: [
                "2".into(),
                "1500".into(),
                "16000".into(),
                "176000".into(),
                "241416".into(),
            ],
        }
    }

    #[test]
    fn provider_ratios_and_explicit_rates_match_five_token_charges_exactly() {
        let result = calculate(input()).unwrap();
        assert_eq!(result.rates, ["8", "40", "0.2", "10", "16"]);
        assert_eq!(
            result.costs,
            ["0.000016", "0.06", "0.0032", "1.76", "3.862656"]
        );
        assert_eq!(result.total, "5.685872");
        let mut discounted = input();
        discounted.group_ratio = "0.5".into();
        assert_eq!(calculate(discounted).unwrap().total, "2.842936");

        // Explicit unit prices avoid repeating-decimal truncation (e.g. $1 / $3 = 0.33333333...).
        let mut exact = input();
        exact.base_price = "3".into();
        exact.model_ratio = "1".into();
        exact.cache_read_ratio = "0.33333333".into();
        exact.rates = Some([
            "3".into(),
            "15".into(),
            "1".into(),
            "3.75".into(),
            "6".into(),
        ]);
        exact.tokens = [
            "0".into(),
            "0".into(),
            "3000000".into(),
            "0".into(),
            "0".into(),
        ];
        assert_eq!(calculate(exact).unwrap().total, "3");
    }

    #[test]
    fn zero_is_allowed_and_invalid_tokens_or_overflowing_input_is_rejected() {
        let mut value = input();
        value.group_ratio = "0".into();
        assert_eq!(calculate(value).unwrap().total, "0");
        for ratio in ["", "-1", "NaN", "999999999999999999999999999999"] {
            let mut value = input();
            value.model_ratio = ratio.into();
            assert!(calculate(value).is_err());
        }
        for bad_token in ["", "-1", "1.5", "abc", "999999999999999999999999999"] {
            let mut value = input();
            value.tokens[0] = bad_token.into();
            let err = calculate(value).unwrap_err();
            assert_eq!(err.message, "Token 数须为有效的非负整数");
        }
    }

    #[test]
    fn selected_supplier_and_used_models_query_before_and_after_rollup() {
        use super::super::{model::*, pricing::Pricing, query, store::Store};
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let prices = Pricing::new(dir.path()).unwrap();
        let at = 20_000 * 86_400_000_i64;
        let attempt = |provider: &str, model: &str, count: u64| Attempt {
            id: format!("fixture-{provider}-{model}"),
            provider: Some(provider.into()),
            response_model: Some(model.into()),
            pricing_model: Some(model.into()),
            started_at: at,
            status: Some(200),
            outcome: "success".into(),
            tokens: Tokens {
                input: Some(count),
                output: Some(1),
                cache_read: Some(2),
                cache_write: Some(8),
                cache_write_5m: Some(3),
                cache_write_1h: Some(5),
                ..Default::default()
            },
            ..Default::default()
        };
        let rows = [
            Record {
                id: "fixture-failover".into(),
                client: "claude".into(),
                source: "proxy".into(),
                started_at: at,
                completed: true,
                attempts: vec![
                    attempt("fixture-A", "claude-sonnet-4-6", 11),
                    attempt("fixture-B", "claude-sonnet-4-6", 22),
                ],
                ..Default::default()
            },
            Record {
                id: "fixture-other-model".into(),
                client: "claude".into(),
                source: "proxy".into(),
                started_at: at,
                completed: true,
                attempts: vec![attempt("fixture-A", "fixture-unpriced", 33)],
                ..Default::default()
            },
        ];
        store.write_batch(&rows, None).unwrap();
        let filter = Filter {
            client: Some("claude".into()),
            provider: Some("fixture-A".into()),
            model: Some("claude-sonnet-4-6".into()),
            ..Default::default()
        };
        for rollup in [false, true] {
            if rollup {
                store.compact(at + 40 * 86_400_000).unwrap();
            }
            let selected = query::calculator_totals(&store.connection, &filter).unwrap();
            assert_eq!(selected.attempts, 1);
            assert_eq!(selected.tokens.input, Some(11));
            assert_eq!(selected.tokens.cache_write_5m, Some(3));
            assert_eq!(selected.tokens.cache_write_1h, Some(5));
            let models = query::calculator_models(&store.connection, "claude").unwrap();
            assert_eq!(models, vec!["claude-sonnet-4-6", "fixture-unpriced"]);
            assert!(prices
                .quote(Some(&models[0]), "1")
                .million_rates()
                .is_some());
            assert!(prices
                .quote(Some(&models[1]), "1")
                .million_rates()
                .is_none());
            assert_eq!(
                query::calculator_totals(
                    &store.connection,
                    &Filter {
                        model: None,
                        ..filter.clone()
                    }
                )
                .unwrap()
                .tokens
                .input,
                Some(44)
            );
        }
    }
}
