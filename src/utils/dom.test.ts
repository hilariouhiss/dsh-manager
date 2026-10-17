import { describe, expect, it } from "vite-plus/test";

import { syncChecked } from "./dom";

describe("syncChecked（异步写入后按 store 真值回写开关）", () => {
  it("store 真值为 false 时，把用户点开的开关改回关闭", () => {
    const el = { checked: true };
    syncChecked(el, false);
    expect(el.checked).toBe(false);
  });

  it("store 真值为 true 时，把用户点关的开关改回开启", () => {
    const el = { checked: false };
    syncChecked(el, true);
    expect(el.checked).toBe(true);
  });
});
