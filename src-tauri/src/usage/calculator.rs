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
    pub cache_write_ratio: String,
    pub group_ratio: String,
    pub tokens: [u64; 4],
}

#[derive(Serialize)]
pub struct Calculation {
    pub rates: [String; 4],
    pub costs: [String; 4],
    pub total: String,
}

pub fn calculate(input: Input) -> Result<Calculation> {
    let parse = |value: &str| decimal(value).ok_or_else(|| failure("价格和倍率须为非负数"));
    let invalid = || failure("换算数值超过可计算范围");
    let base = parse(&input.base_price)?
        .checked_mul(parse(&input.model_ratio)?)
        .ok_or_else(invalid)?;
    let ratios = [
        Decimal::ONE,
        parse(&input.output_ratio)?,
        parse(&input.cache_read_ratio)?,
        parse(&input.cache_write_ratio)?,
    ];
    let group = parse(&input.group_ratio)?;
    let mut rates = std::array::from_fn(|_| String::new());
    let mut costs = rates.clone();
    let mut total = Decimal::ZERO;
    for (index, ratio) in ratios.into_iter().enumerate() {
        let rate = base.checked_mul(ratio).ok_or_else(invalid)?;
        let cost = rate
            .checked_mul(Decimal::from(input.tokens[index]))
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
            cache_write_ratio: "1.25".into(),
            group_ratio: "1".into(),
            tokens: [2, 1500, 16000, 176000],
        }
    }

    #[test]
    fn provider_ratios_match_the_four_separate_token_charges_exactly() {
        let result = calculate(input()).unwrap();
        assert_eq!(result.rates, ["8", "40", "0.2", "10"]);
        assert_eq!(result.costs, ["0.000016", "0.06", "0.0032", "1.76"]);
        assert_eq!(result.total, "1.823216");
        let mut discounted = input();
        discounted.group_ratio = "0.5".into();
        assert_eq!(calculate(discounted).unwrap().total, "0.911608");
    }

    #[test]
    fn zero_is_allowed_and_invalid_or_overflowing_input_is_rejected() {
        let mut value = input();
        value.group_ratio = "0".into();
        assert_eq!(calculate(value).unwrap().total, "0");
        for ratio in ["", "-1", "NaN", "999999999999999999999999999999"] {
            let mut value = input();
            value.model_ratio = ratio.into();
            assert!(calculate(value).is_err());
        }
    }

    #[test]
    fn selected_supplier_counts_its_own_attempts_before_and_after_rollup() {
        use super::super::{model::*, query, store::Store};
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
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
                cache_write: Some(3),
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
                    attempt("fixture-A", "fixture-model", 11),
                    attempt("fixture-B", "fixture-model", 22),
                ],
                ..Default::default()
            },
            Record {
                id: "fixture-other-model".into(),
                client: "claude".into(),
                source: "proxy".into(),
                started_at: at,
                completed: true,
                attempts: vec![attempt("fixture-A", "fixture-other", 33)],
                ..Default::default()
            },
        ];
        store.write_batch(&rows, None).unwrap();
        let filter = Filter {
            client: Some("claude".into()),
            provider: Some("fixture-A".into()),
            model: Some("fixture-model".into()),
            ..Default::default()
        };
        for rollup in [false, true] {
            if rollup {
                store.compact(at + 40 * 86_400_000).unwrap();
            }
            let selected = query::calculator_totals(&store.connection, &filter).unwrap();
            assert_eq!(selected.attempts, 1);
            assert_eq!(selected.tokens.input, Some(11));
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
            assert_eq!(
                query::calculator_totals(
                    &store.connection,
                    &Filter {
                        start: Some(at + 86_400_000),
                        ..filter.clone()
                    }
                )
                .unwrap()
                .attempts,
                0
            );
        }
    }
}
