import { describe, expect, it } from "vite-plus/test";

import { formatDate, formatDuration, formatTimestamp } from "./format";

describe("formatTimestamp", () => {
  it("形如 HH:mm:ss.SSS", () => {
    expect(formatTimestamp(Date.now())).toMatch(/^\d{2}:\d{2}:\d{2}\.\d{3}$/);
  });

  it("毫秒补零到三位（用本地时间构造，避免时区依赖）", () => {
    const ms = new Date(2026, 9, 16, 13, 5, 7, 40).getTime();
    expect(formatTimestamp(ms)).toBe("13:05:07.040");
  });

  it("纪元零点是可渲染的", () => {
    expect(formatTimestamp(0)).toMatch(/^\d{2}:\d{2}:\d{2}\.\d{3}$/);
  });
});

describe("formatDuration", () => {
  it("读不到运行时长时如实显示「无法取得」", () => {
    expect(formatDuration(null)).toBe("无法取得");
  });

  it("0 与负值都归为 0 秒，不出现负数时长", () => {
    expect(formatDuration(0)).toBe("0秒");
    expect(formatDuration(-5000)).toBe("0秒");
  });

  it("秒 / 分秒 / 小时分三档", () => {
    expect(formatDuration(1000)).toBe("1秒");
    expect(formatDuration(65000)).toBe("1分05秒");
    expect(formatDuration(7_384_000)).toBe("2小时03分");
  });

  it("跨天时长用小时累加，不出现「25小时」以外的怪值", () => {
    expect(formatDuration(25 * 3_600_000)).toBe("25小时00分");
  });
});

describe("formatDate", () => {
  it("把注册表返回的 ISO 串渲染成 YYYY-MM-DD HH:mm", () => {
    const iso = new Date(2026, 9, 16, 13, 36).toISOString();
    expect(formatDate(iso)).toBe("2026-10-16 13:36");
  });

  it("非法 ISO 串给破折号，不抛异常", () => {
    expect(formatDate("not-a-date")).toBe("—");
    expect(formatDate("")).toBe("—");
  });
});
