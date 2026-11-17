import { useEffect, useRef, useState } from "react";
import { command } from "./bridge";
import { Drawer } from "./UsageDrawer";
import { errorOf, type ClientId } from "./types";
import type {
  CalculatorModel,
  CostCalculation,
  CostCalculatorInput,
  UsageFilter,
} from "./usage-types";

type FiveTuple = [string, string, string, string, string];
type SubRatioKey =
  | "outputRatio"
  | "cacheReadRatio"
  | "cacheWrite5mRatio"
  | "cacheWrite1hRatio";

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
const subRatioKeys: SubRatioKey[] = [
  "outputRatio",
  "cacheReadRatio",
  "cacheWrite5mRatio",
  "cacheWrite1hRatio",
];
type RatioField = [keyof Omit<CostCalculatorInput, "rates" | "tokens">, string];
const baseFields: RatioField[] = [
  ["basePrice", "基准价格 / 美元每百万"],
  ["groupRatio", "分组倍率"],
];
const ratioFields: RatioField[] = [
  ["modelRatio", "模型倍率"],
  ["outputRatio", "输出倍率"],
  ["cacheReadRatio", "缓存读取倍率"],
  ["cacheWrite5mRatio", "创建 5 分钟缓存倍率"],
  ["cacheWrite1hRatio", "创建 1 小时缓存倍率"],
];

