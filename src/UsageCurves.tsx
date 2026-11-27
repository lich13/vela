import {
  useMemo,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { Search } from "lucide-react";
import {
  compact,
  firstTokenLabel,
  numeric,
  type CurveFailures,
  type CurveProvider,
  type UsageCurves,
} from "./usage-types";
import "./usage-curves.css";

/**
 * The kinds of failure in the order they are stacked. Their colours were
 * checked in this order: neighbours stay apart for every kind of colour
 * vision, in both themes.
 */
const kinds = [
  { key: "limited", label: "429 限流" },
  { key: "server", label: "5xx" },
  { key: "rejected", label: "其他 4xx" },
  { key: "broken", label: "中断" },
] as const;
/** With fewer than this in a bucket, what the bucket says is drawn faint. */
const few = 5;
const day = 86_400_000;
/** The plot in the units its paths are written in; it is stretched to fit. */
const wide = 1000;
const tall = 100;
/** The tops an axis may end at: the first that holds what is drawn. */
const rateTops = [0.02, 0.05, 0.1, 0.2, 0.3, 0.5, 0.75, 1];
const timeTops = [
  250, 500, 1000, 2000, 3000, 5000, 8000, 10_000, 15_000, 20_000, 30_000,
  45_000, 60_000, 90_000, 120_000, 180_000, 300_000, 600_000,
];

const failedOf = (failures: CurveFailures) =>
  failures.limited + failures.rejected + failures.server + failures.broken;

function percent(share: number | null) {
  if (share == null) return "—";
  if (share === 0) return "0%";
  if (share < 0.001) return "<0.1%";
  const text = (share * 100).toFixed(1);
  return `${text.endsWith(".0") ? text.slice(0, -2) : text}%`;
}
const seconds = (ms: number) => `${Number((ms / 1000).toFixed(2))}s`;
const two = (n: number) => String(n).padStart(2, "0");
const date = (at: number) => {
  const d = new Date(at);
  return `${two(d.getMonth() + 1)}-${two(d.getDate())}`;
};
const clock = (at: number) => {
  const d = new Date(at);
  return `${two(d.getHours())}:${two(d.getMinutes())}`;
};
function stepLabel(step: number) {
  const minutes = Math.round(step / 60_000);
  if (minutes < 60) return `${minutes} 分钟`;
  if (minutes < 1440) return `${minutes / 60} 小时`;
  return "1 天";
}

type Point = {
  at: number;
  /** Where its middle falls across the plot, from 0 to 1. */
  x: number;
  ok: number;
  failures: CurveFailures | null;
  /** Attempts known to have ended; null where the journal does not reach. */
  attempts: number | null;
  rate: number | null;
  /** Where each kind's band ends, as a share of the attempts. */
  stack: number[] | null;
  p50: number | null;
  p90: number | null;
  samples: number;
};

function pointsOf(provider: CurveProvider, curves: UsageCurves): Point[] {
  const span = Math.max(1, curves.end - curves.start);
  return provider.buckets.map((bucket) => {
    const failures = bucket.failures;
    const attempts = failures ? bucket.ok + failedOf(failures) : null;
    let reached = 0;
    return {
      at: bucket.at,
      x: Math.min(
        1,
        Math.max(0, (bucket.at + curves.step / 2 - curves.start) / span),
      ),
      ok: bucket.ok,
      failures,
      attempts,
      rate: failures && attempts ? failedOf(failures) / attempts : null,
      stack:
        failures && attempts
          ? kinds.map(({ key }) => (reached += failures[key] / attempts))
          : null,
      p50: bucket.firstToken.p50,
      p90: bucket.firstToken.p90,
      samples: bucket.firstToken.samples,
    };
  });
}

/**
 * The stretches of buckets next to one another that `has` holds for. A
 * bucket in which nothing ended breaks the line: it is not a bucket of zero.
 */
function runsOf(points: Point[], step: number, has: (p: Point) => boolean) {
  const runs: Point[][] = [];
  let last: Point | null = null;
  for (const point of points) {
    if (!has(point)) {
      last = null;
      continue;
    }
    if (last && point.at - last.at <= step * 1.5)
      runs[runs.length - 1].push(point);
    else runs.push([point]);
    last = point;
  }
  return runs;
}
/** A bucket with none beside it is drawn as wide as a part of itself. */
const widen = (run: Point[], half: number): Point[] =>
  run.length > 1
    ? run
    : [
        { ...run[0], x: run[0].x - half },
        { ...run[0], x: run[0].x + half },
      ];
const ceiling = (tops: number[], value: number) =>
  tops.find((top) => top >= value) ?? value;
const px = (x: number) => Number((x * wide).toFixed(2));
const py = (value: number, top: number) =>
  Number((tall - Math.min(1, Math.max(0, value / top)) * tall).toFixed(2));
const trace = (run: Point[], y: (p: Point) => number) =>
  run.map((p, i) => `${i ? "L" : "M"}${px(p.x)} ${y(p)}`).join("");
const band = (
  run: Point[],
  upper: (p: Point) => number,
  lower: (p: Point) => number,
) =>
  `${trace(run, upper)}${[...run]
    .reverse()
    .map((p) => `L${px(p.x)} ${lower(p)}`)
    .join("")}Z`;
/**
 * Where the time axis is marked: on round hours of the clock, or on whole
 * days where the range is of several, seven marks at most.
 */
function ticksOf(start: number, end: number) {
  const span = Math.max(1, end - start);
  const every = [5, 10, 15, 30, 60, 120, 180, 360, 720]
    .map((minutes) => minutes * 60_000)
    .find((spacing) => span / spacing <= 6);
  const days = Math.max(1, Math.ceil(span / day / 6));
  const marks: number[] = [];
  const at = new Date(start);
  at.setHours(0, 0, 0, 0);
  while (at.getTime() <= end) {
    if (at.getTime() >= start) marks.push(at.getTime());
    if (every) at.setTime(at.getTime() + every);
    else at.setDate(at.getDate() + days);
  }
  return marks.map((mark) => ({ at: mark, x: (mark - start) / span }));
}

/** Everything of the plots that does not move with the pointer. */
function draw(curves: UsageCurves, points: Point[]) {
  const span = Math.max(1, curves.end - curves.start);
  const width = curves.step / span;
  const half = Math.max(width * 0.3, 0.0015);
  // A bucket of one failed attempt is a rate of 100%. The axis is fitted to
  // the buckets with enough in them, and the others end at its top.
  const rated = points.filter((p) => p.rate != null);
  const sure = rated.filter((p) => (p.attempts ?? 0) >= few);
  const rateTop = ceiling(
    rateTops,
    Math.max(0, ...(sure.length ? sure : rated).map((p) => p.rate ?? 0)) ||
      rateTops[0],
  );
  const timed = points.filter((p) => p.p50 != null);
  const steady = timed.filter((p) => p.samples >= few);
  const timeTop = ceiling(
    timeTops,
    Math.max(
      0,
      ...(steady.length ? steady : timed).map((p) => p.p90 ?? p.p50 ?? 0),
    ) || 1000,
  );
  const rateRuns = runsOf(points, curves.step, (p) => p.stack != null).map(
    (run) => widen(run, half),
  );
  const timeRuns = runsOf(points, curves.step, (p) => p.p50 != null).map(
    (run) => widen(run, half),
  );
  const edge = (p: Point, kind: number) =>
    py(kind < 0 ? 0 : (p.stack?.[kind] ?? 0), rateTop);
  const slow = (p: Point) => py(p.p90 ?? p.p50 ?? 0, timeTop);
  const middle = (p: Point) => py(p.p50 ?? 0, timeTop);
  const ticks = ticksOf(curves.start, curves.end);
  const volumes = points.map(
    (p) => p.ok + (p.failures ? failedOf(p.failures) : 0),
  );
  const volumeTop = Math.max(1, ...volumes);
  let peak: Point | null = null;
  for (const point of points)
    if ((point.failures?.limited ?? 0) > (peak?.failures?.limited ?? 0))
      peak = point;
  const blind = (since: number) =>
    since > curves.start ? Math.min(1, (since - curves.start) / span) : 0;
  return {
    rateTop,
    timeTop,
    grid: `M0 0H${wide}M0 ${tall / 2}H${wide}M0 ${tall}H${wide}${ticks
      .map((tick) => `M${px(tick.x)} 0V${tall}`)
      .join("")}`,
    bands: kinds.map(({ key }, kind) => ({
      key,
      d: rateRuns
        .filter((run) => run.some((p) => (p.failures?.[key] ?? 0) > 0))
        .map((run) =>
          band(
            run,
            (p) => edge(p, kind),
            (p) => edge(p, kind - 1),
          ),
        )
        .join(""),
    })),
    total: rateRuns
      .map((run) => trace(run, (p) => edge(p, kinds.length - 1)))
      .join(""),
    reach: timeRuns.map((run) => band(run, slow, middle)).join(""),
    wash: timeRuns.map((run) => band(run, middle, () => tall)).join(""),
    slow: timeRuns.map((run) => trace(run, slow)).join(""),
    middle: timeRuns.map((run) => trace(run, middle)).join(""),
    bars: points.map((p, i) => {
      const height = Math.max(4, (volumes[i] / volumeTop) * tall);
      const reach = Math.max(width * 0.31, 0.0012);
      return {
        at: p.at,
        x: px(p.x - reach),
        y: tall - height,
        width: px(reach * 2),
        height,
      };
    }),
    volumeTop,
    ticks,
    peak: peak as Point | null,
    peakY: peak ? edge(peak, 0) : 0,
    rateBlind: blind(curves.failuresSince),
    timeBlind: blind(curves.detailSince),
    ratedNone: rated.length === 0,
    timedNone: timed.length === 0,
  };
}

function Spark({ trend, top }: { trend: (number | null)[]; top: number }) {
  const width = Math.max(2, trend.length * 3 - 1);
  return (
    <svg
      className="curve-spark"
      width={width}
      height={16}
      viewBox={`0 0 ${width} 16`}
      aria-hidden="true"
    >
      {trend.map((share, i) => {
        const height = share ? Math.max(2, (share / top) * 16) : 1.5;
        return (
          <rect
            key={i}
            data-level={share == null ? "none" : share === 0 ? "calm" : "hot"}
            x={i * 3}
            y={16 - height}
            width={2}
            height={height}
            rx={0.5}
          />
        );
      })}
    </svg>
  );
}

/**
 * What dims the marks over the buckets with too few in them: a bucket of one
 * failed attempt is a rate of 100% and is not to be read as loudly as one of
 * a hundred. The marks fade from a bucket to the next rather than step.
 */
function Sure({
  id,
  points,
  weak,
}: {
  id: string;
  points: Point[];
  weak: (p: Point) => boolean;
}) {
  return (
    <defs>
      <linearGradient
        id={`${id}-fade`}
        gradientUnits="userSpaceOnUse"
        x1="0"
        y1="0"
        x2={wide}
        y2="0"
      >
        {points.map((p) => (
          <stop
            key={p.at}
            offset={p.x}
            stopColor="#fff"
            stopOpacity={weak(p) ? 0.34 : 1}
          />
        ))}
      </linearGradient>
      <mask
        id={id}
        maskUnits="userSpaceOnUse"
        x={-20}
        y={-20}
        width={wide + 40}
        height={tall + 40}
      >
        <rect
          x={-20}
          y={-20}
          width={wide + 40}
          height={tall + 40}
          fill={`url(#${id}-fade)`}
        />
      </mask>
    </defs>
  );
}
const thinRate = (p: Point) => p.rate != null && (p.attempts ?? 0) < few;
const thinTime = (p: Point) => p.p50 != null && p.samples < few;

function Stage({
  curves,
  points,
  table,
}: {
  curves: UsageCurves;
  points: Point[];
  table: boolean;
}) {
  const [active, setActive] = useState<number | null>(null);
  const drawn = useMemo(() => draw(curves, points), [curves, points]);
  const daily = curves.step >= day;
  const within = date(curves.start) === date(curves.end - 1);
  // A mark at midnight says which day begins there.
  const mark = (at: number) =>
    daily || clock(at) === "00:00" ? date(at) : clock(at);
  const when = (at: number) =>
    daily
      ? date(at)
      : `${within ? "" : `${date(at)} `}${clock(at)}–${clock(at + curves.step)}`;
  if (table)
    return (
      <div className="curve-table">
        <table>
          <thead>
            <tr>
              <th scope="col">时间</th>
              <th scope="col">成功</th>
              {kinds.map(({ key, label }) => (
                <th scope="col" key={key}>
                  {label}
                </th>
              ))}
              <th scope="col">错误率</th>
              <th scope="col">首字 P50</th>
              <th scope="col">首字 P90</th>
              <th scope="col">首字样本</th>
            </tr>
          </thead>
          <tbody>
            {points.map((p) => (
              <tr key={p.at}>
                <th scope="row">{when(p.at)}</th>
                <td>{numeric(p.ok)}</td>
                {kinds.map(({ key }) => (
                  <td key={key}>
                    {p.failures ? numeric(p.failures[key]) : "不可知"}
                  </td>
                ))}
                <td>{p.failures ? percent(p.rate) : "不可知"}</td>
                <td>{firstTokenLabel(p.p50)}</td>
                <td>{firstTokenLabel(p.p90)}</td>
                <td>{numeric(p.samples)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    );
  const shown = active == null ? null : (points[active] ?? null);
  const move = (event: PointerEvent<HTMLDivElement>) => {
    const box = event.currentTarget.getBoundingClientRect();
    if (!box.width || !points.length) return;
    const at = (event.clientX - box.left) / box.width;
    let nearest = 0;
    points.forEach((p, i) => {
      if (Math.abs(p.x - at) < Math.abs(points[nearest].x - at)) nearest = i;
    });
    setActive(nearest);
  };
  const keys = (event: KeyboardEvent<HTMLDivElement>) => {
    const last = points.length - 1;
    const next =
      event.key === "ArrowRight"
        ? Math.min(last, (active ?? -1) + 1)
        : event.key === "ArrowLeft"
          ? Math.max(0, (active ?? points.length) - 1)
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? last
              : event.key === "Escape"
                ? null
                : undefined;
    if (next === undefined || last < 0) return;
    event.preventDefault();
    setActive(next);
  };
  const left = (x: number) => `${x * 100}%`;
  const cross = shown && (
    <i className="curve-cross" style={{ left: left(shown.x) }} />
  );
  const rateSure = points.some(thinRate) ? "url(#curve-sure-rate)" : undefined;
  const timeSure = points.some(thinTime) ? "url(#curve-sure-time)" : undefined;
  const scale = (top: number, label: (value: number) => string) =>
    [1, 0.5, 0].map((share) => (
      <span
        key={share}
        className="curve-tick-y"
        style={{ top: `${(1 - share) * 100}%` }}
      >
        {label(top * share)}
      </span>
    ));
  const ink = {
    viewBox: `0 0 ${wide} ${tall}`,
    preserveAspectRatio: "none",
    "aria-hidden": true,
  } as const;
  const failed = shown?.failures ? failedOf(shown.failures) : 0;
  const thin =
    shown != null &&
    ((shown.attempts != null && shown.attempts < few) ||
      (shown.p50 != null && shown.samples < few));
  return (
    <div
      className="curve-stage"
      role="group"
      tabIndex={0}
      aria-label="逐格读数，按左右方向键移动"
      onKeyDown={keys}
      onBlur={() => setActive(null)}
      onPointerLeave={() => setActive(null)}
    >
      <div className="curve-caption">
        <h3>错误率</h3>
        <span>
          <i className="curve-key" data-line="total" />
          合计，色带为各类失败
        </span>
      </div>
      <div className="curve-plot" data-plot="rate" onPointerMove={move}>
        {scale(drawn.rateTop, percent)}
        <svg className="curve-ink" {...ink}>
          {rateSure && (
            <Sure id="curve-sure-rate" points={points} weak={thinRate} />
          )}
          <path className="curve-grid" d={drawn.grid} />
          <g mask={rateSure}>
            {drawn.bands.map(
              ({ key, d }) =>
                d && (
                  <path
                    key={key}
                    className="curve-band"
                    data-kind={key}
                    d={d}
                  />
                ),
            )}
          </g>
        </svg>
        <svg className="curve-ink curve-glow" {...ink}>
          <g mask={rateSure}>
            <path className="curve-line" data-line="total" d={drawn.total} />
          </g>
        </svg>
        {drawn.rateBlind > 0 && (
          <div className="curve-blind" style={{ width: left(drawn.rateBlind) }}>
            <span>早于运行日志</span>
          </div>
        )}
        {drawn.peak?.failures && (
          <div
            className="curve-peak"
            data-side={drawn.peak.x > 0.6 ? "before" : "after"}
            style={{ left: left(drawn.peak.x), height: `${drawn.peakY}%` }}
          >
            <span>429 峰值 {numeric(drawn.peak.failures.limited)} 次</span>
          </div>
        )}
        {drawn.ratedNone && (
          <p className="curve-hollow">
            {drawn.rateBlind >= 1
              ? "这段时间早于运行日志，失败次数不可知"
              : "没有已知结果的请求"}
          </p>
        )}
        {cross}
        {shown?.rate != null && (
          <i
            className="curve-dot"
            data-of="total"
            style={{
              left: left(shown.x),
              top: `${py(shown.rate, drawn.rateTop)}%`,
            }}
          />
        )}
      </div>
      <div className="curve-plot" data-plot="volume" onPointerMove={move}>
        <span className="curve-tick-y" style={{ top: "50%" }}>
          请求量
        </span>
        <svg className="curve-ink" {...ink}>
          {drawn.bars.map((bar) => (
            <rect
              key={bar.at}
              className="curve-bar"
              x={bar.x}
              y={bar.y}
              width={bar.width}
              height={bar.height}
            />
          ))}
        </svg>
        {cross}
      </div>
      <div className="curve-caption">
        <h3>首字</h3>
        <span>
          <i className="curve-key" data-line="p50" />
          中位 P50
          <i className="curve-key" data-line="p90" />
          P90
        </span>
      </div>
      <div className="curve-plot" data-plot="time" onPointerMove={move}>
        {scale(drawn.timeTop, seconds)}
        <svg className="curve-ink" {...ink}>
          <defs>
            <linearGradient
              id="curve-wash"
              gradientUnits="userSpaceOnUse"
              x1="0"
              y1="0"
              x2="0"
              y2={tall}
            >
              <stop offset="0" className="curve-wash-from" />
              <stop offset="1" className="curve-wash-to" />
            </linearGradient>
          </defs>
          {timeSure && (
            <Sure id="curve-sure-time" points={points} weak={thinTime} />
          )}
          <path className="curve-grid" d={drawn.grid} />
          <g mask={timeSure}>
            <path
              className="curve-wash"
              fill="url(#curve-wash)"
              d={drawn.wash}
            />
            <path className="curve-reach" d={drawn.reach} />
            <path className="curve-line" data-line="p90" d={drawn.slow} />
          </g>
        </svg>
        <svg className="curve-ink curve-glow" {...ink}>
          <g mask={timeSure}>
            <path className="curve-line" data-line="p50" d={drawn.middle} />
          </g>
        </svg>
        {drawn.timeBlind > 0 && (
          <div className="curve-blind" style={{ width: left(drawn.timeBlind) }}>
            <span>只留有日汇总</span>
          </div>
        )}
        {drawn.timedNone && <p className="curve-hollow">没有首字样本</p>}
        {cross}
        {shown?.p50 != null && (
          <>
            {shown.p90 != null && (
              <i
                className="curve-dot"
                data-of="p90"
                style={{
                  left: left(shown.x),
                  top: `${py(shown.p90, drawn.timeTop)}%`,
                }}
              />
            )}
            <i
              className="curve-dot"
              data-of="p50"
              style={{
                left: left(shown.x),
                top: `${py(shown.p50, drawn.timeTop)}%`,
              }}
            />
          </>
        )}
      </div>
      <div className="curve-axis" aria-hidden="true">
        {drawn.ticks.map((tick) => (
          <span
            key={tick.at}
            data-edge={
              tick.x < 0.04 ? "start" : tick.x > 0.96 ? "end" : undefined
            }
            style={{ left: left(tick.x) }}
          >
            {mark(tick.at)}
          </span>
        ))}
      </div>
      {shown && (
        <div
          className="curve-tip"
          role="status"
          data-side={shown.x > 0.5 ? "before" : "after"}
          style={{ "--at": shown.x } as CSSProperties}
        >
          <strong>{when(shown.at)}</strong>
          <dl>
            <div>
              <dt>错误率</dt>
              <dd>
                {!shown.failures
                  ? "不可知"
                  : shown.attempts
                    ? `${percent(shown.rate)}（${numeric(failed)} / ${numeric(shown.attempts)}）`
                    : "—"}
              </dd>
            </div>
            {kinds.map(
              ({ key, label }) =>
                shown.failures &&
                shown.failures[key] > 0 && (
                  <div key={key} data-kind={key}>
                    <dt>
                      <i />
                      {label}
                    </dt>
                    <dd>{numeric(shown.failures[key])}</dd>
                  </div>
                ),
            )}
            <div>
              <dt>成功</dt>
              <dd>{numeric(shown.ok)}</dd>
            </div>
            <div>
              <dt>首字 P50</dt>
              <dd>{firstTokenLabel(shown.p50)}</dd>
            </div>
            <div>
              <dt>首字 P90</dt>
              <dd>{firstTokenLabel(shown.p90)}</dd>
            </div>
            <div>
              <dt>首字样本</dt>
              <dd>{numeric(shown.samples)}</dd>
            </div>
          </dl>
          {!shown.failures ? (
            <p>早于运行日志，这一格失败了多少次不可知。</p>
          ) : (
            thin && <p>样本不足 {few} 个，仅供参考。</p>
          )}
        </div>
      )}
    </div>
  );
}

export default function UsageCurvesPanel({
  curves,
  stale,
  selected,
  name,
  onSelect,
}: {
  curves: UsageCurves | null;
  /** What is shown is of another time or client than the one being read. */
  stale: boolean;
  /** The provider the page is narrowed to, empty for none. */
  selected: string;
  name: (id: string) => string;
  onSelect: (id: string) => void;
}) {
  const [find, setFind] = useState("");
  const [table, setTable] = useState(false);
  const providers = curves?.providers;
  // Where the page names none, the one rate-limited the most is the one to
  // look at first.
  const chosen =
    (selected
      ? providers?.find((provider) => provider.id === selected)
      : providers?.[0]) ?? null;
  const points = useMemo(
    () => (curves && chosen ? pointsOf(chosen, curves) : []),
    [curves, chosen],
  );
  if (!curves || !providers)
    return (
      <div className="usage-empty" aria-busy="true">
        正在读取曲线…
      </div>
    );
  if (!providers.length)
    return (
      <div className="usage-empty">
        这段时间没有经过网关的请求。换一个时间范围或客户端再看。
      </div>
    );
  const wanted = find.trim().toLowerCase();
  const listed = providers.filter((provider) =>
    name(provider.id).toLowerCase().includes(wanted),
  );
  // One scale for every row, so that the rows compare.
  const sparkTop = Math.max(
    0.02,
    ...providers.flatMap((provider) =>
      provider.trend.map((share) => share ?? 0),
    ),
  );
  const failed = chosen ? failedOf(chosen.failures) : 0;
  const attempts = chosen ? chosen.ok + failed : 0;
  const daily = curves.step >= day;
  const edge = (at: number) => (daily ? date(at) : `${date(at)} ${clock(at)}`);
  return (
    <section className="usage-curves" aria-busy={stale}>
      <aside className="curve-rail" aria-label="供应商">
        <label className="curve-find">
          <Search size={14} aria-hidden="true" />
          <input
            type="search"
            value={find}
            placeholder="筛选供应商"
            aria-label="筛选供应商"
            onChange={(event) => setFind(event.target.value)}
          />
        </label>
        <p className="curve-order">被限流最多的排在前面</p>
        {listed.length ? (
          <ul className="curve-picks">
            {listed.map((provider) => {
              const lost = failedOf(provider.failures);
              const ended = provider.ok + lost;
              return (
                <li key={provider.id}>
                  <button
                    type="button"
                    className="curve-pick"
                    aria-pressed={provider.id === chosen?.id}
                    onClick={() => onSelect(provider.id)}
                  >
                    <span className="curve-pick-name" title={name(provider.id)}>
                      {name(provider.id)}
                    </span>
                    <strong>{percent(ended ? lost / ended : null)}</strong>
                    <span className="curve-pick-more">
                      <Spark trend={provider.trend} top={sparkTop} />
                      <small>
                        {provider.failures.limited
                          ? `429 ${compact(provider.failures.limited)} 次`
                          : lost
                            ? `失败 ${compact(lost)} 次`
                            : ended
                              ? "无失败"
                              : "失败不可知"}
                        {" · "}
                        {ended
                          ? `共 ${compact(ended)}`
                          : `${compact(provider.requests)} 次请求`}
                      </small>
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        ) : (
          <p className="curve-none">没有名称包含“{find.trim()}”的供应商</p>
        )}
      </aside>
      <div className="curve-main">
        {chosen ? (
          <>
            <header className="curve-head">
              <div>
                <h2>{name(chosen.id)}</h2>
                <p>
                  {edge(curves.start)} 至 {edge(curves.end)}，每格{" "}
                  {stepLabel(curves.step)}
                </p>
              </div>
              <div className="segmented" role="group" aria-label="显示方式">
                <button
                  type="button"
                  aria-pressed={!table}
                  onClick={() => setTable(false)}
                >
                  曲线
                </button>
                <button
                  type="button"
                  aria-pressed={table}
                  onClick={() => setTable(true)}
                >
                  表格
                </button>
              </div>
            </header>
            <dl className="curve-figures">
              <div>
                <dt>错误率</dt>
                <dd>
                  <strong>
                    {percent(attempts ? failed / attempts : null)}
                  </strong>
                  <span>
                    {attempts
                      ? `${numeric(failed)} / ${numeric(attempts)} 次尝试失败`
                      : "没有已知结果的尝试"}
                  </span>
                </dd>
              </div>
              <div>
                <dt>429 限流</dt>
                <dd>
                  <strong>{numeric(chosen.failures.limited)}</strong>
                  <span>
                    {failed
                      ? `占失败的 ${percent(chosen.failures.limited / failed)}`
                      : "没有失败"}
                  </span>
                </dd>
              </div>
              <div>
                <dt>首字中位</dt>
                <dd>
                  <strong>{firstTokenLabel(chosen.firstToken.p50)}</strong>
                  <span>
                    {chosen.firstToken.samples
                      ? `${numeric(chosen.firstToken.samples)} 个样本`
                      : "没有首字样本"}
                  </span>
                </dd>
              </div>
              <div>
                <dt>首字 P90</dt>
                <dd>
                  <strong>{firstTokenLabel(chosen.firstToken.p90)}</strong>
                  <span>九成请求不慢于此</span>
                </dd>
              </div>
            </dl>
            <div className="curve-mix">
              <div className="curve-mix-bar" aria-hidden="true">
                {kinds.map(
                  ({ key }) =>
                    chosen.failures[key] > 0 && (
                      <i
                        key={key}
                        data-kind={key}
                        style={{ flexGrow: chosen.failures[key] }}
                      />
                    ),
                )}
              </div>
              <ul className="curve-legend" aria-label="失败构成">
                {kinds.map(({ key, label }) => (
                  <li key={key} data-kind={key}>
                    <i aria-hidden="true" />
                    {label}
                    <b>{numeric(chosen.failures[key])}</b>
                    {failed > 0 && (
                      <span>{percent(chosen.failures[key] / failed)}</span>
                    )}
                  </li>
                ))}
              </ul>
            </div>
            <Stage
              key={chosen.id}
              curves={curves}
              points={points}
              table={table}
            />
            <p className="curve-foot">
              错误率按每次尝试计：失败 ÷（成功 +
              失败），结果未知的请求不计入；淡显的时段样本不足 {few}{" "}
              个。失败取自运行日志（只保留最近 7
              天），成功与首字取自用量记录；请求类型与 Key 筛选对曲线不生效。
            </p>
          </>
        ) : (
          <div className="usage-empty">
            “{name(selected)}”在这段时间没有经过网关的请求，换一家看看。
          </div>
        )}
      </div>
    </section>
  );
}
