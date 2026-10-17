/** 运行实例视图的纯逻辑：停止确认强度与错误归一化。这些逻辑不碰 DOM，才能用 Vitest 钉住。 */

import type { AppError, InstanceInfo } from "../ipc/types";

import { formatDate } from "./format";

export interface StopConfirmSpec {
  strength: "normal" | "strong" | "force";
  title: string;
  message: string;
}

/** 启动时间的可读文本；读不到时如实说明，不拿 1970 冒充（FR-4.1）。 */
function startedText(instance: InstanceInfo): string {
  if (instance.startedAtMs === null) return "启动时间无法取得";
  return "启动于 " + formatDate(new Date(instance.startedAtMs).toISOString());
}

/**
 * 停止前的三档确认（§3.9 D-3）：
 * 本工具实例普通、外部实例强化、强制终止单独确认且必须写明可能丢失任务。
 */
export function stopConfirmSpec(instance: InstanceInfo, force: boolean): StopConfirmSpec {
  // 外部实例的标识本来就是 pid-<pid>：再写一遍 pid 只会让文案显得像复读。
  const who =
    instance.instanceId === "pid-" + instance.pid
      ? "实例 " + instance.instanceId + "（" + startedText(instance) + "）"
      : "实例 " +
        instance.instanceId +
        "（pid " +
        instance.pid +
        "，" +
        startedText(instance) +
        "）";

  if (force) {
    return {
      strength: "force",
      title: "强制终止实例",
      message: who + "在等待时限内没有退出。强制终止可能丢失它正在进行的任务，确定继续吗？",
    };
  }

  if (!instance.managed) {
    return {
      strength: "strong",
      title: "停止外部实例",
      message: who + "不是本工具启动的，它可能正在执行任务。停止它风险更高，确定继续吗？",
    };
  }

  return {
    strength: "normal",
    title: "停止实例",
    message: "将停止本工具启动的" + who + "。",
  };
}

const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);

/** 只接受 http(s)、无 userinfo 的地址：`127.0.0.1:3099@evil.com` 会解析成 evil.com。 */
function parseAddress(candidate: string): URL | null {
  try {
    const parsed = new URL(candidate);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
    if (parsed.username !== "" || parsed.password !== "") return null;
    return parsed;
  } catch {
    return null;
  }
}

function effectivePort(url: URL): string {
  if (url.port !== "") return url.port;
  return url.protocol === "https:" ? "443" : "80";
}

/** 只有本机回环地址才允许自动打开浏览器；第三方链接一律不自动开。 */
export function isLocalAddress(candidate: string): boolean {
  const parsed = parseAddress(candidate);
  return parsed !== null && LOOPBACK_HOSTS.has(parsed.hostname.toLowerCase());
}

/**
 * instance:url 事件里的地址是否属于该实例。
 *
 * T-I3 的提取器会从输出里识别任意可访问地址（第三方 API 链接也会命中），界面不能把它
 * 当成实例访问地址。解析后比较 origin（协议 + 主机 + 端口），不做字符串前缀比较：
 * `http://127.0.0.1:30990`、`http://127.0.0.1:3099@evil.com` 都不能冒充 3099。
 * 非本机地址只有在实例自己的 origin 完全一致时才接受（--host / --tls-cert）；
 * 列表拿不到地址（--port 0）时只认本机回环地址，不把第三方链接当访问入口。
 * 注意「0.0.0.0:3099 监听、输出给 127.0.0.1:3099」要靠端口兜底，主机写法不同 origin 不同。
 */
export function isInstanceAddress(known: string | null, candidate: string): boolean {
  const parsed = parseAddress(candidate);
  if (parsed === null) return false;
  const knownUrl = known === null ? null : parseAddress(known);
  if (knownUrl === null) return LOOPBACK_HOSTS.has(parsed.hostname.toLowerCase());
  if (parsed.origin === knownUrl.origin) return true;
  return (
    LOOPBACK_HOSTS.has(parsed.hostname.toLowerCase()) &&
    effectivePort(parsed) === effectivePort(knownUrl)
  );
}

/** 把 catch 到的未知值收窄成可展示的 AppError：后端错误原样返回，其余包成原因 + 下一步。 */
export function asAppError(value: unknown): AppError {
  if (typeof value === "object" && value !== null) {
    const candidate = value as Record<string, unknown>;
    if (
      typeof candidate.code === "string" &&
      typeof candidate.reason === "string" &&
      typeof candidate.nextStep === "string"
    ) {
      return {
        code: candidate.code,
        reason: candidate.reason,
        nextStep: candidate.nextStep,
        raw: typeof candidate.raw === "string" ? candidate.raw : null,
        retryable: candidate.retryable === true,
      };
    }
  }
  return {
    code: "unknown",
    reason: value instanceof Error ? value.message : String(value),
    nextStep: "重试一次；若反复失败，请到「设置」页导出诊断包反馈。",
    raw: null,
    retryable: true,
  };
}
