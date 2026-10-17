/** 时间戳与日期的格式化。全部用原生 Date，**不引 dayjs**（§1.3 红线 3）。 */

function pad(n: number, width = 2): string {
  return String(n).padStart(width, "0");
}

/** Unix 纪元毫秒 → 本地时间 HH:mm:ss.SSS。 */
export function formatTimestamp(ms: number): string {
  const d = new Date(ms);
  return (
    pad(d.getHours()) +
    ":" +
    pad(d.getMinutes()) +
    ":" +
    pad(d.getSeconds()) +
    "." +
    pad(d.getMilliseconds(), 3)
  );
}

/**
 * 运行时长 → 中文可读文本。
 * null（启动时间读不到）如实显示「无法取得」，不填 0、不猜（FR-4.1）。
 */
export function formatDuration(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms)) return "无法取得";
  if (ms <= 0) return "0秒";

  const totalSeconds = Math.floor(ms / 1000);
  if (totalSeconds < 60) return totalSeconds + "秒";

  const totalMinutes = Math.floor(totalSeconds / 60);
  if (totalMinutes < 60) {
    return totalMinutes + "分" + pad(totalSeconds % 60) + "秒";
  }

  const hours = Math.floor(totalMinutes / 60);
  return hours + "小时" + pad(totalMinutes % 60) + "分";
}

/** 注册表返回的 ISO-8601 串 → 本地 YYYY-MM-DD HH:mm。解析不了给破折号。 */
export function formatDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return (
    d.getFullYear() +
    "-" +
    pad(d.getMonth() + 1) +
    "-" +
    pad(d.getDate()) +
    " " +
    pad(d.getHours()) +
    ":" +
    pad(d.getMinutes())
  );
}
