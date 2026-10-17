import { describe, expect, it } from "vite-plus/test";
import { createSSRApp } from "vue";
import { renderToString } from "vue/server-renderer";
import ErrorAlert from "./ErrorAlert.vue";
import type { AppError } from "../ipc/types";

async function render(error: AppError): Promise<string> {
  return await renderToString(createSSRApp(ErrorAlert, { error }));
}

const base: AppError = {
  code: "network.offline",
  reason: "网络请求未能完成：连接失败或超时",
  nextStep: "检查网络连接与系统代理设置后重试。",
  raw: null,
  retryable: false,
};

describe("ErrorAlert", () => {
  it("同时渲染原因与下一步（NFR-1 两段式）", async () => {
    const html = await render(base);
    expect(html).toContain("网络请求未能完成：连接失败或超时");
    expect(html).toContain("检查网络连接与系统代理设置后重试。");
    expect(html).toContain("原因");
    expect(html).toContain("下一步");
  });

  it("有原始错误时渲染它，没有时不渲染原始错误区", async () => {
    const withRaw = await render({ ...base, raw: "dns error: no such host" });
    expect(withRaw).toContain("dns error: no such host");
    expect(withRaw).toContain("原始错误");

    const withoutRaw = await render(base);
    expect(withoutRaw).not.toContain("原始错误");
  });

  it("retryable 为真时有重试按钮，为假时没有", async () => {
    // base.nextStep 里本身含有「重试」二字，因此断言按钮自身的标记而不是该子串
    expect(await render({ ...base, retryable: true })).toContain("btn-outline");
    expect(await render(base)).not.toContain("btn-outline");
  });
});