const SCALE8 = 100_000_000n;
function pow10(n: number): bigint {
  return 10n ** BigInt(n);
}
function parseDec(raw: string): { num: bigint; scale: number } | null {
  const s = raw.trim();
  if (!/^\d+(?:\.\d+)?$/.test(s)) return null;
  const dot = s.indexOf(".");
  if (dot < 0) return { num: BigInt(s), scale: 0 };
  const frac = s.slice(dot + 1);
  return { num: BigInt(s.slice(0, dot) + frac), scale: frac.length };
}
function formatScaled(scaled: bigint, scale: number): string {
  if (scaled === 0n) return "0";
  const base = pow10(scale);
  const whole = scaled / base;
  const rem = scaled % base;
  if (rem === 0n) return whole.toString();
  const frac = rem.toString().padStart(scale, "0").replace(/0+$/, "");
  return `${whole.toString()}.${frac}`;
}
function mul8(aStr: string, bStr: string): string | null {
  const a = parseDec(aStr),
    b = parseDec(bStr);
  if (!a || !b) return null;
  const prod = a.num * b.num;
  const totalScale = a.scale + b.scale;
  if (totalScale <= 8) {
    return formatScaled(prod * pow10(8 - totalScale), 8);
  }
  const denom = pow10(totalScale - 8);
  const rounded = (2n * prod + denom) / (2n * denom);
  return formatScaled(rounded, 8);
}
function mulExact(aStr: string, bStr: string): string | null {
  const a = parseDec(aStr),
    b = parseDec(bStr);
  if (!a || !b) return null;
  return formatScaled(a.num * b.num, a.scale + b.scale);
}
function div8(aStr: string, bStr: string): string | null {
  const a = parseDec(aStr),
    b = parseDec(bStr);
  if (!a || !b || b.num === 0n) return null;
  const num = a.num * SCALE8 * pow10(b.scale);
  const denom = b.num * pow10(a.scale);
  const rounded = (2n * num + denom) / (2n * denom);
  return formatScaled(rounded, 8);
}
function isPositive(raw: string): boolean {
  const d = parseDec(raw);
  return d !== null && d.num > 0n;
}
function isZero(raw: string): boolean {
  const d = parseDec(raw);
  return d !== null && d.num === 0n;
}

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
  const [model, setModel] = useState(filter.model ?? "");
  const [unitPrices, setUnitPrices] = useState<FiveTuple>([
    "2",
    "2",
    "2",
    "2",
    "2",
  ]);
  const [exactRates, setExactRates] = useState<FiveTuple | null>(null);
  const [values, setValues] = useState({
    basePrice: "2",
    modelRatio: "1",
    outputRatio: "1",
    cacheReadRatio: "1",
    cacheWrite5mRatio: "1",
    cacheWrite1hRatio: "1",
    groupRatio: "1",
  });
  const [tokens, setTokens] = useState<FiveTuple>(["0", "0", "0", "0", "0"]);
  const [result, setResult] = useState<CostCalculation | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const editedRef = useRef(false);
  // A ratio was left invalid, which dropped the exact prices.
  const invalidRatioRef = useRef(false);

  const applyModelRates = (rates: FiveTuple) => {
    setUnitPrices(rates);
    setExactRates(rates);
    setValues((prev) => {
      const base = isPositive(prev.basePrice) ? prev.basePrice : "2";
      const modelRatio = div8(rates[0], base) ?? prev.modelRatio;
      const pInPos = isPositive(rates[0]);
      const pInZero = isZero(rates[0]);
      const sub = (idx: 1 | 2 | 3 | 4, fallback: string) =>
        pInPos ? (div8(rates[idx], rates[0]) ?? fallback) : pInZero ? "0" : fallback;
      return {
        ...prev,
        basePrice: base,
        modelRatio,
        outputRatio: sub(1, prev.outputRatio),
        cacheReadRatio: sub(2, prev.cacheReadRatio),
        cacheWrite5mRatio: sub(3, prev.cacheWrite5mRatio),
        cacheWrite1hRatio: sub(4, prev.cacheWrite1hRatio),
      };
    });
  };

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
      if (!editedRef.current && model) {
        const matched = usedModels.find((item) => item.model === model);
        if (matched?.rates) applyModelRates(matched.rates);
      }
    });
    return () => {
      active = false;
    };
  }, [client]);

  const updateUnitPrice = (index: number, nextPrice: string) => {
    editedRef.current = true;
    const nextPrices = unitPrices.map((v, i) =>
      i === index ? nextPrice : v,
    ) as FiveTuple;
    setUnitPrices(nextPrices);
    setExactRates(nextPrices);
    if (index === 0) {
      if (!parseDec(nextPrice)) return;
      setValues((prev) => {
        const base =
          !isPositive(prev.basePrice) && isPositive(nextPrice)
            ? "2"
            : prev.basePrice;
        const modelRatio = isPositive(base)
          ? (div8(nextPrice, base) ?? prev.modelRatio)
          : "0";
        const pInPos = isPositive(nextPrice);
        const pInZero = isZero(nextPrice);
        const sub = (idx: 1 | 2 | 3 | 4, fallback: string) =>
          pInPos
            ? (div8(nextPrices[idx], nextPrice) ?? fallback)
            : pInZero
              ? "0"
              : fallback;
        return {
          ...prev,
          basePrice: base,
          modelRatio,
          outputRatio: sub(1, prev.outputRatio),
          cacheReadRatio: sub(2, prev.cacheReadRatio),
          cacheWrite5mRatio: sub(3, prev.cacheWrite5mRatio),
          cacheWrite1hRatio: sub(4, prev.cacheWrite1hRatio),
        };
      });
      return;
    }
    if (!parseDec(nextPrice)) return;
    const key = subRatioKeys[index - 1];
    setValues((prev) => ({
      ...prev,
      [key]: isPositive(nextPrices[0])
        ? (div8(nextPrice, nextPrices[0]) ?? prev[key])
        : isZero(nextPrices[0])
          ? "0"
          : prev[key],
    }));
  };

  const updateRatio = (
    key: keyof typeof values,
    nextVal: string,
  ) => {
    editedRef.current = true;
    const nextValues = { ...values, [key]: nextVal };
    setValues(nextValues);
    if (key === "groupRatio") return;
    if (!parseDec(nextVal)) {
      invalidRatioRef.current = true;
      setExactRates(null);
      return;
    }
    // Once every ratio is valid again the prices on screen are the exact
    // ones, so the result matches what is shown.
    const restore = (prices: FiveTuple) => {
      const valid =
        prices.every((v) => parseDec(v)) &&
        Object.values(nextValues).every((v) => parseDec(v));
      if (invalidRatioRef.current && valid) {
        invalidRatioRef.current = false;
        setExactRates(prices);
        return true;
      }
      return false;
    };
    if (key === "basePrice" || key === "modelRatio") {
      setExactRates(null);
      const pIn = mul8(nextValues.basePrice, nextValues.modelRatio);
      if (pIn === null) return;
      const pInExact =
        mulExact(nextValues.basePrice, nextValues.modelRatio) ?? pIn;
      const nextPrices: FiveTuple = [
        pIn,
        mul8(pInExact, nextValues.outputRatio) ?? unitPrices[1],
        mul8(pInExact, nextValues.cacheReadRatio) ?? unitPrices[2],
        mul8(pInExact, nextValues.cacheWrite5mRatio) ?? unitPrices[3],
        mul8(pInExact, nextValues.cacheWrite1hRatio) ?? unitPrices[4],
      ];
      setUnitPrices(nextPrices);
      restore(nextPrices);
      return;
    }
    const subIdx = subRatioKeys.indexOf(key as SubRatioKey);
    if (subIdx >= 0) {
      const priceIdx = subIdx + 1;
      const computed = mul8(unitPrices[0], nextVal);
      const nextPrices =
        computed === null
          ? unitPrices
          : (unitPrices.map((v, i) =>
              i === priceIdx ? computed : v,
            ) as FiveTuple);
      if (computed !== null) setUnitPrices(nextPrices);
      if (restore(nextPrices)) return;
      if (exactRates) {
        const exact = mulExact(exactRates[0], nextVal);
        setExactRates(
          exact !== null
            ? (exactRates.map((v, i) =>
                i === priceIdx ? exact : v,
              ) as FiveTuple)
            : null,
        );
      }
    }
  };

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
      setTokens([
        String(t.input ?? 0),
        String(t.output ?? 0),
        String(t.cacheRead ?? 0),
        String(cw5m),
        String(cw1h),
      ]);
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
          setBusy(true);
          setError("");
          setResult(null);
          void command("calculate_usage_cost", {
            input: {
              ...values,
              ...(exactRates ? { rates: exactRates } : {}),
              tokens,
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
          <div className="usage-calculator-source">
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
                  const found = models.find((item) => item.model === chosen);
                  if (found) editedRef.current = true;
                  if (found?.rates) applyModelRates(found.rates);
                }}
              />
              <datalist id="usage-calculator-models">
                {models.map((item) => (
                  <option key={item.model} value={item.model} />
                ))}
              </datalist>
            </label>
            <button
              type="button"
              disabled={!provider}
              onClick={() => void loadUsage()}
            >
              填入当前范围用量
            </button>
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
                  onChange={(e) => updateUnitPrice(index, e.target.value)}
                />
              </label>
            ))}
          </div>
          {[baseFields, ratioFields].map((fields, row) => (
            <div
              key={row}
              className={
                row === 0
                  ? "usage-price-fields usage-calculator-ratios"
                  : "usage-price-fields usage-calculator-triple"
              }
            >
              {fields.map(([key, label]) => (
                <label key={key} className="usage-field">
                  {label}
                  <input
                    required
                    min="0"
                    step="any"
                    type="number"
                    value={values[key]}
                    onChange={(e) => updateRatio(key, e.target.value)}
                  />
                </label>
              ))}
            </div>
          ))}
          <div className="usage-price-fields usage-calculator-triple usage-calculator-section">
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
                      tokens.map((v, i) =>
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
