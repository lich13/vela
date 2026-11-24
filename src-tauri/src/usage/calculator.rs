use super::model::{decimal, failure};
use crate::storage::Result;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct Input {
    pub rates: [String; 5],
    pub tokens: [String; 5],
}

#[derive(Debug, Serialize)]
pub struct Calculation {
    pub rates: [String; 5],
    pub costs: [String; 5],
    pub total: String,
}

pub fn calculate(input: Input) -> Result<Calculation> {
    let invalid = || failure("换算数值超过可计算范围");
    let mut rates = std::array::from_fn(|_| String::new());
    let mut costs = rates.clone();
    let mut total = Decimal::ZERO;
    for index in 0..5 {
        let rate = decimal(&input.rates[index]).ok_or_else(|| failure("单价须为非负数"))?;
        let tokens = input.tokens[index].trim();
        let count = tokens
            .parse::<u64>()
            .ok()
            .filter(|_| tokens.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| failure("Token 数须为有效的非负整数"))?;
        let cost = rate
            .checked_mul(Decimal::from(count))
            .and_then(|v| v.checked_div(Decimal::from(1_000_000)))
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
            rates: ["8", "40", "0.2", "10", "16"].map(String::from),
            tokens: ["2", "1500", "16000", "176000", "241416"].map(String::from),
        }
    }

    #[test]
    fn unit_prices_match_five_token_charges_exactly() {
        let result = calculate(input()).unwrap();
        assert_eq!(result.rates, ["8", "40", "0.2", "10", "16"]);
        assert_eq!(
            result.costs,
            ["0.000016", "0.06", "0.0032", "1.76", "3.862656"]
        );
        assert_eq!(result.total, "5.685872");

        let exact = Input {
            rates: ["3", "15", "1", "3.75", "6"].map(String::from),
            tokens: ["0", "0", "3000000", "0", "0"].map(String::from),
        };
        assert_eq!(calculate(exact).unwrap().total, "3");
    }

    #[test]
    fn zero_is_allowed_and_invalid_prices_tokens_or_overflowing_input_are_rejected() {
        let mut value = input();
        value.rates = ["0", "0", "0", "0", "0"].map(String::from);
        assert_eq!(calculate(value).unwrap().total, "0");
        for rate in ["", "-1", "NaN", "999999999999999999999999999999"] {
            let mut value = input();
            value.rates[1] = rate.into();
            assert_eq!(calculate(value).unwrap_err().message, "单价须为非负数");
        }
        for bad_token in ["", "-1", "1.5", "abc", "999999999999999999999999999"] {
            let mut value = input();
            value.tokens[0] = bad_token.into();
            let err = calculate(value).unwrap_err();
            assert_eq!(err.message, "Token 数须为有效的非负整数");
        }
        let mut value = input();
        value.rates[0] = "79228162514264337593543950335".into();
        value.tokens[0] = "18446744073709551615".into();
        assert_eq!(
            calculate(value).unwrap_err().message,
            "换算数值超过可计算范围"
        );
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
