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
/** 启动行为（主计划 §3.7，FR-5.2），对应 Rust 的 m0::settings::AutostartMode。 */
export type AutostartMode = "tray" | "window";

/**
 * 设置全表（主计划 §3.7，14 项）。字段名与 Rust 侧 #[serde(rename_all = "camelCase")]
 * 序列化后的键完全一致。
 */
export interface Settings {
  autoChannel: string | null;
  autoInstall: boolean;
  confirmBeforeInstall: boolean;
  launchArgs: string[];
  workDir: string;
  autoOpenBrowser: boolean;
  autoScroll: boolean;
  outputBufferLines: number;
  logMaxMiB: number;
  logKeepFiles: number;
  oplogMaxMiB: number;
  oplogKeepFiles: number;
  stopInstanceOnExit: boolean;
  autostartMode: AutostartMode;
}

/** update_settings 的入参（主计划 §3.4 命令 12）。 */
export type SettingsPatch = Partial<Settings>;

/** 操作记录结果（主计划 §3.6："ok" | "fail"）。 */
export type OplogResult = "ok" | "fail";

/** 操作记录实体（主计划 §3.6，对应 Rust 的 m0::oplog::OplogEntry）。 */
export interface OplogEntry {
  timestampMs: number;
  action: string;
  target: string;
  result: OplogResult;
  code: string | null;
  raw: string | null;
}

/** list_oplog 的出参（主计划 §3.4 命令 16）。 */
export interface OplogPage {
  entries: OplogEntry[];
  total: number;
}

/** export_diagnostics 与 export_output 共用的出参（主计划 §3.4 命令 10/15）。 */
export interface ExportResult {
  bytesWritten: number;
}

/** 通道三态（主计划 §3.6，对应 Rust 的 m1::version_check::ChannelState）。 */
export type ChannelState = "published" | "unpublished" | "failed";

/** 一条通道的对外状态（主计划 §3.6，对应 Rust 的 ChannelStatus）。 */
export interface ChannelStatus {
  channel: string;
  state: ChannelState;
  version: string | null;
  publishedAtIso: string | null;
  tarballUrl: string | null;
  lastSuccessAtMs: number | null;
  lastError: AppError | null;
}

/**
 * 版本对比结论（主计划 §3.4 命令 1）。由后端算好，前端只做文案映射，
 * 不得自己写一份 semver 比较（审计重要-8）。
 */
export type VersionVerdict = "newer" | "up_to_date" | "not_applicable";

/** check_updates 的出参（主计划 §3.4 命令 1，对应 Rust 的 CheckUpdatesReport）。 */
export interface CheckUpdatesReport {
  channels: ChannelStatus[];
  localVersion: string | null;
  versionApplicable: boolean;
  notApplicableReason: string | null;
  lastSuccessAtMs: number | null;
  verdicts: Record<string, VersionVerdict>;
}

/** §3.5 事件 update:available 的载荷（对应 Rust 的 UpdateNotice）。 */
export interface UpdateAvailablePayload {
  channel: string;
  version: string;
  needsConfirm: boolean;
}

/** task:progress 事件里的安装阶段（主计划 §3.5 冻结的四个取值）。 */
export type InstallPhase = "checking" | "downloading" | "verifying" | "installing";

/** §3.5 事件 task:progress 的载荷（对应 Rust 的 m1::installer::TaskProgress）。 */
export interface TaskProgressPayload {
  taskId: string;
  percent: number;
  remainingMs: number | null;
  phase: InstallPhase;
}

/** 实例状态（主计划 §3.5 的 instance:state 取值，对应 Rust 的 m3::InstanceState）。 */
export type InstanceState = "starting" | "running" | "stopping" | "stopped" | "failed";

/** list_instances 的出参（主计划 §3.4 命令 6，对应 Rust 的 m3::instance_discovery::InstanceInfo）。 */
export interface InstanceInfo {
  instanceId: string;
  managed: boolean;
  pid: number;
  startedAtMs: number | null;
  uptimeMs: number | null;
  url: string | null;
  state: InstanceState;
}

/** 包管理器种类（对应 Rust 的 ports::package_manager::PmKind）。 */
export type PmKind = "npm" | "pnpm" | "bun";

/**
 * 安装来源结论（对应 Rust 的 m1::version_check::OriginVerdict）。
 *
 * 序列化用外部标签：带载荷的变体是单键对象（`{ packageManager: "npm" }`），
 * 其余变体是字符串。M2 复用 M1 的枚举，前端镜像不得改成 `{ kind, pmKind }` 形状。
 */
export type OriginVerdict =
  | { packageManager: PmKind }
  | "officialInstaller"
  | "portable"
  | "unidentified";

/** 一条判定依据（对应 Rust 的 m2::origin_detect::Evidence）。 */
export interface Evidence {
  source: string;
  path: string;
  observed: string;
  supports: boolean;
}

/** detect_origin 的出参（主计划 §3.4 命令 5，对应 Rust 的 OriginReport）。 */
export interface OriginReport {
  verdict: OriginVerdict;
  evidences: Evidence[];
  missing: string[];
}

/** 一行实例输出（主计划 §3.6，对应 Rust 的 m3::output_buffer::OutputLine）。 */
export interface OutputLine {
  seq: number;
  timestampMs: number;
  text: string;
  isStderr: boolean;
}

/** list_output 的出参（主计划 §3.4 命令 9）。 */
export interface ListOutputResult {
  lines: OutputLine[];
  nextSeq: number;
  truncated: boolean;
}

/** §3.5 事件 instance:state 的载荷。 */
export interface InstanceStatePayload {
  instanceId: string;
  state: InstanceState;
  startedAtMs: number | null;
  uptimeMs: number | null;
}

/** §3.5 事件 instance:output 的载荷。 */
export interface InstanceOutputPayload {
  instanceId: string;
  seq: number;
  timestampMs: number;
  text: string;
  isStderr: boolean;
}

/** §3.5 事件 instance:url 的载荷（地址识别由 T-I3 接入，本任务不产出）。 */
export interface InstanceUrlPayload {
  instanceId: string;
  url: string;
}

/**
 * stop_instance 的出参（主计划 §3.4 命令 8，对应 Rust 的 StopResult）。
 * residualPids 非空时 stopped 必为 false：如实报告残留，不谎报成功。
 */
export interface StopResult {
  stopped: boolean;
  residualPids: number[];
  forced: boolean;
}
