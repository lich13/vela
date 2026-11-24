import { useEffect, useState } from "react";
import { command } from "./bridge";
import { Drawer } from "./Drawer";
import { billTime, localSecond } from "./UsageBill";
import { errorOf, type ClientId } from "./types";
import {
  numeric,
  type CalculatorModel,
  type CostCalculation,
  type UsageFilter,
} from "./usage-types";

type FiveTuple = [string, string, string, string, string];

const clients: [ClientId, string][] = [
  ["claude", "Claude Code"],
  ["codex", "Codex"],
  ["grok", "Grok Build"],
  ["dsh", "DeepSeek Harness"],
];
const quantities = [
  "输入",
  "输出",
  "缓存读取",
  "创建 5 分钟缓存",
  "创建 1 小时缓存",
] as const;

const today = () => new Date().setHours(0, 0, 0, 0);

export default function UsageCostCalculator({
  filter,
  close,
}: {
  filter: UsageFilter;
  close: () => void;
}) {
  const [client, setClient] = useState<ClientId>(() =>
    clients.some(([id]) => id === filter.client)
      ? (filter.client as ClientId)
      : "claude",
  );
  const [provider, setProvider] = useState(filter.provider ?? "");
  const [providers, setProviders] = useState<{ id: string; name: string }[]>(
    [],
  );
  const [models, setModels] = useState<CalculatorModel[]>([]);
  const [model, setModel] = useState("");
  const [from, setFrom] = useState(() => localSecond(filter.start ?? today()));
  const [to, setTo] = useState(() => localSecond(filter.end ?? Date.now()));
  const [unitPrices, setUnitPrices] = useState<FiveTuple>([
    "2",
    "2",
    "2",
    "2",
    "2",
  ]);
  const [result, setResult] = useState<
    (CostCalculation & { tokens: FiveTuple }) | null
  >(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let active = true;
    setProviders([]);
    setModels([]);
    void Promise.all([
      command("get_gateway", { clientId: client }).catch(() => null),
      command("get_usage_calculator_models", { client }).catch(() => []),
    ]).then(([view, usedModels]) => {
      if (!active) return;
      if (view) {
        setProviders(view.providers.map(({ id, name }) => ({ id, name })));
      }
      setModels(usedModels);
    });
    return () => {
      active = false;
    };
  }, [client]);

  const calculate = async () => {
    const start = billTime(from, "start"),
      end = billTime(to, "end");
    if (start === null || end === null) {
      setError("请填写有效的开始和结束时间");
      return;
    }
    if (start > end) {
      setError("开始时间不能晚于结束时间");
      return;
    }
    setBusy(true);
    setError("");
    try {
      const totals = await command("get_usage_calculator_totals", {
        filter: {
          start,
          end,
          client,
          provider,
          model: model.trim() || undefined,
          source: "proxy",
        },
      });
      if (!totals.attempts) throw new Error("所选周期没有符合条件的网关用量");
      const t = totals.tokens;
      // Writes reported without a duration go to the 5 minute price.
      const aggregate = t.cacheWrite ?? 0;
      let cw5m = t.cacheWrite5m ?? 0;
      let cw1h = t.cacheWrite1h ?? 0;
      const sum = cw5m + cw1h;
      if (aggregate > 0 && sum > aggregate) {
        cw5m = Number(
          (BigInt(aggregate) * BigInt(cw5m) + BigInt(sum) / 2n) / BigInt(sum),
        );
        cw1h = aggregate - cw5m;
      } else if (aggregate > sum) {
        cw5m += aggregate - sum;
      }
      const tokens: FiveTuple = [
        String(t.input ?? 0),
        String(t.output ?? 0),
        String(t.cacheRead ?? 0),
        String(cw5m),
        String(cw1h),
      ];
      const cost = await command("calculate_usage_cost", {
        input: { rates: unitPrices, tokens },
      });
      setResult({ ...cost, tokens });
    } catch (e) {
      setError(errorOf(e).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Drawer title="倍率换算" onClose={close}>
      <form
        className="usage-calculator"
        noValidate
        onSubmit={(event) => {
          event.preventDefault();
          setResult(null);
          void calculate();
        }}
        onChange={() => setResult(null)}
      >
        <fieldset disabled={busy}>
          <div className="usage-price-fields">
            <label className="usage-field">
              客户端
              <select
                value={client}
                onChange={(e) => {
                  setClient(e.target.value as ClientId);
                  setProvider("");
                  setError("");
                }}
              >
                {clients.map(([id, label]) => (
                  <option key={id} value={id}>
                    {label}
                  </option>
                ))}
              </select>
            </label>
            <label className="usage-field">
              供应商
              <select
                value={provider}
                onChange={(e) => setProvider(e.target.value)}
              >
                <option value="">全部供应商</option>
                {providers.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <label className="usage-field">
            模型
            <input
              value={model}
              list="usage-calculator-models"
              placeholder="全部模型，可选择已使用模型"
              onChange={(e) => {
                const chosen = e.target.value;
                setModel(chosen);
                // A used model brings its prices along.
                const rates = models.find(
                  (item) => item.model === chosen,
                )?.rates;
                if (rates) setUnitPrices(rates);
              }}
            />
            <datalist id="usage-calculator-models">
              {models.map((item) => (
                <option key={item.model} value={item.model} />
              ))}
            </datalist>
          </label>
          <div className="usage-price-fields">
            <label className="usage-field">
              开始
              <input
                type="datetime-local"
                step="1"
                value={from}
                onChange={(e) =>
                  setFrom(
                    e.target.value.length === 16
                      ? `${e.target.value}:00`
                      : e.target.value,
                  )
                }
              />
            </label>
            <label className="usage-field">
              结束
              <input
                type="datetime-local"
                step="1"
                value={to}
                onChange={(e) =>
                  setTo(
                    e.target.value.length === 16
                      ? `${e.target.value}:59`
                      : e.target.value,
                  )
                }
              />
            </label>
          </div>
          <div className="usage-price-fields usage-calculator-triple usage-calculator-section">
            {quantities.map((label, index) => (
              <label key={label} className="usage-field">
                {label}单价 / 美元每百万
                <input
                  required
                  min="0"
                  step="any"
                  type="number"
                  value={unitPrices[index]}
                  onChange={(e) =>
                    setUnitPrices(
                      unitPrices.map((v, i) =>
                        i === index ? e.target.value : v,
                      ) as FiveTuple,
                    )
                  }
                />
              </label>
            ))}
          </div>
          {error && (
            <div className="form-error" role="alert">
              {error}
            </div>
          )}
          <div className="usage-form-actions">
            <button className="primary" type="submit">
              计算
            </button>
          </div>
        </fieldset>
        {result && (
          <div className="usage-calculator-result" aria-live="polite">
            <div className="usage-calculator-total">
              <span>消耗金额</span>
              <output>${result.total}</output>
            </div>
            <table>
              <thead>
                <tr>
                  <th>项目</th>
                  <th>Tokens</th>
                  <th>美元 / 百万</th>
                  <th>费用 / 美元</th>
                </tr>
              </thead>
              <tbody>
                {quantities.map((label, i) => (
                  <tr key={label}>
                    <th>{label}</th>
                    <td>{numeric(Number(result.tokens[i]))}</td>
                    <td>{result.rates[i]}</td>
                    <td>{result.costs[i]}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </form>
    </Drawer>
  );
}
