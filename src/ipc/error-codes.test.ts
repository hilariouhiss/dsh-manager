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

function valuesOf(table: Record<string, string>): string[] {
  return Object.values(table).sort();
}

describe("错误码与事件名在两侧一致（防漂移）", () => {
  it("error.rs 的 pub const 与 types.ts 的 ERROR_CODES 同名同值", () => {
    const rust = rustConstants(ERROR_RS, "");
    expect(Object.keys(rust).sort()).toEqual(Object.keys(ERROR_CODES).sort());
    expect(valuesOf(rust)).toEqual(valuesOf(ERROR_CODES));
  });

  it("error.rs 的错误码数量锁定为 24（主计划 §3.2）", () => {
    expect(Object.keys(rustConstants(ERROR_RS, "")).length).toBe(24);
  });

  it("ipc.rs 的 EVENT_ 常量与 types.ts 的 EVENTS 同名同值", () => {
    const rust = rustConstants(IPC_RS, "EVENT_");
    expect(Object.keys(rust).sort()).toEqual(Object.keys(EVENTS).sort());
    expect(valuesOf(rust)).toEqual(valuesOf(EVENTS));
  });
});
