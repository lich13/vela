import UsageBill from "./UsageBill";
import { currentInstance, useInstance } from "./instance";
import Detail from "./UsageDetail";
import Sources from "./UsageSources";
import UsageLogTable, { UsageLogControls } from "./UsageLogTable";
import { subscriptions } from "./subscriptions";
import { useVisible } from "./visibility";
import type {
  AntigravityAuthState,
  CommandName,
  CommandParams,
  CommandResult,
} from "./bridge-contracts";
import {
  useCallback,
  useEffect,
  useEffectEvent,
  useRef,
  useState,
} from "react";
import {
  Download,
  ArrowDown,
  ArrowUp,
  ChevronLeft,
  ChevronRight,
  Database,
  RefreshCw,
  ChevronDown,
  CalendarDays,
} from "lucide-react";
import { command } from "./bridge";
import { errorOf, type EntryKey } from "./types";
import { confirmAction } from "./confirmation";
import Pricing from "./UsagePricing";
import UsageAntigravity from "./UsageAntigravity";
import UsageClaudeQuota from "./UsageClaudeQuota";
import UsageCurvesPanel from "./UsageCurves";
import {
  type AntigravityUsageStats,
  type ClaudeUsageQuota,
  type UsageCurves,
  averageFirstToken,
  clientName,
  deletedKeyName,
  firstTokenLabel,
  firstTokenTitle,
  ownEntryKey,
  type Totals,
  compact,
  money,
  numeric,
  tokenTotal,
  type Dashboard,
  type Group,
  type UsageFilter,
  type UsagePage,
  type UsageRecord,
  type UsageState,
} from "./usage-types";
import "./usage.css";
export function syncStatus(state: UsageState | null, client: string) {
  if (!state) return "";
  if (state.maintenance) return state.maintenance;
  const reports = client
    ? [state.reports[client]].filter(Boolean)
    : Object.values(state.reports);
  if (
    reports.some((r) =>
      ["syncing", "scanning", "rebuilding"].includes(r.phase ?? ""),
    ) ||
    (!client && state.syncing)
  )
    return "同步中…";
  if (!reports.length) return "";
  if (reports.every((r) => r.phase === "failed")) return "同步失败";
  if (
    reports.some(
      (r) => r.phase === "failed" || r.phase === "partial" || r.errors > 0,
    )
  )
    return "部分未同步";
  return reports.every((r) => r.completedAt != null) ? "已同步" : "";
}
/** Why the shown sources are not fully synced, for the status tooltip. */
function syncReason(state: UsageState | null, client: string) {
  if (!state) return undefined;
  const reports = client
    ? [state.reports[client]].filter(Boolean)
    : Object.values(state.reports);
  return reports.find((r) => r.lastError)?.lastError ?? undefined;
}
const local = (n: number) => {
  const d = new Date(n);
  return new Date(n - d.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
};
const ranges = [
  ["today", "今日"],
  ["1", "最近 24 小时"],
  ["7", "7 天"],
  ["14", "14 天"],
  ["30", "30 天"],
  ["all", "全部"],
  ["custom", "自定义"],
];
type RankingView = {
  page: number;
  sort: "requests" | "cost" | "tokens" | "firstToken";
  asc: boolean;
};
const initialRanking: RankingView = { page: 1, sort: "requests", asc: false };
const empty: UsagePage = { rows: [], page: 1, total: 0, detailSince: 0 };
/** The gateways that hand keys out, in the order the page lists their keys. */
const keyGateways = ["codex", "claude", "grok", "dsh", "agy"] as const;
type QuotaAnswer = CommandResult<"refresh_antigravity_usage_quota">;
/** The instances whose Antigravity quota is being asked for, each once at a time. */
const quotaAsked = new Map<string, Promise<QuotaAnswer>>();
/** Asks the instance shown for its quota, unless it is being asked already. */
function askQuota() {
  const instance = currentInstance();
  let asked = quotaAsked.get(instance);
  if (!asked) {
    asked = command("refresh_antigravity_usage_quota")
      .catch(
        (e): QuotaAnswer => ({ changed: false, error: errorOf(e).message }),
      )
      .finally(() => quotaAsked.delete(instance));
    quotaAsked.set(instance, asked);
  }
  return asked;
}
export default function Usage({
  onDirtyChange,
}: {
  onDirtyChange?: (dirty: boolean) => void;
}) {
  const { remote, supports } = useInstance();
  // A server from before the quota had a command of its own still asks for it
  // while it reads the statistics, and reports the failure with them.
  const quotaApart = supports("refresh_antigravity_usage_quota");
  const [billOpen, setBillOpen] = useState(false);
  const [billResult, setBillResult] = useState("");
  const [range, setRange] = useState("today"),
    [from, setFrom] = useState(local(Date.now() - 86400000)),
    [to, setTo] = useState(local(Date.now())),
    [live, setLive] = useState(true);
  const [client, setClient] = useState(""),
    [provider, setProvider] = useState(""),
    [operation, setOperation] = useState(""),
    [entryKey, setEntryKey] = useState(""),
    [status, setStatus] = useState(""),
    [page, setPage] = useState(1);
  const [tab, setTab] = useState("requests");
  const independent = tab === "antigravity" || tab === "claude";
  // A server from before the curves has no command to read them with.
  const charted = supports("get_usage_curves");
  const curved = tab === "curves";
  // The totals above the tabs are of the requests the filters narrow, which
  // neither a client's own page nor the curves are.
  const totalled = !independent && !curved;
  // The curves on the page, and the time and client they were read for.
  const [curves, setCurves] = useState<{
      scope: string;
      data: UsageCurves;
    } | null>(null),
    [curvesBusy, setCurvesBusy] = useState(false),
    // The time and client the last reading of them failed for.
    [curvesLost, setCurvesLost] = useState<string | null>(null);
  const [claudeQuota, setClaudeQuota] = useState<ClaudeUsageQuota | null>(null);
  const [agStats, setAgStats] = useState<AntigravityUsageStats | null>(null),
    [agBusy, setAgBusy] = useState(false),
    [agQuotaError, setAgQuotaError] = useState<string | null>(null),
    [agCurrentAccount, setAgCurrentAccount] = useState<string | null>(null);
  const [data, setData] = useState<Dashboard | null>(null),
    [logs, setLogs] = useState<UsagePage>(empty),
    [state, setState] = useState<UsageState | null>(null);
  const [providers, setProviders] = useState<Record<string, string>>({}),
    [claudeProviders, setClaudeProviders] = useState<Record<string, string>>(
      {},
    ),
    [agProviders, setAgProviders] = useState<Record<string, string>>({}),
    // The keys each gateway hands out, by its client id.
    [handedOut, setHandedOut] = useState<Record<string, EntryKey[]>>({}),
    [selected, setSelected] = useState<UsageRecord | null>(null),
    [sheet, setSheet] = useState(false),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(false),
    [timeOpen, setTimeOpen] = useState(false),
    [rowsBusy, setRowsBusy] = useState(false);
  const [rankings, setRankings] = useState<Record<string, RankingView>>({
    providers: initialRanking,
    models: initialRanking,
    keys: initialRanking,
  });
  const timeButton = useRef<HTMLButtonElement>(null);
  const resetPages = () => {
    setPage(1);
    setRankings((v) => ({
      providers: { ...v.providers, page: 1 },
      models: { ...v.models, page: 1 },
      keys: { ...v.keys, page: 1 },
    }));
  };
  const pricingDirty = useRef(false);
  useEffect(() => {
    onDirtyChange?.(sheet || billOpen || pricingDirty.current);
    return () => onDirtyChange?.(false);
  }, [sheet, billOpen, onDirtyChange]);
  const priceDirty = useCallback(
    (dirty: boolean) => {
      pricingDirty.current = dirty;
      onDirtyChange?.(dirty || sheet || billOpen);
    },
    [onDirtyChange, sheet, billOpen],
  );
  const selectTab = async (next: string) => {
    if (
      next !== tab &&
      pricingDirty.current &&
      !(await confirmAction("离开定价会丢弃未保存的修改。"))
    )
      return;
    if (next === "antigravity" || next === "claude") {
      if (entryKey || next !== client) resetPages();
      // Those two pages do not count by key.
      setEntryKey("");
      if (next !== client) {
        setClient(next);
        setProvider("");
      }
    }
    if (next !== tab) {
      setAgStats(null);
      setAgBusy(false);
      setAgQuotaError(null);
    }
    setTab(next);
  };
  const visible = useVisible("app-visibility", (message) => setError(message));
  const sequence = useRef(0),
    rowsSequence = useRef(0),
    agSequence = useRef(0),
    quotaSequence = useRef(0),
    quotaHeard = useRef(0),
    stateSequence = useRef(0),
    curveSequence = useRef(0),
    // The time and client of the curves being read, null while none are.
    curveFlight = useRef<string | null>(null),
    detailSequence = useRef(0);
  useEffect(
    () => () => {
      detailSequence.current++;
    },
    [],
  );
  const pending = useRef(new Map<string, Promise<unknown>>());
  const request = useCallback(
    <K extends CommandName>(
      name: K,
      ...params: CommandParams<K>
    ): Promise<CommandResult<K>> => {
      const args = params[0] ?? {};
      const key = name + JSON.stringify(args);
      let promise = pending.current.get(key);
      if (!promise) {
        promise = command(name, ...params).finally(() => {
          if (pending.current.get(key) === promise) pending.current.delete(key);
        });
        pending.current.set(key, promise);
      }
      return promise as Promise<CommandResult<K>>;
    },
    [],
  );
  useEffect(() => {
    if (!visible) pending.current.clear();
  }, [visible]);
  const span = useCallback(() => {
    const end = live
      ? Math.floor(Date.now() / 1000) * 1000
      : new Date(to).getTime();
    let start: number | undefined;
    if (range === "today") {
      const d = new Date(end);
      d.setHours(0, 0, 0, 0);
      start = d.getTime();
    } else if (range === "custom") start = new Date(from).getTime();
    else if (range !== "all") start = end - Number(range) * 86400000;
    return { start, end };
  }, [range, from, to, live]);
  const query = useCallback(
    (): UsageFilter => ({
      ...span(),
      client: client || undefined,
      provider: provider || undefined,
      operation: operation || undefined,
      entryKey: entryKey || undefined,
    }),
    [span, client, provider, operation, entryKey],
  );
  const loadOverview = useCallback(async () => {
    const id = ++sequence.current;
    setBusy(true);
    try {
      const filter = query();
      const overview = await request("get_usage_dashboard", {
        filter,
      });
      if (id === sequence.current) {
        setData(overview);
        setError("");
      }
    } catch (e) {
      if (id === sequence.current) setError(errorOf(e).message);
    } finally {
      if (id === sequence.current) setBusy(false);
    }
  }, [query, request]);
  // The journal knows a failure's client, provider and time and nothing else
  // of the request, so the curves follow the time and the client alone. Every
  // provider's come at once: choosing one among them reads nothing again.
  const curveScope = [range, from, to, live, client].join("|");
  const loadCurves = useCallback(
    async (fresh = false) => {
      // A range that ends now ends a little later each time it is asked for,
      // so no two readings of it are one request. A reading that is only the
      // next in turn leaves the one under way to answer: taking its place
      // each time would leave a reading slower than the turns never answered.
      if (!fresh && curveFlight.current === curveScope) return;
      const id = ++curveSequence.current;
      curveFlight.current = curveScope;
      setCurvesBusy(true);
      try {
        const data = await request("get_usage_curves", {
          filter: { ...span(), client: client || undefined },
        });
        if (id === curveSequence.current) {
          setCurves({ scope: curveScope, data });
          setCurvesLost(null);
          setError("");
        }
      } catch (e) {
        if (id === curveSequence.current) {
          setCurvesLost(curveScope);
          setError(errorOf(e).message);
        }
      } finally {
        if (id === curveSequence.current) {
          curveFlight.current = null;
          setCurvesBusy(false);
        }
      }
    },
    [span, client, request, curveScope],
  );
  const loadRows = useCallback(async () => {
    const id = ++rowsSequence.current;
    setRowsBusy(true);
    try {
      const rows = await request("get_usage_logs", {
        filter: { ...query(), status: status || undefined, page },
      });
      if (id === rowsSequence.current) setLogs(rows);
    } catch (e) {
      if (id === rowsSequence.current) setError(errorOf(e).message);
    } finally {
      if (id === rowsSequence.current) setRowsBusy(false);
    }
  }, [query, status, page, request]);
  /**
   * Reads the history and the saved periods. False where that failed; a
   * reading that a later one overtook did not. With `changed`, the periods
   * were saved anew since, and a reading under way may have set out before.
   */
  const readClientStats = useCallback(
    async (changed = false): Promise<boolean> => {
      const id = ++agSequence.current;
      setAgBusy(true);
      try {
        const name =
          tab === "claude"
            ? "get_claude_usage_stats"
            : "get_antigravity_usage_stats";
        const params = { filter: query() };
        if (changed) pending.current.delete(name + JSON.stringify(params));
        const stats = await request(name, params);
        if (id === agSequence.current) {
          setAgStats(stats);
          setError("");
        }
        return true;
      } catch (e) {
        if (id === agSequence.current) setError(errorOf(e).message);
        return false;
      } finally {
        if (id === agSequence.current) setAgBusy(false);
      }
    },
    [query, request, tab],
  );
  const loadClientStats = useCallback(async () => {
    const apart = tab === "antigravity" && quotaApart;
    // A question under way now may be answered before the statistics are
    // read, so its answer is the one waited for.
    const under = apart ? quotaAsked.get(currentInstance()) : undefined;
    const round = ++quotaSequence.current;
    if (!(await readClientStats()) || !apart) return;
    if (round !== quotaSequence.current) return;
    // The history is on the page. The quota takes its time to be answered,
    // so it is asked after.
    const heard = quotaHeard.current;
    const answer = await (under ?? askQuota());
    if (round !== quotaSequence.current) return;
    setAgQuotaError(answer.error);
    // An instance that tells of the periods it saves has had them read
    // again by now; one from before that tells of them only here.
    if (answer.changed && heard === quotaHeard.current)
      await readClientStats(true);
  }, [readClientStats, tab, quotaApart]);
  const loadClaudeQuota = useCallback(
    async (refresh = false) => {
      try {
        setClaudeQuota(await request("get_claude_usage_quota", { refresh }));
      } catch (error) {
        setClaudeQuota({
          account: null,
          plan: null,
          windows: [],
          error: errorOf(error).message,
        });
      }
    },
    [request],
  );
  const loadState = useCallback(
    async (fresh = false) => {
      if (fresh) pending.current.delete("get_usage_state{}");
      const id = ++stateSequence.current;
      try {
        const value = await request("get_usage_state");
        if (id === stateSequence.current) setState(value);
      } catch (e) {
        if (id === stateSequence.current) setError(errorOf(e).message);
      }
    },
    [request],
  );
  const loadData = useCallback(
    (fresh = false, refreshQuota = false) => {
      if (fresh) pending.current.clear();
      return Promise.all([
        ...(totalled ? [loadOverview()] : []),
        ...(tab === "requests" ? [loadRows()] : []),
        ...(curved ? [loadCurves(fresh)] : []),
        ...(independent ? [loadClientStats()] : []),
        ...(tab === "claude" ? [loadClaudeQuota(refreshQuota)] : []),
      ]);
    },
    [
      loadOverview,
      loadRows,
      loadCurves,
      loadClientStats,
      loadClaudeQuota,
      independent,
      totalled,
      curved,
      tab,
    ],
  );
  const load = useCallback(
    (fresh = false) => Promise.all([loadData(fresh, fresh), loadState()]),
    [loadData, loadState],
  );
  useEffect(() => {
    if (visible && totalled) void loadOverview();
    return () => {
      sequence.current++;
    };
  }, [loadOverview, visible, totalled]);
  useEffect(() => {
    if (visible && curved) void loadCurves();
    return () => {
      curveSequence.current++;
      curveFlight.current = null;
      setCurvesBusy(false);
    };
  }, [loadCurves, visible, curved]);
  useEffect(() => {
    if (curved && !charted) setTab("requests");
  }, [curved, charted]);
  useEffect(() => {
    if (visible && tab === "requests") void loadRows();
    return () => {
      rowsSequence.current++;
    };
  }, [loadRows, visible, tab]);
  useEffect(() => {
    if (!visible || !independent) return;
    void loadClientStats();
    return () => {
      agSequence.current++;
      quotaSequence.current++;
    };
  }, [loadClientStats, visible, independent]);
  useEffect(() => {
    if (!visible || tab !== "antigravity") return;
    const events = subscriptions((e) => setError(errorOf(e).message));
    // Whoever asked, the periods on the page are no longer the saved ones.
    // They are read again and nothing is asked: the question this page has
    // out is still answered to it.
    events.watch("antigravity-quota", () => {
      quotaHeard.current++;
      void readClientStats(true);
    });
    return () => events.dispose();
  }, [visible, tab, readClientStats]);
  useEffect(() => {
    if (!visible || tab !== "claude") return;
    void loadClaudeQuota();
    const events = subscriptions((e) => setError(errorOf(e).message));
    events.watch("claude-login-state", (value) => {
      if (value.phase === "complete") void loadClaudeQuota(true);
    });
    return () => events.dispose();
  }, [visible, tab, loadClaudeQuota]);
  const reloadAccountStats = useEffectEvent(() => {
    pending.current.clear();
    // What went wrong was about the account signed in until now.
    setAgQuotaError(null);
    if (tab === "antigravity") void loadClientStats();
  });
  useEffect(() => {
    if (!visible) return;
    void loadState();
    const events = subscriptions((e) => setError(errorOf(e).message));
    events.watch("usage-state", () => void loadState(true));
    return () => {
      stateSequence.current++;
      events.dispose();
    };
  }, [loadState, visible]);
  useEffect(() => {
    if (!visible) return;
    const events = subscriptions((e) => setError(errorOf(e).message));
    events.watch("pricing-state", () => void loadData(true));
    return () => events.dispose();
  }, [loadData, visible]);
  // A client's keys are as its gateway last said them, in an event of its own
  // or in the answer to a reading: an answer that set out before what is on
  // the page does not replace it.
  const keyClock = useRef(0),
    keysAsOf = useRef<Record<string, number>>({});
  const acceptKeys = useCallback(
    (at: number, clientId: string, entryKeys: EntryKey[] = []) => {
      if ((keysAsOf.current[clientId] ?? 0) > at) return;
      keysAsOf.current[clientId] = at;
      setHandedOut((previous) => ({ ...previous, [clientId]: entryKeys }));
    },
    [],
  );
  /** Reads the keys again: another window may have changed them since. */
  const loadKeys = useCallback(() => {
    const began = ++keyClock.current;
    for (const clientId of keyGateways)
      void command("get_gateway", { clientId })
        .then((gateway) => acceptKeys(began, clientId, gateway.entryKeys))
        // The names on the page stay as they are.
        .catch(() => {});
  }, [acceptKeys]);
  useEffect(() => {
    if (!visible) return;
    let gone = false;
    const began = ++keyClock.current;
    const seenAgKeyRef = { current: null as string | null };
    const agSignature = (ag: AntigravityAuthState | null) =>
      ag
        ? `${ag.revision ?? ""}:${(ag.accounts ?? []).map((a) => `${a.id}:${Boolean(a.current)}`).join(",")}`
        : "none";
    const updateAccounts = (ag: AntigravityAuthState | null) => {
      setAgCurrentAccount(ag?.accounts.find((a) => a.current)?.id ?? null);
      const names = Object.fromEntries(
        (ag?.accounts ?? []).map((a) => [a.id, a.name]),
      );
      setAgProviders(names);
      return names;
    };
    void Promise.all([
      command("get_gateway", { clientId: "codex" }),
      command("get_gateway", { clientId: "claude" }),
      command("get_gateway", { clientId: "grok" }),
      command("get_gateway", { clientId: "dsh" }),
      command("get_gateway", { clientId: "agy" }),
      command("get_antigravity_auth_state").catch(() => null),
    ])
      .then(([codex, claude, grok, dsh, agy, ag]) => {
        if (gone) return;
        const gw = Object.fromEntries(
          [codex, claude, grok, dsh, agy].flatMap((s) =>
            s.providers.map((p) => [p.id, p.name]),
          ),
        );
        setClaudeProviders(
          Object.fromEntries(claude.providers.map((p) => [p.id, p.name])),
        );
        for (const [clientId, gateway] of Object.entries({
          codex,
          claude,
          grok,
          dsh,
          agy,
        }))
          acceptKeys(began, clientId, gateway.entryKeys);
        if (seenAgKeyRef.current === null) {
          seenAgKeyRef.current = agSignature(ag);
          const agMap = updateAccounts(ag);
          setProviders({ ...gw, ...agMap });
        } else {
          setProviders((prev) => ({ ...gw, ...prev }));
        }
      })
      .catch(() => {});
    const events = subscriptions((e) => setError(errorOf(e).message));
    events.watch("antigravity-auth-state", (ag) => {
      if (gone) return;
      const sig = agSignature(ag);
      const changed =
        seenAgKeyRef.current !== null && seenAgKeyRef.current !== sig;
      seenAgKeyRef.current = sig;
      const agMap = updateAccounts(ag);
      setProviders((prev) => ({ ...prev, ...agMap }));
      if (changed) reloadAccountStats();
    });
    events.watch("gateway-state", (gateway) =>
      acceptKeys(++keyClock.current, gateway.clientId, gateway.entryKeys),
    );
    return () => {
      gone = true;
      sequence.current++;
      events.dispose();
    };
  }, [visible, acceptKeys]);
  useEffect(() => {
    if (!visible || !state?.settings.refreshSeconds) return;
    const t = setInterval(
      () => void load(),
      state.settings.refreshSeconds * 1000,
    );
    return () => clearInterval(t);
  }, [visible, state?.settings.refreshSeconds, load]);
  const choose = <T extends string>(setter: (s: T) => void, value: T) => {
    setter(value);
    resetPages();
  };
  const name = (id: string | null) =>
    id ? providers[id] || `供应商 ${id.slice(0, 8)}` : "本机会话";
  // The providers the page can be narrowed to by name.
  const providerOptions =
    client === "antigravity" || tab === "antigravity"
      ? agProviders
      : client === "claude"
        ? claudeProviders
        : providers;
  // Curves read for another time or client stay up, dimmed, until the ones
  // asked for are read. Once reading those has failed they are not what the
  // page is about: it says so in their place until a reading succeeds, and
  // does not go back and forth as the refreshes come round.
  const curvesRead = curves?.scope === curveScope;
  const curvesGone = !curvesRead && curvesLost === curveScope;
  // Every key handed out, under the name usage knows its client by: the agy
  // gateway is antigravity there.
  const keys = keyGateways.flatMap((clientId) =>
    (handedOut[clientId] ?? []).map((key) => ({
      ...key,
      client: clientId === "agy" ? "antigravity" : clientId,
    })),
  );
  // Keys only the history still knows: deleted since, they have no name left.
  const formerKeys = (data?.entryKeys ?? [])
    .map((group) => group.id)
    .filter((id) => id !== ownEntryKey && !keys.some((key) => key.id === id));
  // The page says nothing of keys until the instance hands one out or has
  // counted one.
  const keyed = keys.length > 0 || formerKeys.length > 0;
  const keyOptions = [
    ...keys.filter((key) => !client || key.client === client).map((k) => k.id),
    ...formerKeys,
  ];
  const keyName = (id: string) =>
    keys.find((key) => key.id === id)?.name ?? deletedKeyName(id);
  // Two clients may each hand out a key of the same name, so with every client
  // shown a key says whose it is.
  const keyLabel = (id: string) => {
    if (id === ownEntryKey) return "本机 Key";
    const key = keys.find((key) => key.id === id);
    return key && !client
      ? `${key.name}（${clientName(key.client)}）`
      : keyName(id);
  };
  // With every client shown, the calculator opens on the one whose key the
  // page is narrowed to.
  const keyOwner = keys.find((key) => key.id === entryKey)?.client;
  const keyLost =
    entryKey !== "" &&
    !(keyed && (entryKey === ownEntryKey || keyOptions.includes(entryKey)));
  useEffect(() => {
    if (!keyLost) return;
    setEntryKey("");
    resetPages();
  }, [keyLost]);
  useEffect(() => {
    if (!keyed && tab === "keys") setTab("requests");
  }, [keyed, tab]);
  const t = data?.totals;
  const cacheRate = t?.cacheInputEligible
    ? (100 * (t.cacheReadEligible ?? 0)) / t.cacheInputEligible
    : null;
  const open = async (r: UsageRecord) => {
    const id = ++detailSequence.current;
    try {
      const detail = await command("get_usage_detail", {
        id: r.id,
      });
      if (id === detailSequence.current) setSelected(detail);
    } catch (e) {
      if (id === detailSequence.current) setError(errorOf(e).message);
    }
  };
  return (
    <section className="usage-page">
      <header className="usage-heading">
        <h1>用量</h1>
        <div className="usage-actions">
          <span
            className="usage-sync-status"
            aria-live="polite"
            title={syncReason(state, client)}
          >
            {syncStatus(state, client)}
          </span>
          <select
            aria-label="用量刷新频率"
            value={state?.settings.refreshSeconds ?? 30}
            onChange={(e) => {
              if (state)
                void command("set_usage_settings", {
                  settings: {
                    ...state.settings,
                    refreshSeconds: Number(e.target.value),
                  },
                })
                  .then(setState)
                  .catch((e) => setError(errorOf(e).message));
            }}
          >
            {[0, 5, 10, 30, 60].map((n) => (
              <option key={n} value={n}>
                {n ? `${n} 秒刷新` : "手动刷新"}
              </option>
            ))}
          </select>
          <button
            disabled={busy || state?.syncing || remote}
            title={remote ? "账单导出到本机文件，仅本机实例可用" : undefined}
            onClick={() => {
              setBillResult("");
              setBillOpen(true);
            }}
          >
            <Download size={16} />
            导出账单
          </button>
          <button
            className="icon-button"
            title="数据来源"
            aria-label="数据来源"
            onClick={(e) => {
              e.currentTarget.focus();
              setSheet(true);
              if (!totalled) void loadOverview();
            }}
          >
            <Database size={17} />
          </button>
          <button
            className="icon-button"
            aria-label="刷新用量"
            disabled={independent ? agBusy : curved ? curvesBusy : busy}
            onClick={() => {
              void load(true);
              loadKeys();
            }}
          >
            <RefreshCw size={17} />
          </button>
        </div>
      </header>
      {billResult && (
        <p role="status" className="usage-bill-result">
          {billResult}
        </p>
      )}
      {totalled && (
        <div className="usage-metrics">
          <Metric label="估算费用" value={money(t?.cost)} title={t?.cost} />
          <Metric
            label="已报告 Token"
            value={t ? compact(tokenTotal(t.tokens)) : "—"}
            title={t ? numeric(tokenTotal(t.tokens)) : undefined}
          />
          <Metric
            label="缓存命中率"
            value={cacheRate == null ? "—" : `${cacheRate.toFixed(1)}%`}
            title={
              cacheRate == null
                ? "无有效缓存样本"
                : `${numeric(t?.cacheReadEligible)} / ${numeric(t?.cacheInputEligible)}`
            }
          />
          <Metric
            label="平均首字"
            value={firstTokenLabel(averageFirstToken(t))}
            title={firstTokenTitle(t)}
          />
        </div>
      )}
      <div className={keyed ? "usage-filters with-key" : "usage-filters"}>
        <select
          aria-label="用量客户端"
          value={client}
          disabled={independent}
          onChange={(e) => {
            const next = e.target.value;
            choose(setClient, next);
            setProvider("");
            setEntryKey("");
          }}
        >
          <option value="">全部客户端</option>
          <option value="codex">Codex</option>
          <option value="claude">Claude Code</option>
          <option value="antigravity">Antigravity</option>
          <option value="grok">Grok Build</option>
          <option value="dsh">DeepSeek Harness</option>
        </select>
        <select
          aria-label="用量供应商"
          value={provider}
          onChange={(e) => choose(setProvider, e.target.value)}
        >
          <option value="">全部供应商</option>
          {Object.entries(providerOptions).map(([id, n]) => (
            <option key={id} value={id}>
              {n}
            </option>
          ))}
          {/* The curves list every provider the records know: one deleted
              since, and under Antigravity the agy gateway's, where the
              choices are its accounts. Chosen there, it is a choice here. */}
          {provider &&
            provider !== "session" &&
            !Object.hasOwn(providerOptions, provider) && (
              <option value={provider}>{name(provider)}</option>
            )}
          {(client === "antigravity" ||
            tab === "antigravity" ||
            client === "claude" ||
            provider === "session") && (
            <option value="session">本机会话（未归属）</option>
          )}
        </select>
        <select
          aria-label="请求类型"
          value={operation}
          // The curves know nothing of a request but its client and provider.
          disabled={curved}
          onChange={(e) => choose(setOperation, e.target.value)}
        >
          <option value="">全部类型</option>
          <option value="model">生成</option>
          <option value="compaction">压缩</option>
          <option value="web_search">搜索</option>
        </select>
        {keyed && (
          <select
            aria-label="用量 Key"
            value={entryKey}
            disabled={independent || curved}
            onChange={(e) => choose(setEntryKey, e.target.value)}
          >
            <option value="">全部 Key</option>
            <option value={ownEntryKey}>本机 Key</option>
            {keyOptions.map((id) => (
              <option key={id} value={id}>
                {keyLabel(id)}
              </option>
            ))}
          </select>
        )}
        <button
          ref={timeButton}
          className="usage-time-button"
          aria-label="用量时间范围"
          aria-haspopup="dialog"
          aria-expanded={timeOpen}
          onClick={() => setTimeOpen(true)}
        >
          <CalendarDays size={14} />
          {ranges.find(([id]) => id === range)?.[1]}
          <ChevronDown size={13} />
        </button>
      </div>
      {timeOpen && (
        <TimeRange
          range={range}
          from={from}
          to={to}
          live={live}
          anchor={timeButton.current}
          close={() => setTimeOpen(false)}
          apply={(next) => {
            setRange(next.range);
            setFrom(next.from);
            setTo(next.to);
            setLive(next.live);
            resetPages();
            setTimeOpen(false);
          }}
        />
      )}
      {(error || state?.error) && (
        <p role="alert" className="form-error">
          {error || state?.error}
        </p>
      )}
      <div className="usage-navigation">
        <div className="usage-tabs" role="tablist" aria-label="用量明细">
          {[
            ["requests", "请求日志"],
            ["providers", "供应商"],
            ...(charted ? [["curves", "曲线"]] : []),
            ["models", "模型"],
            ...(keyed ? [["keys", "Key"]] : []),
            ["claude", "Claude Code"],
            ["antigravity", "Antigravity"],
            ["pricing", "定价"],
          ].map(([id, n]) => (
            <button
              role="tab"
              data-tab={id}
              aria-selected={tab === id}
              key={id}
              onClick={() => void selectTab(id)}
            >
              {n}
            </button>
          ))}
        </div>
        {tab === "requests" && (
          <UsageLogControls
            status={status}
            onStatusChange={(value) => choose(setStatus, value)}
            settings={state?.settings}
            save={(settings) => {
              void command("set_usage_settings", { settings })
                .then(setState)
                .catch((e) => setError(errorOf(e).message));
            }}
          />
        )}
      </div>
      {tab === "requests" ? (
        <>
          <UsageLogTable
            rows={logs.rows}
            busy={rowsBusy}
            settings={state?.settings}
            name={name}
            open={(r) => void open(r)}
          />
          <Pagination page={logs.page} total={logs.total} onChange={setPage} />
        </>
      ) : independent ? (
        <>
          {tab === "claude" && <UsageClaudeQuota quota={claudeQuota} />}
          <UsageAntigravity
            client={tab as "claude" | "antigravity"}
            stats={agStats}
            quotaError={agQuotaError}
            busy={agBusy}
            currentAccountId={agCurrentAccount}
            selectedProvider={provider}
            providerName={name}
            onSelectProvider={(nextProvider) => {
              if (nextProvider) setClient(tab);
              setProvider(nextProvider);
              resetPages();
            }}
          />
        </>
      ) : curved ? (
        <UsageCurvesPanel
          curves={curvesGone ? null : (curves?.data ?? null)}
          stale={curves != null && !curvesRead && !curvesGone}
          unread={curvesGone}
          unrecorded={state?.settings.recording === false}
          selected={provider}
          name={(id) => name(id === "session" ? null : id)}
          onSelect={(id) => choose(setProvider, id)}
        />
      ) : tab === "pricing" ? (
        <Pricing
          onDirtyChange={priceDirty}
          filter={{ ...query(), client: client || keyOwner }}
        />
      ) : (
        <Ranking
          groups={
            tab === "providers"
              ? (data?.providers ?? [])
              : tab === "keys"
                ? // Of every key, whichever one the page is narrowed to.
                  (data?.entryKeys ?? [])
                : (data?.models ?? []).filter(
                    ({ totals }) =>
                      totals.success > 0 ||
                      Object.values(totals.tokens).some(
                        (value) => (value ?? 0) > 0,
                      ) ||
                      Number(totals.cost) !== 0,
                  )
          }
          name={(id) =>
            tab === "providers"
              ? name(id === "session" ? null : id)
              : tab === "keys"
                ? keyLabel(id)
                : id === "web_search"
                  ? "网络搜索"
                  : id
          }
          kind={
            tab === "providers" ? "供应商" : tab === "keys" ? "Key" : "模型"
          }
          view={rankings[tab]}
          onChange={(v) =>
            setRankings((previous) => ({ ...previous, [tab]: v }))
          }
        />
      )}
      {selected && (
        <Detail
          record={selected}
          name={name}
          keyName={keyName}
          onClose={() => {
            detailSequence.current++;
            setSelected(null);
          }}
        />
      )}
      {billOpen && (
        <UsageBill
          start={query().start ?? (data?.detailSince || undefined)}
          end={query().end}
          close={() => setBillOpen(false)}
          saved={(result) => {
            setBillOpen(false);
            setBillResult(
              result.cleaned || result.retained || result.cleanupError
                ? `已导出 ${numeric(result.requests)} 个请求、${numeric(result.attempts)} 次尝试，清理 ${numeric(result.cleaned)} 条已导出记录与对应统计${result.retained ? `，保留 ${numeric(result.retained)} 条更新记录` : ""}。${result.cleanupError ?? ""}`
                : `已导出 ${numeric(result.requests)} 个请求、${numeric(result.attempts)} 次尝试。`,
            );
            void load(true);
          }}
        />
      )}
      {sheet && state && (
        <Sources
          state={state}
          counts={data?.sources ?? {}}
          onClose={() => setSheet(false)}
          onChange={(s) => {
            stateSequence.current++;
            setState(s);
            void loadData(true);
          }}
        />
      )}
    </section>
  );
}
function Metric({
  label,
  value,
  title,
  extra,
}: {
  label: string;
  value: string;
  title?: string;
  extra?: string;
}) {
  const [focused, setFocused] = useState(false);
  return (
    <div className="usage-metric">
      <span>{label}</span>
      <strong
        tabIndex={0}
        title={title}
        aria-label={title}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        onKeyDown={(event) => {
          if (event.key === "Escape") setFocused(false);
        }}
      >
        {value}
      </strong>
      {focused && title && (
        <span className="usage-exact-value" role="tooltip">
          {title}
        </span>
      )}
      {extra && <em>{extra}</em>}
    </div>
  );
}
function NumberValue({
  value,
  missing = "历史缺失",
}: {
  value: number | null | undefined;
  missing?: string;
}) {
  const [focused, setFocused] = useState(false);
  return (
    <span
      className="usage-number"
      tabIndex={0}
      title={value == null ? missing : numeric(value)}
      aria-label={value == null ? missing : numeric(value)}
      onFocus={() => setFocused(true)}
      onBlur={() => setFocused(false)}
    >
      {value == null ? "—" : focused ? numeric(value) : compact(value)}
    </span>
  );
}
function TimeRange({
  range,
  from,
  to,
  live,
  anchor,
  close,
  apply,
}: {
  range: string;
  from: string;
  to: string;
  live: boolean;
  anchor: HTMLButtonElement | null;
  close: () => void;
  apply: (value: {
    range: string;
    from: string;
    to: string;
    live: boolean;
  }) => void;
}) {
  const [draft, setDraft] = useState({ range, from, to, live }),
    [error, setError] = useState("");
  const ref = useRef<HTMLDialogElement>(null);
  const customExpanded = draft.range === "custom";
  useEffect(() => {
    const dialog = ref.current;
    if (dialog && !dialog.open) dialog.showModal();
    return () => {
      dialog?.close();
      anchor?.focus({ preventScroll: true });
    };
  }, [anchor]);
  useEffect(() => {
    const dialog = ref.current;
    if (dialog && anchor) {
      const rect = anchor.getBoundingClientRect();
      const width = customExpanded ? 316 : 196;
      dialog.style.left = `${Math.max(12, Math.min(rect.right - width, window.innerWidth - width - 12))}px`;
      dialog.style.top = `${Math.min(rect.bottom + 6, Math.max(12, window.innerHeight - dialog.offsetHeight - 12))}px`;
    }
  }, [anchor, customExpanded]);
  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    if (
      !Number.isFinite(new Date(draft.from).getTime()) ||
      !Number.isFinite(new Date(draft.to).getTime()) ||
      new Date(draft.from).getTime() >
        (draft.live ? Date.now() : new Date(draft.to).getTime())
    ) {
      setError("请选择有效时间范围");
      return;
    }
    apply({ ...draft, range: "custom" });
  };
  return (
    <dialog
      ref={ref}
      className={`usage-range-dialog${customExpanded ? " custom-open" : ""}`}
      aria-label="选择时间范围"
      onCancel={(e) => {
        e.preventDefault();
        close();
      }}
      onClick={(e) => {
        if (e.target === ref.current) {
          const r = e.currentTarget.getBoundingClientRect();
          if (
            e.clientX < r.left ||
            e.clientX > r.right ||
            e.clientY < r.top ||
            e.clientY > r.bottom
          )
            close();
        }
      }}
    >
      <div className="usage-range-menu" role="group" aria-label="时间范围选项">
        {ranges.map(([v, n]) => {
          const active = draft.range === v;
          return (
            <button
              type="button"
              key={v}
              className={`usage-range-option${active ? " active" : ""}`}
              aria-pressed={active}
              onClick={() => {
                setError("");
                if (v === "custom") {
                  setDraft((prev) => ({ ...prev, range: "custom" }));
                  return;
                }
                apply({ range: v, from, to, live: true });
              }}
            >
              <span>{n}</span>
            </button>
          );
        })}
      </div>
      {customExpanded && (
        <form className="usage-range-custom" onSubmit={submit}>
          <div className="usage-range-fields">
            <label className="usage-field">
              开始时间
              <input
                type="datetime-local"
                value={draft.from}
                onChange={(e) => setDraft({ ...draft, from: e.target.value })}
              />
            </label>
            <label className="usage-field">
              结束时间
              <input
                type="datetime-local"
                value={draft.to}
                disabled={draft.live}
                onChange={(e) => setDraft({ ...draft, to: e.target.value })}
              />
            </label>
            <label className="usage-range-live">
              <input
                type="checkbox"
                checked={draft.live}
                onChange={(e) => setDraft({ ...draft, live: e.target.checked })}
              />
              跟随当前
            </label>
          </div>
          {error && (
            <p role="alert" className="form-error">
              {error}
            </p>
          )}
          <div className="usage-form-actions">
            <button type="button" onClick={close}>
              取消
            </button>
            <button className="primary">确定</button>
          </div>
        </form>
      )}
    </dialog>
  );
}
function Pagination({
  page,
  total,
  onChange,
}: {
  page: number;
  total: number;
  onChange: (page: number) => void;
}) {
  const count = Math.max(1, Math.ceil(total / 20));
  const pages = [1, page - 1, page, page + 1, count]
    .filter((n, i, a) => n > 0 && n <= count && a.indexOf(n) === i)
    .sort((a, b) => a - b);
  return (
    <div className="usage-pagination">
      <span title={numeric(total)}>{compact(total)} 条</span>
      <button
        className="icon-button"
        aria-label="上一页"
        disabled={page <= 1}
        onClick={() => onChange(page - 1)}
      >
        <ChevronLeft size={16} />
      </button>
      {pages.map((n, i) => (
        <span key={n}>
          {i > 0 && n - pages[i - 1] > 1 && <span>…</span>}
          <button
            aria-current={n === page ? "page" : undefined}
            onClick={() => onChange(n)}
          >
            {n}
          </button>
        </span>
      ))}
      <button
        className="icon-button"
        aria-label="下一页"
        disabled={page >= count}
        onClick={() => onChange(page + 1)}
      >
        <ChevronRight size={16} />
      </button>
      <input
        aria-label="跳转页码"
        type="number"
        min="1"
        max={count}
        key={`${page}-${count}`}
        defaultValue={page}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            const n = Number(e.currentTarget.value);
            if (Number.isInteger(n) && n > 0 && n <= count) onChange(n);
          }
        }}
      />
    </div>
  );
}
function Ranking({
  groups,
  name,
  kind,
  view,
  onChange,
}: {
  groups: Group[];
  name: (id: string) => string;
  kind: string;
  view: RankingView;
  onChange: (view: RankingView) => void;
}) {
  const sorted = [...groups].sort((a, b) => {
    const value = (g: Group) =>
      view.sort === "cost"
        ? Number(g.totals.cost)
        : view.sort === "tokens"
          ? tokenTotal(g.totals.tokens)
          : view.sort === "firstToken"
            ? averageFirstToken(g.totals)
            : g.totals.requests;
    const av = value(a),
      bv = value(b);
    if (av == null || bv == null)
      return av == null ? (bv == null ? a.id.localeCompare(b.id) : 1) : -1;
    return (av - bv) * (view.asc ? 1 : -1) || a.id.localeCompare(b.id);
  });
  const metricOf = (g: Group) =>
    view.sort === "cost"
      ? Number(g.totals.cost)
      : view.sort === "tokens"
        ? (tokenTotal(g.totals.tokens) ?? 0)
        : view.sort === "firstToken"
          ? (averageFirstToken(g.totals) ?? 0)
          : g.totals.requests;
  const maxMetric = sorted.reduce(
    (max, g) => Math.max(max, metricOf(g) || 0),
    0,
  );
  const page = Math.min(view.page, Math.max(1, Math.ceil(sorted.length / 20)));
  return (
    <>
      <div className="usage-table-scroll">
        <table className="usage-table usage-ranking">
          <thead>
            <tr>
              <th>{kind}</th>
              {[
                ["requests", "请求"],
                ["tokens", "Token"],
                ["cost", "费用"],
              ].map(([key, label]) => (
                <th key={key}>
                  <button
                    onClick={() =>
                      onChange({
                        ...view,
                        page: 1,
                        sort: key as RankingView["sort"],
                        asc: view.sort === key ? !view.asc : false,
                      })
                    }
                  >
                    {label}
                    {view.sort === key &&
                      (view.asc ? (
                        <ArrowUp size={12} />
                      ) : (
                        <ArrowDown size={12} />
                      ))}
                  </button>
                </th>
              ))}
              <th>成功率</th>
              <th>
                <button
                  onClick={() =>
                    onChange({
                      ...view,
                      page: 1,
                      sort: "firstToken",
                      asc: view.sort === "firstToken" ? !view.asc : true,
                    })
                  }
                >
                  平均首字
                  {view.sort === "firstToken" &&
                    (view.asc ? (
                      <ArrowUp size={12} />
                    ) : (
                      <ArrowDown size={12} />
                    ))}
                </button>
              </th>
            </tr>
          </thead>
          <tbody>
            {sorted.slice((page - 1) * 20, page * 20).map((group) => {
              const { id, totals: t } = group;
              const ratio =
                maxMetric > 0
                  ? Math.min(
                      100,
                      Math.max(2, (metricOf(group) / maxMetric) * 100),
                    )
                  : 0;
              return (
                <tr key={id}>
                  <td title={name(id)}>
                    <div className="usage-rank-cell">
                      <span>{name(id)}</span>
                      <i className="usage-rank-bar" aria-hidden="true">
                        <b style={{ width: `${ratio}%` }} />
                      </i>
                    </div>
                  </td>
                  <td>
                    <NumberValue value={t.requests} />
                  </td>
                  <td>
                    <NumberValue value={tokenTotal(t.tokens)} />
                  </td>
                  <td title={t.cost}>{money(t.cost)}</td>
                  <td>{successRate(t)}</td>
                  <td>
                    <FirstTokenValue totals={t} />
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {!sorted.length && <div className="usage-empty">暂无数据</div>}
      </div>
      <Pagination
        page={page}
        total={sorted.length}
        onChange={(page) => onChange({ ...view, page })}
      />
    </>
  );
}
// The share of the requests whose result is known that succeeded. Where only
// a part of them has one, the rate says over how many it stands: three known
// failures among thousands of imported requests are not a rate of the whole.
function successRate(t: Totals) {
  if (!t.statusKnown) return "—";
  const rate = `${((t.success / t.statusKnown) * 100).toFixed(1)}%`;
  return t.statusKnown < t.requests
    ? `${rate}（${compact(t.statusKnown)} 笔已知）`
    : rate;
}
function FirstTokenValue({ totals }: { totals: Totals }) {
  const [focused, setFocused] = useState(false);
  return (
    <span
      tabIndex={0}
      className="usage-number"
      title={firstTokenTitle(totals)}
      aria-label={firstTokenTitle(totals)}
      onFocus={() => setFocused(true)}
      onBlur={() => setFocused(false)}
    >
      {focused
        ? firstTokenTitle(totals)
        : firstTokenLabel(averageFirstToken(totals))}
    </span>
  );
}
