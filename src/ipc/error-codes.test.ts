// @ts-expect-error 本仓库未安装 @types/node（与 vite.config.ts 的既有处置一致），运行时由 vitest 解析 node:fs
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vite-plus/test";
import { ERROR_CODES, EVENTS } from "./types";

const ERROR_RS = readFileSync(new URL("../../src-tauri/src/error.rs", import.meta.url), "utf8");
const IPC_RS = readFileSync(new URL("../../src-tauri/src/ipc.rs", import.meta.url), "utf8");

/** 从 Rust 源码里抽出 pub const <前缀…>: &str = "值"; 形式的常量表。 */
function rustConstants(source: string, prefix: string): Record<string, string> {
  const found: Record<string, string> = {};
  const pattern = new RegExp("pub const (" + prefix + '\\w*): &str = "([^"]+)";', "g");
  for (const match of source.matchAll(pattern)) {
    found[match[1]] = match[2];
  }
  return found;
}

describe("错误码与事件名在两侧一致（防漂移）", () => {
  // 整表比较：名字与值的对应关系也在断言范围内，不能只比较两边的键集合与值集合。
  it("error.rs 的 pub const 与 types.ts 的 ERROR_CODES 同名同值", () => {
    expect(rustConstants(ERROR_RS, "")).toEqual({ ...ERROR_CODES });
  });

  it("error.rs 的错误码数量锁定为 24（主计划 §3.2）", () => {
    expect(Object.keys(rustConstants(ERROR_RS, "")).length).toBe(24);
  });

  it("ipc.rs 的 EVENT_ 常量与 types.ts 的 EVENTS 同名同值", () => {
    expect(rustConstants(IPC_RS, "EVENT_")).toEqual({ ...EVENTS });
  });
});
