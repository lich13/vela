import { render, screen, within } from "@testing-library/react";
import { expect, it } from "vitest";
import Speedometer from "./Speedometer";

const marks = [
  { label: "1", level: 0.24 },
  { label: "5", level: 0.46 },
  { label: "20", level: 0.66 },
  { label: "100", level: 0.9 },
];

it("reads a rate as a meter from nothing to the top of the scale", () => {
  render(
    <Speedometer
      rate={{ costPerHour: "12.3456", level: 0.5, unpriced: false }}
      label="Codex 速率"
      name="Codex"
      color="var(--accent)"
    />,
  );
  const meter = screen.getByRole("meter", { name: "Codex 速率" });
  expect(meter).toHaveAttribute("aria-valuemin", "0");
  expect(meter).toHaveAttribute("aria-valuemax", "1");
  expect(meter).toHaveAttribute("aria-valuenow", "0.5");
  expect(meter).toHaveAttribute("aria-valuetext", "$12.3/h");
  expect(meter).toHaveClass("is-small", "is-live");
  expect(within(meter).getByText("Codex")).toBeInTheDocument();
  // The figures are rounded to fit; the whole of it is a hover away.
  expect(within(meter).getByText("$12.3/h")).toHaveAttribute(
    "title",
    "$12.3456/h",
  );
});

it("turns the needle and lights the arc as far as the level", () => {
  const { container, rerender } = render(
    <Speedometer
      rate={{ costPerHour: "0", level: 0, unpriced: false }}
      label="Grok 速率"
      color="var(--accent-amber)"
    />,
  );
  const needle = container.querySelector<SVGElement>(".speedometer-needle")!;
  const lit = [...container.querySelectorAll<SVGElement>(".speedometer-lit")];
  // The arc itself and the glow under it.
  expect(lit).toHaveLength(2);
  // Nothing spent stands at the lower left, and nothing is lit.
  expect(needle.style.transform).toBe("rotate(150deg)");
  for (const arc of lit) expect(arc.style.strokeDashoffset).toBe("200");
  expect(screen.getByRole("meter")).not.toHaveClass("is-live");

  rerender(
    <Speedometer
      rate={{ costPerHour: "5", level: 0.5, unpriced: false }}
      label="Grok 速率"
      color="var(--accent-amber)"
    />,
  );
  // Half way up is straight up.
  expect(needle.style.transform).toBe("rotate(270deg)");
  for (const arc of lit) {
    expect(arc.style.strokeDashoffset).toBe("150");
    expect(arc).toHaveAttribute("stroke", "var(--accent-amber)");
  }

  rerender(
    <Speedometer
      rate={{ costPerHour: "200", level: 1, unpriced: false }}
      label="Grok 速率"
      color="var(--accent-amber)"
    />,
  );
  // The top of the scale is at the lower right, the long way round.
  expect(needle.style.transform).toBe("rotate(390deg)");
  for (const arc of lit) expect(arc.style.strokeDashoffset).toBe("100");
});

it("draws the marks it is given, and the track red past the last of them", () => {
  const { container } = render(
    <Speedometer
      size="large"
      rate={{ costPerHour: "8.5", level: 0.53, unpriced: false }}
      label="总速率"
      name="总速率"
      marks={marks}
    />,
  );
  const meter = screen.getByRole("meter", { name: "总速率" });
  expect(meter).toHaveClass("is-large");
  expect(
    [...container.querySelectorAll(".speedometer-mark text")].map(
      (mark) => mark.textContent,
    ),
  ).toEqual(["1", "5", "20", "100"]);
  expect(container.querySelectorAll(".speedometer-mark.is-last")).toHaveLength(
    1,
  );
  expect(container.querySelector(".speedometer-zone")).toBeInTheDocument();
  // Without a colour of its own it is lit from cold to hot.
  const gradient = container.querySelector("linearGradient")!;
  expect(gradient.querySelectorAll("stop")).toHaveLength(3);
  for (const arc of container.querySelectorAll(".speedometer-lit"))
    expect(arc).toHaveAttribute("stroke", `url(#${gradient.id})`);
});

it("draws no marks and no red on a dial given none", () => {
  const { container } = render(
    <Speedometer
      rate={{ costPerHour: "8.5", level: 0.53, unpriced: false }}
      label="Claude 速率"
      color="var(--accent-blue)"
    />,
  );
  expect(container.querySelector(".speedometer-mark")).toBeNull();
  expect(container.querySelector(".speedometer-zone")).toBeNull();
  expect(container.querySelector("linearGradient")).toBeNull();
});

it("reads what could not be priced as 未定价, with no figure behind it", () => {
  render(
    <Speedometer
      rate={{ costPerHour: "0", level: 0, unpriced: true }}
      label="DSH 速率"
      color="var(--accent-emerald)"
    />,
  );
  const meter = screen.getByRole("meter", { name: "DSH 速率" });
  expect(meter).toHaveAttribute("aria-valuetext", "未定价");
  expect(within(meter).getByText("未定价")).not.toHaveAttribute("title");
});
