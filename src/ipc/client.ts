import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppError } from "./types";

/**
 * 命令调用。失败时 Tauri 把后端返回的 AppError 反序列化成 JS 对象抛出，
 * 这里不做任何包装与改写：调用方拿到的就是那五个字段。
 */
export async function invokeCmd<T>(name: string, args: Record<string, unknown> = {}): Promise<T> {
  return await invoke<T>(name, args);
}

/**
 * 订阅事件，返回注销函数。视图必须在卸载时调用它：不注销会导致输出重复渲染
 * （主计划 §3.5 订阅约束、设计文档 §6.2）。
 * 事件名一律取 `EVENTS` 里的常量，不得直接写字符串字面量（主计划 §3.5）。
 */
export async function on<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return await listen<T>(event, (message) => handler(message.payload));
}

/** 把 catch 到的未知值收窄成 AppError。 */
export function isAppError(value: unknown): value is AppError {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.code === "string" &&
    typeof candidate.reason === "string" &&
    typeof candidate.nextStep === "string" &&
    (candidate.raw === null || typeof candidate.raw === "string") &&
    typeof candidate.retryable === "boolean"
  );
}
