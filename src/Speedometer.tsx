import "./speedometer.css";
import { useId } from "react";
import { speedLabel } from "./usage-speed";
import type { UsageRate, UsageSpeed } from "./usage-types";

// The drawing is 100 across and turns about its middle. Nothing spent stands
// at the lower left and the top of the scale at the lower right: the arc runs
// clockwise over the top and is open below, as a car's is.
const middle = 50;
const from = 150;
const sweep = 240;
const viewBox = "0 0 100 78";
// In the drawing's own units, so that one drawing serves both sizes: a small
// dial is the same thing in heavier strokes, to still read at half the size.
const shapes = {
  large: { radius: 43, stroke: 6, needle: 33, girth: 1.8, hub: 4.2 },
  small: { radius: 40, stroke: 10, needle: 31, girth: 3.6, hub: 7.5 },
};

const degrees = (level: number) => from + sweep * level;
/** Where a level stands, that far from the middle. */
const point = (radius: number, level: number) => {
  const angle = (degrees(level) * Math.PI) / 180;
  return [
    middle + radius * Math.cos(angle),
    middle + radius * Math.sin(angle),
  ].map((n) => Number(n.toFixed(2)));
};
/** The arc from one level to another, the way the needle climbs. */
const arc = (radius: number, start: number, end: number) =>
  `M${point(radius, start).join(" ")}A${radius} ${radius} 0 ${
    (end - start) * sweep > 180 ? 1 : 0
  } 1 ${point(radius, end).join(" ")}`;

/**
 * A rate on a dial, with its reading beneath: the arc is lit and the needle
 * stands as far up the scale as the rate's `level` says, and both move to a
 * new reading rather than jump to it.
 */
export default function Speedometer({
  rate,
  label,
  name,
  size = "small",
  color,
  marks,
}: {
  rate: UsageRate;
  /** What it is the rate of, for who cannot see the dial. */
  label: string;
  /** What to call it beneath the dial, where its place does not say. */
  name?: string;
  size?: "large" | "small";
  /** What the arc is lit in; without one it runs from cold to hot. */
  color?: string;
  /** The scale's marks, drawn where each one's `level` puts it. */
  marks?: UsageSpeed["dial"];
}) {
  const gradient = useId();
  const { radius, stroke, needle, girth, hub } = shapes[size];
  const reading = speedLabel(rate);
  const lit = {
    className: "speedometer-lit",
    d: arc(radius, 0, 1),
    // The paint is named on the element: a fragment in a stylesheet is not
    // everywhere taken to be of the page.
    stroke: color ?? `url(#${gradient})`,
    strokeWidth: stroke,
    pathLength: 100,
    // One dash twice as long as the arc, drawn back by what is not lit: a
    // reading that overshoots as it settles then shows no gap at either end.
    style: { strokeDashoffset: 200 - 100 * rate.level },
  };
  const inner = radius - stroke / 2;
  const last = marks?.at(-1);
  // A round end reaches half a stroke past where the line stops.
  const cap = stroke / 2 / ((radius * sweep * Math.PI) / 180);
  return (
    <div
      className={`speedometer is-${size}${rate.level > 0 ? " is-live" : ""}`}
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={1}
      aria-valuenow={rate.level}
      aria-valuetext={reading}
    >
      <span className="speedometer-dial">
        {/* The glow is the lit arc again, blurred: a filter is taken everywhere
            of a drawing as a whole, and not of a shape inside one. */}
        <svg className="speedometer-aura" viewBox={viewBox} aria-hidden="true">
          <path {...lit} />
        </svg>
        <svg viewBox={viewBox} aria-hidden="true">
          {!color && (
            <defs>
              <linearGradient
                id={gradient}
                gradientUnits="userSpaceOnUse"
                x1={middle - radius}
                x2={middle + radius}
                y1="0"
                y2="0"
              >
                <stop offset="0" className="speedometer-cold" />
                <stop offset="0.5" className="speedometer-warm" />
                <stop offset="1" className="speedometer-hot" />
              </linearGradient>
            </defs>
          )}
          <path
            className="speedometer-track"
            d={arc(radius, 0, 1)}
            strokeWidth={stroke}
          />
          {/* From the last mark to the top of the scale the track is red; a
              scale whose last mark is its top has none of it. */}
          {last && last.level + cap < 1 && (
            <path
              className="speedometer-zone"
              d={arc(radius, last.level + cap, 1)}
              strokeWidth={stroke}
            />
          )}
          {marks?.map((mark) => {
            const [x, y] = point(inner - 14.5, mark.level);
            return (
              <g
                key={mark.label}
                className={`speedometer-mark${mark === last ? " is-last" : ""}`}
              >
                <path
                  d={`M${point(inner - 1.5, mark.level).join(" ")}L${point(inner - 4.5, mark.level).join(" ")}`}
                />
                <text x={x} y={y}>
                  {mark.label}
                </text>
              </g>
            );
          })}
          <path {...lit} />
          <path
            className="speedometer-needle"
            d={`M${middle} ${middle - girth}L${middle + needle} ${middle}L${middle} ${middle + girth}Z`}
            style={{ transform: `rotate(${degrees(rate.level)}deg)` }}
          />
          <circle
            className="speedometer-hub"
            cx={middle}
            cy={middle}
            r={hub}
            strokeWidth={girth}
          />
        </svg>
      </span>
      {name && <span className="speedometer-name">{name}</span>}
      <span
        className="speedometer-reading"
        title={rate.unpriced ? undefined : `${rate.costPerHour}/h`}
      >
        {reading}
      </span>
    </div>
  );
}
