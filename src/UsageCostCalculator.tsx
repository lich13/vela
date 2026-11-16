import { useEffect, useState } from "react";
import { command } from "./bridge";
import { Drawer } from "./UsageDrawer";
import { errorOf, type ClientId } from "./types";
import type {
  CostCalculation,
  CostCalculatorInput,
  UsageFilter,
} from "./usage-types";

const clients: [ClientId, string][] = [
  ["claude", "Claude Code"],
  ["codex", "Codex"],
  ["grok", "Grok Build"],
  ["dsh", "DeepSeek Harness"],
];
const quantities = ["输入", "输出", "缓存读取", "缓存创建"];
const ratios: [Exclude<keyof CostCalculatorInput, "tokens">, string][] = [
  ["basePrice", "基准价格 / 美元每百万"],
  ["modelRatio", "模型倍率"],
  ["outputRatio", "输出倍率"],
  ["groupRatio", "分组倍率"],
  ["cacheReadRatio", "缓存读取倍率"],
  ["cacheWriteRatio", "缓存创建倍率"],
];

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
  const [model, setModel] = useState(filter.model ?? "");
  const [values, setValues] = useState({
    basePrice: "2",
    modelRatio: "1",
    outputRatio: "1",
    cacheReadRatio: "1",
    cacheWriteRatio: "1",
    groupRatio: "1",
  });
  const [tokens, setTokens] = useState(["0", "0", "0", "0"]);
  const [result, setResult] = useState<CostCalculation | null>(null);
  const [error, setError] = useState("");
  const [providerError, setProviderError] = useState("");
  const [busy, setBusy] = useState(false);
  const [reload, setReload] = useState(0);
  useEffect(() => {
    let active = true;
    setProviders([]);
    setProviderError("");
    void command("get_gateway", { clientId: client })
      .then((view) => {
        if (active)
          setProviders(view.providers.map(({ id, name }) => ({ id, name })));
      })
      .catch((e) => {
        if (active) setProviderError(errorOf(e).message);
      });
    return () => {
      active = false;
    };
  }, [client, reload]);
  const loadUsage = async () => {
    setBusy(true);
    setError("");
    setResult(null);
    try {
      const totals = await command("get_usage_calculator_totals", {
        filter: {
          ...filter,
          client,
          provider,
          model: model.trim() || undefined,
          source: "proxy",
        },
      });
      if (!totals.attempts) throw new Error("当前范围没有该供应商的网关用量");
      const t = totals.tokens;
      setTokens(
        [t.input, t.output, t.cacheRead, t.cacheWrite].map((value) =>
          value == null ? "" : String(value),
        ),
      );
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
        onSubmit={(event) => {
          event.preventDefault();
          const counts = tokens.map(Number);
          if (counts.some((n) => !Number.isSafeInteger(n) || n < 0)) {
            setError("Token 数须为有效的非负整数");
            return;
          }
          setBusy(true);
          setError("");
          setResult(null);
          void command("calculate_usage_cost", {
            input: {
              ...values,
              tokens: counts as CostCalculatorInput["tokens"],
            },
          })
            .then(setResult)
            .catch((e) => setError(errorOf(e).message))
            .finally(() => setBusy(false));
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
                <option value="">手动输入用量</option>
                {providers.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
            </label>
          </div>
          {providerError && (
            <div className="form-error" role="alert">
              {providerError}
              <button type="button" onClick={() => setReload((n) => n + 1)}>
                重新加载供应商
              </button>
            </div>
          )}
          <div className="usage-calculator-source">
            <label className="usage-field">
              模型
              <input
                value={model}
                placeholder="全部模型"
                onChange={(e) => setModel(e.target.value)}
              />
            </label>
            <button
              type="button"
              disabled={!provider}
              onClick={() => void loadUsage()}
            >
              填入当前范围用量
            </button>
          </div>
          <div className="usage-price-fields usage-calculator-ratios">
            {ratios.map(([key, label]) => (
              <label key={key} className="usage-field">
                {label}
                <input
                  required
                  min="0"
                  step="any"
                  type="number"
                  value={values[key]}
                  onChange={(e) =>
                    setValues({ ...values, [key]: e.target.value })
                  }
                />
              </label>
            ))}
          </div>
          <div className="usage-price-fields">
            {quantities.map((label, index) => (
              <label key={label} className="usage-field">
                {label} Tokens
                <input
                  required
                  min="0"
                  step="1"
                  type="number"
                  value={tokens[index]}
                  onChange={(e) =>
                    setTokens(
                      tokens.map((v, i) => (i === index ? e.target.value : v)),
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
              <span>换算费用</span>
              <output>${result.total}</output>
            </div>
            <table>
              <thead>
                <tr>
                  <th>项目</th>
                  <th>美元 / 百万</th>
                  <th>费用 / 美元</th>
                </tr>
              </thead>
              <tbody>
                {quantities.map((label, i) => (
                  <tr key={label}>
                    <th>{label}</th>
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
