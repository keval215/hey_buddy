import { describe, expect, it } from "vitest";
import {
  FADE_MS,
  HOLD_MS,
  easeOutCubic,
  isForDisplay,
  markerAt,
  opacityAt,
  parseDisplayParams,
  toViewport,
} from "./marker";

const left = { id: 2, x: -1920, y: -100, w: 1920, h: 1080 };

describe("parseDisplayParams", () => {
  it("parses negative origins", () => {
    expect(parseDisplayParams("?id=2&x=-1920&y=-100&w=1920&h=1080")).toEqual(left);
  });
  it("rejects missing, non-numeric and empty-size values", () => {
    expect(parseDisplayParams("")).toBeNull();
    expect(parseDisplayParams("?id=1&x=0&y=0&w=0&h=10")).toBeNull();
    expect(parseDisplayParams("?id=1&x=a&y=0&w=10&h=10")).toBeNull();
    expect(parseDisplayParams("?id=1&x=0&y=0&w=10")).toBeNull();
    expect(parseDisplayParams("?id=1&x=&y=0&w=10&h=10")).toBeNull();
  });
});

describe("display matching", () => {
  it("matches by display_id only", () => {
    expect(isForDisplay({ display_id: 2 }, left)).toBe(true);
    expect(isForDisplay({ display_id: 1 }, left)).toBe(false);
  });
});

describe("toViewport", () => {
  it("subtracts the display origin", () => {
    expect(toViewport({ x: -960, y: 440 }, left, { w: 1920, h: 1080 })).toEqual({
      x: 960,
      y: 540,
    });
  });
  it("scales physical px to CSS px (Windows 150% DPI)", () => {
    const d = { id: 1, x: 0, y: 0, w: 3000, h: 2000 };
    expect(toViewport({ x: 1500, y: 1000 }, d, { w: 2000, h: 1333.5 })).toEqual({
      x: 1000,
      y: 666.75,
    });
  });
  it("clamps to the viewport", () => {
    const d = { id: 1, x: 0, y: 0, w: 100, h: 100 };
    expect(toViewport({ x: -50, y: 500 }, d, { w: 100, h: 100 })).toEqual({ x: 0, y: 100 });
  });
});

describe("animation", () => {
  it("eases from 0 to 1 and clamps", () => {
    expect(easeOutCubic(0)).toBe(0);
    expect(easeOutCubic(1)).toBe(1);
    expect(easeOutCubic(2)).toBe(1);
    expect(easeOutCubic(-1)).toBe(0);
    expect(easeOutCubic(0.5)).toBeGreaterThan(0.5);
  });
  it("moves between points", () => {
    const a = { x: 0, y: 0 };
    const b = { x: 100, y: 50 };
    expect(markerAt(a, b, 0)).toEqual(a);
    expect(markerAt(a, b, 450)).toEqual(b);
    expect(markerAt(a, b, 10_000)).toEqual(b);
    expect(markerAt(a, b, 100, 0)).toEqual(b);
  });
  it("holds for ~3 s then fades out", () => {
    expect(opacityAt(0)).toBe(1);
    expect(opacityAt(HOLD_MS)).toBe(1);
    expect(opacityAt(HOLD_MS + FADE_MS / 2)).toBeCloseTo(0.5);
    expect(opacityAt(HOLD_MS + FADE_MS)).toBe(0);
    expect(opacityAt(60_000)).toBe(0);
  });
});
