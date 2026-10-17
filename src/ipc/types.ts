/**
 * 与 Rust 侧一一对应的 IPC 类型（主计划 §3.1、§3.2、§3.5）。
 * 本文件是前端侧唯一真源；新增错误码或事件名必须同时改 src-tauri/src/error.rs 与 ipc.rs。
 */

/** 镜像 src-tauri/src/error.rs 的错误码常量（主计划 §3.2，共 24 个）。 */
export const ERROR_CODES = {
  SETTINGS_CORRUPT: "settings.corrupt",
  PATH_PERMISSION_DENIED: "path.permission_denied",
  NETWORK_OFFLINE: "network.offline",
  NETWORK_PROXY_AUTH: "network.proxy_auth",
  NETWORK_TLS: "network.tls",
  REGISTRY_PARSE: "registry.parse",
  REGISTRY_HTTP: "registry.http",
  DOWNLOAD_INSUFFICIENT_SPACE: "download.insufficient_space",
  DOWNLOAD_INTEGRITY_MISMATCH: "download.integrity_mismatch",
  DOWNLOAD_NO_INTEGRITY: "download.no_integrity",
  DOWNLOAD_CANCELLED: "download.cancelled",
  INSTALL_PM_MISSING: "install.pm_missing",
  INSTALL_INSTANCES_RUNNING: "install.instances_running",
  INSTALL_FAILED: "install.failed",
  INSTANCE_ALREADY_RUNNING: "instance.already_running",
  INSTANCE_SPAWN_FAILED: "instance.spawn_failed",
  INSTANCE_PORT_IN_USE: "instance.port_in_use",
  INSTANCE_EXITED_IMMEDIATELY: "instance.exited_immediately",
  INSTANCE_STOP_TIMEOUT: "instance.stop_timeout",
  INSTANCE_FORCED_STOP_REQUIRED: "instance.forced_stop_required",
  INSTANCE_NOT_MANAGED: "instance.not_managed",
  AUTOSTART_DENIED: "autostart.denied",
  AUTOSTART_READ_FAILED: "autostart.read_failed",
  TASK_NOT_FOUND: "task.not_found",
} as const;

export type ErrorCode = (typeof ERROR_CODES)[keyof typeof ERROR_CODES];

/** 镜像 src-tauri/src/ipc.rs 的 EVENT_ 常量（主计划 §3.5，共 6 个）。 */
export const EVENTS = {
  EVENT_TASK_PROGRESS: "task:progress",
  EVENT_INSTANCE_STATE: "instance:state",
  EVENT_INSTANCE_OUTPUT: "instance:output",
  EVENT_INSTANCE_URL: "instance:url",
  EVENT_UPDATE_AVAILABLE: "update:available",
  EVENT_OPLOG_ENTRY: "oplog:entry",
} as const;

export type EventName = (typeof EVENTS)[keyof typeof EVENTS];

/**
 * 统一错误结构（主计划 §3.1）。Rust 侧带 #[serde(rename_all = "camelCase")]，
 * 因此 next_step 在 IPC 上是 nextStep。
 */
export interface AppError {
  code: string;
  reason: string;
  nextStep: string;
  raw: string | null;
  retryable: boolean;
}
