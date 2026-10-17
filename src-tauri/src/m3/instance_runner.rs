//! M3 实例运行：spawn、流式输出、状态事件、停止与复核。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::error::{AppError, CmdResult};
use crate::m3::InstanceState;
use crate::m3::output_buffer::OutputBuffer;

/// 传给 CreateProcess 的 CREATE_NEW_PROCESS_GROUP：独立进程组，停止时能整组终止（FR-3.4）。
pub const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// 不弹出控制台窗口。
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 子进程退出后等待两个输出读者读到 EOF 的上限。超时只告警、仍然宣布 stopped：
/// 卡死的子进程不能把实例状态永远留在 running（修复 3）。
pub const OUTPUT_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// §3.5 `instance:state` 载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceStatePayload {
    pub instance_id: String,
    pub state: InstanceState,
    pub started_at_ms: Option<i64>,
    pub uptime_ms: Option<i64>,
}

/// §3.5 `instance:output` 载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceOutputPayload {
    pub instance_id: String,
    pub seq: u64,
    pub timestamp_ms: i64,
    pub text: String,
    pub is_stderr: bool,
}

/// §3.5 `instance:url` 载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceUrlPayload {
    pub instance_id: String,
    pub url: String,
}

/// M3 推送事件的出口。真实实现持有 `AppHandle`（装配点构造），测试用记录型实现。
///
/// 它不是 §6.1 的三个端口之一——它是 IPC 边界的实现细节，与 `TaskKill` 同类。
/// 由 `InstanceRunner` 持有，因此 `AppState` **不需要**任何事件相关字段：
/// 主计划 §3.3.2 冻结的 10 个字段里没有它，事件经 `AppHandle` 发出。
pub trait InstanceEmitter: Send + Sync {
    fn emit_state(&self, payload: InstanceStatePayload);
    fn emit_output(&self, payload: InstanceOutputPayload);
    fn emit_url(&self, payload: InstanceUrlPayload);
}

/// 一次启动的完整描述。命令层负责解析 dsh 启动器与设置项，M3 只负责 spawn。
#[derive(Debug, Clone)]
pub struct StartSpec {
    /// 实际交给 CreateProcess 的程序。
    pub program: PathBuf,
    /// 普通参数。
    pub args: Vec<String>,
    /// 需要原样写入命令行的参数（Windows 上给 cmd.exe 的内层命令行）。
    pub raw_arg: Option<String>,
    pub work_dir: PathBuf,
    pub instance_id: String,
    pub buffer_lines: usize,
    pub log_path: PathBuf,
    pub log_max_bytes: u64,
    /// 与 §0.4 的 LogWriter::open(_, _, keep_files: u32) 对齐。
    pub log_keep_files: u32,
}

/// Windows 上 `.cmd` / `.bat` 不能由 CreateProcess 直接执行，必须经 `cmd.exe /C`。
///
/// 与适配层里的同名判断是刻意的重复：设计文档 §2.3 禁止应用层依赖适配层。
fn is_script(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            e == "cmd" || e == "bat"
        })
        .unwrap_or(false)
}

fn quote_cmd_arg(arg: &str) -> String {
    let needs = arg.is_empty() || arg.chars().any(|c| " \t&|<>^()".contains(c));
    if needs {
        format!("\"{arg}\"")
    } else {
        arg.to_string()
    }
}

/// 启动器是不是 dsh 本体。只有它才需要补 web 子命令。
///
/// 判定用 file_stem()：dsh / dsh.cmd / dsh.exe 都算；
/// node、测试桩、任何别的程序一律不算——**给它们前置 web 会让程序把 web 当成自己的参数吃掉**。
fn launches_dsh(path: &Path) -> bool {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("dsh"))
        .unwrap_or(false)
}

/// 把设置里的默认启动参数与本次请求的参数合起来：**设置在前、请求在后**，后者可以覆盖前者。
///
/// FR-3.1（允许附加启动参数）与 §7.1 偏差 D-B（默认 --no-open，由本工具独占浏览器打开行为）
/// 的落点就是这个函数。
pub fn merge_launch_args(settings_args: &[String], request_args: &[String]) -> Vec<String> {
    let mut out = settings_args.to_vec();
    out.extend(request_args.iter().cloned());
    out
}

/// 由启动器路径与用户参数构造启动描述。**纯函数**，可离线单测。
#[allow(clippy::too_many_arguments)]
pub fn build_start_spec(
    launcher: &Path,
    user_args: &[String],
    work_dir: &Path,
    instance_id: &str,
    buffer_lines: usize,
    log_path: &Path,
    log_max_bytes: u64,
    log_keep_files: u32,
) -> StartSpec {
    let mut inner_args: Vec<String> = if launches_dsh(launcher) {
        vec!["web".to_string()]
    } else {
        Vec::new()
    };
    inner_args.extend(user_args.iter().cloned());

    let (program, args, raw_arg) = if is_script(launcher) {
        // 启动器路径总是加引号：cmd.exe 下带空格的路径不加引号会被切断。
        let mut inner = format!("\"{}\"", launcher.to_string_lossy());
        for a in &inner_args {
            inner.push(' ');
            inner.push_str(&quote_cmd_arg(a));
        }
        (
            PathBuf::from("cmd.exe"),
            vec!["/C".to_string()],
            Some(inner),
        )
    } else {
        (launcher.to_path_buf(), inner_args, None)
    };

    StartSpec {
        program,
        args,
        raw_arg,
        work_dir: work_dir.to_path_buf(),
        instance_id: instance_id.to_string(),
        buffer_lines,
        log_path: log_path.to_path_buf(),
        log_max_bytes,
        log_keep_files,
    }
}

/// taskkill 的唯一出口。**它不是端口**：进程终止是 M3 的实现细节（§3.3 契约约束）。
///
/// 测试用 `TaskKill::recording()` 替换真实调用，从而断言「拒绝路径没有发出任何终止命令」。
pub struct TaskKill {
    mode: KillMode,
}

enum KillMode {
    Real,
    Record(Arc<Mutex<Vec<(u32, bool)>>>),
    /// 只用于测试：模拟 taskkill 无法启动。
    #[cfg(test)]
    SpawnFail,
}

/// 系统 taskkill 的绝对路径；取不到 SystemRoot 时退回裸命令名让 CreateProcess 走 PATH。
pub fn taskkill_program() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| PathBuf::from(root).join("System32").join("taskkill.exe"))
        .unwrap_or_else(|| PathBuf::from("taskkill.exe"))
}

impl TaskKill {
    pub fn real() -> Self {
        Self {
            mode: KillMode::Real,
        }
    }

    /// 只记录、不真的终止进程。
    pub fn recording() -> Self {
        Self {
            mode: KillMode::Record(Arc::new(Mutex::new(Vec::new()))),
        }
    }

    /// 只用于测试：taskkill 无法启动，用来钉死「先复核、后定论」的时序。
    #[cfg(test)]
    pub fn spawn_failing() -> Self {
        Self {
            mode: KillMode::SpawnFail,
        }
    }

    /// 已发出的终止调用，按时间顺序的 `(pid, force)`。真实实现返回空表。
    pub fn calls(&self) -> Vec<(u32, bool)> {
        match &self.mode {
            KillMode::Record(v) => v.lock().expect("记录锁被毒化").clone(),
            #[cfg(test)]
            KillMode::SpawnFail => Vec::new(),
            KillMode::Real => Vec::new(),
        }
    }

    /// 终止整个进程树。`force == false` 时是 `taskkill /PID <pid> /T`（**不带 /F**）。
    ///
    /// 返回的是 taskkill 的退出码：非零（例如进程已不存在）**不作为错误**，最终结论由停止后的复核给出。
    pub async fn kill_tree(&self, pid: u32, force: bool) -> CmdResult<i32> {
        if let KillMode::Record(v) = &self.mode {
            v.lock().expect("记录锁被毒化").push((pid, force));
            return Ok(0);
        }

        #[cfg(test)]
        if matches!(&self.mode, KillMode::SpawnFail) {
            return Err(AppError::new(
                crate::error::INSTANCE_STOP_TIMEOUT,
                format!("无法执行 taskkill 终止进程 {pid}"),
                "先在本机命令行执行 taskkill /? 确认该命令可用，然后重试。",
            )
            .with_raw("测试注入：taskkill 未能启动")
            .retryable(true));
        }

        let mut args = vec!["/PID".to_string(), pid.to_string(), "/T".to_string()];
        if force {
            args.push("/F".to_string());
        }

        let mut cmd = Command::new(taskkill_program());
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.creation_flags(CREATE_NO_WINDOW);

        let out = cmd.output().await.map_err(|e| {
            AppError::new(
                crate::error::INSTANCE_STOP_TIMEOUT,
                format!("无法执行 taskkill 终止进程 {pid}"),
                "先在本机命令行执行 taskkill /? 确认该命令可用，然后重试。",
            )
            .with_raw(e.to_string())
            .retryable(true)
        })?;
        Ok(out.status.code().unwrap_or(-1))
    }
}

/// 端口被占用的输出特征（大小写不敏感）。配置化，便于按实测输出修正。
pub const PORT_IN_USE_PATTERNS: &[&str] = &[
    "eaddrinuse",
    "address already in use",
    "only one usage of each socket address",
    "port is already in use",
    "端口已被占用",
];

/// 失败上下文里回带的最后若干行输出（FR-3.6「最后若干行输出作为上下文」）。
pub const FAILURE_CONTEXT_LINES: usize = 20;

/// 启动宽限窗口：进程在这段时间内退出即判为启动失败。
pub const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_millis(800);

/// 由退出码与最后的输出判定启动失败属于哪一类（FR-3.6）。
pub fn classify_start_failure(lines: &[String], exit_code: i32) -> AppError {
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(FAILURE_CONTEXT_LINES)
        .rev()
        .map(String::as_str)
        .collect();
    let raw = tail.join("\n");
    let joined = lines.join("\n").to_ascii_lowercase();

    if PORT_IN_USE_PATTERNS.iter().any(|p| joined.contains(p)) {
        AppError::new(
            crate::error::INSTANCE_PORT_IN_USE,
            format!("启动失败：DSH 报告端口被占用（退出码 {exit_code}）"),
            "换一个端口再试：在启动参数里加上 --port 3099（或其他空闲端口）；也可以先停掉占用该端口的程序。",
        )
        .with_raw(raw)
        .retryable(true)
    } else {
        AppError::new(
            crate::error::INSTANCE_EXITED_IMMEDIATELY,
            format!("启动失败：DSH 启动后立即退出（退出码 {exit_code}）"),
            "检查启动参数与运行配置档是否正确；下方的最后若干行输出可用于定位原因。",
        )
        .with_raw(raw)
        .retryable(true)
    }
}

/// 一个实例的运行期记录。登记表只放**本工具启动的**实例。
pub struct InstanceEntry {
    pub instance_id: String,
    pub pid: u32,
    pub started_at_ms: i64,
    pub log_path: PathBuf,
    pub buffer: Arc<Mutex<OutputBuffer>>,
    state: Mutex<InstanceState>,
    /// 两个管道读者共用的行锁：seq、时间戳、日志与事件必须同序（修复 2）。
    line_lock: Mutex<()>,
    /// 本实例的地址提取器：两个读者共用同一份 seen 集合，同一地址只上报一次（§5.3）。
    extractor: Mutex<crate::m3::url_extract::UrlExtractor>,
    /// 受管登记表的共享句柄：进程退出/停止时自动摘除，供实例枚举按祖先链匹配（修复 1）。
    managed: Arc<Mutex<ManagedRegistry>>,
}

impl InstanceEntry {
    pub fn state(&self) -> InstanceState {
        *self.state.lock().expect("状态锁被毒化")
    }

    pub fn set_state(&self, state: InstanceState) {
        *self.state.lock().expect("状态锁被毒化") = state;
        // stopped/failed 之后枚举不得再把残留的 web 进程认成本工具实例。
        if matches!(state, InstanceState::Stopped | InstanceState::Failed) {
            self.managed
                .lock()
                .expect("受管登记锁被毒化")
                .unregister(self.pid, &self.instance_id);
        }
    }

    /// 一行输出的整条管线：取号、落盘与推送都在同一把行锁内完成。
    fn record_line(
        &self,
        emitter: &dyn InstanceEmitter,
        log: &Mutex<crate::m0::logging::LogWriter>,
        text: String,
        is_stderr: bool,
    ) {
        let _line = self.line_lock.lock().expect("行管线锁被毒化");
        // 时间戳必须在锁内采样，才能与 seq 同序（stdout/stderr 两个读者并发）。
        let timestamp_ms = crate::m3::now_ms();
        let seq = self.buffer.lock().expect("输出缓冲锁被毒化").push(
            timestamp_ms,
            text.clone(),
            is_stderr,
        );
        if let Ok(mut writer) = log.lock() {
            // LogWriter::append_line 只收一个字符串：时间戳由我们拼进去（§0.4）。
            let _ = writer.append_line(&format!("[{timestamp_ms}] {text}"));
        }
        // 地址在读流这一刻识别：与本行输出共用同一把行锁，instance:url 先于本行 instance:output（§5.3）。
        // 两个读者共用本实例的提取器，同一地址出现在 stdout/stderr 上也只上报一次。
        let found = self
            .extractor
            .lock()
            .expect("地址提取器锁被毒化")
            .feed(&text);
        if let Some(url) = found {
            emitter.emit_url(InstanceUrlPayload {
                instance_id: self.instance_id.clone(),
                url,
            });
        }
        emitter.emit_output(InstanceOutputPayload {
            instance_id: self.instance_id.clone(),
            seq,
            timestamp_ms,
            text,
            is_stderr,
        });
    }
}

/// 本工具已启动实例的登记记录。枚举看到的是 cmd.exe 外壳下的 node 进程，
/// 所以匹配要按祖先链，而不是 spawn 出来的那个 pid（修复 1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedInstance {
    pub pid: u32,
    pub instance_id: String,
    pub started_at_ms: i64,
}

/// 受管实例登记表。`ProcessLister` 与启动前的「已在运行」检查共用它；
/// 它不反向持有 `InstanceRunner`，因此两者之间没有 `Arc` 环。
#[derive(Debug, Default)]
pub struct ManagedRegistry {
    instances: Vec<ManagedInstance>,
}

impl ManagedRegistry {
    /// 登记一次成功 spawn。crate 内可见，便于 lister 的测试直接喂数据。
    pub(crate) fn register(&mut self, instance: ManagedInstance) {
        self.unregister(instance.pid, &instance.instance_id);
        self.instances.push(instance);
    }

    fn unregister(&mut self, pid: u32, instance_id: &str) {
        self.instances
            .retain(|i| i.pid != pid || i.instance_id != instance_id);
    }

    pub fn snapshot(&self) -> Vec<ManagedInstance> {
        self.instances.clone()
    }
}

/// 本工具启动过的实例登记表 + M3 的事件出口。
pub struct InstanceRunner {
    emitter: Arc<dyn InstanceEmitter>,
    entries: Mutex<std::collections::HashMap<String, Arc<InstanceEntry>>>,
    managed: Arc<Mutex<ManagedRegistry>>,
}

impl InstanceRunner {
    /// 装配点传入持有 `AppHandle` 的真实出口；测试传入记录型出口。
    pub fn new(emitter: Arc<dyn InstanceEmitter>) -> Self {
        Self {
            emitter,
            entries: Mutex::new(std::collections::HashMap::new()),
            managed: Arc::new(Mutex::new(ManagedRegistry::default())),
        }
    }

    /// 受管登记表的共享句柄：装配点把它交给 `ProcessLister`，让实例枚举能识别本工具实例。
    pub fn managed_registry(&self) -> Arc<Mutex<ManagedRegistry>> {
        self.managed.clone()
    }

    /// 事件出口。两个读流任务与监督任务都用它推送。
    pub fn emitter(&self) -> Arc<dyn InstanceEmitter> {
        self.emitter.clone()
    }

    pub fn get(&self, instance_id: &str) -> Option<Arc<InstanceEntry>> {
        self.entries
            .lock()
            .expect("登记表锁被毒化")
            .get(instance_id)
            .cloned()
    }

    pub fn insert(&self, entry: Arc<InstanceEntry>) {
        // 只有 start 成功后会走到这里：登记项即「本工具启动的实例」。
        if matches!(
            entry.state(),
            InstanceState::Starting | InstanceState::Running | InstanceState::Stopping
        ) {
            self.managed
                .lock()
                .expect("受管登记锁被毒化")
                .register(ManagedInstance {
                    pid: entry.pid,
                    instance_id: entry.instance_id.clone(),
                    started_at_ms: entry.started_at_ms,
                });
        }
        self.entries
            .lock()
            .expect("登记表锁被毒化")
            .insert(entry.instance_id.clone(), entry);
    }

    /// 仍在运行（starting / running / stopping）的实例标识，按字典序。
    pub fn running_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .entries
            .lock()
            .expect("登记表锁被毒化")
            .values()
            .filter(|e| {
                matches!(
                    e.state(),
                    InstanceState::Starting | InstanceState::Running | InstanceState::Stopping
                )
            })
            .map(|e| e.instance_id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// 供实例枚举标注 `managed` 用的登记快照（pid、实例标识、启动时间）。
    pub fn managed_pids(&self) -> Vec<ManagedInstance> {
        self.managed.lock().expect("受管登记锁被毒化").snapshot()
    }
}

/// 一条管道一个读循环：**来一行推一行**，绝不缓冲到进程结束（FR-3.2）。
///
/// `runner` 只用来拿事件出口；`log` 与 `entry` 都来自同一次启动。
async fn read_stream<R>(
    reader: R,
    is_stderr: bool,
    entry: Arc<InstanceEntry>,
    runner: Arc<InstanceRunner>,
    log: Arc<Mutex<crate::m0::logging::LogWriter>>,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt as _;

    let emitter = runner.emitter();
    let mut lines = tokio::io::BufReader::new(reader).lines();
    // 管道在进程退出时断裂是正常现象，不是错误：循环自然结束。
    while let Ok(Some(text)) = lines.next_line().await {
        entry.record_line(emitter.as_ref(), log.as_ref(), text, is_stderr);
    }
}

fn state_payload(entry: &InstanceEntry, state: InstanceState) -> InstanceStatePayload {
    InstanceStatePayload {
        instance_id: entry.instance_id.clone(),
        state,
        started_at_ms: Some(entry.started_at_ms),
        uptime_ms: Some((crate::m3::now_ms() - entry.started_at_ms).max(0)),
    }
}

/// 启动一个本工具实例，成功后立刻返回实例标识（命令 7 的出参）。
///
/// 取 `&Arc<InstanceRunner>` 而不是 `&InstanceRunner`：两个读流任务与监督任务都是 `'static`，
/// 需要各自持有一份 runner 来拿事件出口。
pub async fn start(
    runner: &Arc<InstanceRunner>,
    probe: &dyn crate::ports::process::ProcessProbe,
    spec: StartSpec,
) -> CmdResult<String> {
    // 启动前**先枚举**：已有实例在运行就拒绝，绝不静默另起（FR-4.2）。
    let existing = crate::m3::instance_discovery::discover(
        probe,
        &runner.managed_pids(),
        crate::m3::now_ms(),
    )?;
    if let Some(first) = existing.first() {
        let owner = if first.managed {
            "由本工具启动"
        } else {
            "由用户自行启动"
        };
        return Err(AppError::new(
            crate::error::INSTANCE_ALREADY_RUNNING,
            format!("已有 DSH Web 实例在运行（pid {}，{owner}）", first.pid),
            "请先停止该实例，再点击启动；本工具不会静默再起一个。",
        ));
    }

    let started_at_ms = crate::m3::now_ms();

    let log = Arc::new(Mutex::new(crate::m0::logging::LogWriter::open(
        spec.log_path.clone(),
        spec.log_max_bytes,
        spec.log_keep_files,
    )?));
    let buffer = Arc::new(Mutex::new(OutputBuffer::new(spec.buffer_lines)));

    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args);
    if let Some(raw) = &spec.raw_arg {
        cmd.raw_arg(raw.clone());
    }
    cmd.current_dir(&spec.work_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 独立进程组：停止时才能整组终止（FR-3.4）；不弹控制台窗口。
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);

    let mut child = cmd.spawn().map_err(|e| {
        AppError::new(
            crate::error::INSTANCE_SPAWN_FAILED,
            format!("无法启动 DSH：{}", spec.program.display()),
            "先在本机命令行执行 dsh --version 确认可用；若提示找不到命令，请到「版本与更新」页安装后再试。",
        )
        .with_raw(e.to_string())
    })?;

    let pid = child.id().ok_or_else(|| {
        AppError::new(
            crate::error::INSTANCE_SPAWN_FAILED,
            "进程已创建但拿不到进程标识",
            "请重试；若反复出现，请在「设置」页导出诊断包反馈。",
        )
    })?;

    let stdout = child.stdout.take().expect("stdout 已设为 piped");
    let stderr = child.stderr.take().expect("stderr 已设为 piped");

    let entry = Arc::new(InstanceEntry {
        instance_id: spec.instance_id.clone(),
        pid,
        started_at_ms,
        log_path: spec.log_path.clone(),
        buffer,
        state: Mutex::new(InstanceState::Starting),
        line_lock: Mutex::new(()),
        extractor: Mutex::new(crate::m3::url_extract::UrlExtractor::new()),
        managed: runner.managed_registry(),
    });
    runner.insert(entry.clone());

    let stdout_task = tokio::spawn(read_stream(
        stdout,
        false,
        entry.clone(),
        runner.clone(),
        log.clone(),
    ));
    let stderr_task = tokio::spawn(read_stream(
        stderr,
        true,
        entry.clone(),
        runner.clone(),
        log.clone(),
    ));

    runner
        .emitter()
        .emit_state(state_payload(&entry, InstanceState::Starting));

    // 启动宽限窗口：窗口内退出即判为启动失败（FR-3.6），不留下一个「看起来在跑」的实例。
    let early_exit = match tokio::time::timeout(STARTUP_GRACE, child.wait()).await {
        Ok(Ok(status)) => Some(status.code().unwrap_or(-1)),
        Ok(Err(_)) => Some(-1),
        // 仍在运行：正常路径。
        Err(_) => None,
    };

    if let Some(code) = early_exit {
        drain_streams(stdout_task, stderr_task, &entry).await;

        // 最后若干行输出就是失败呈现的上下文（FR-3.6）；读完管道再取，最后一行才不会漏。
        let lines: Vec<String> = entry
            .buffer
            .lock()
            .expect("输出缓冲锁被毒化")
            .snapshot()
            .into_iter()
            .map(|l| l.text)
            .collect();
        entry.set_state(InstanceState::Failed);
        runner
            .emitter()
            .emit_state(state_payload(&entry, InstanceState::Failed));
        return Err(classify_start_failure(&lines, code));
    }

    entry.set_state(InstanceState::Running);
    runner
        .emitter()
        .emit_state(state_payload(&entry, InstanceState::Running));

    // 监督任务：进程退出后先等两个读者读到 EOF，再推 stopped，否则最后几行会在 stopped 之后才到。
    // **登记项保留**，这样停止之后仍能读输出与导出日志；受管登记则由 set_state 摘除。
    let supervised = entry.clone();
    let emitter = runner.emitter();
    tokio::spawn(async move {
        let _ = child.wait().await;
        drain_streams(stdout_task, stderr_task, &supervised).await;
        supervised.set_state(InstanceState::Stopped);
        emitter.emit_state(state_payload(&supervised, InstanceState::Stopped));
    });

    Ok(entry.instance_id.clone())
}

/// 等两个管道读者读到 EOF（上限 OUTPUT_DRAIN_TIMEOUT）。超时只告警：
/// 卡死的子进程不能把实例状态永远留在 running（修复 3）。
async fn drain_streams(
    stdout_task: tokio::task::JoinHandle<()>,
    stderr_task: tokio::task::JoinHandle<()>,
    entry: &InstanceEntry,
) {
    let drain = async {
        let _ = stdout_task.await;
        let _ = stderr_task.await;
    };
    if tokio::time::timeout(OUTPUT_DRAIN_TIMEOUT, drain)
        .await
        .is_err()
    {
        log::warn!(
            "实例 {} 的输出管道在 {:?} 内未读到 EOF；仍宣布状态落幕",
            entry.instance_id,
            OUTPUT_DRAIN_TIMEOUT
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::{Path, PathBuf};

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dsh-manager-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// dsh 的 .cmd 启动器：经 cmd.exe 包裹，并且**补上 web 子命令**。
    #[test]
    fn dsh_cmd_launcher_is_wrapped_through_cmd_exe_with_web() {
        let spec = build_start_spec(
            Path::new(r"C:\Users\me\AppData\Roaming\npm\dsh.cmd"),
            &["--no-open".to_string()],
            Path::new(r"C:\Users\me"),
            "inst-1",
            5000,
            Path::new(r"C:\tmp\instance-inst-1.log"),
            5 * 1024 * 1024,
            3,
        );
        assert_eq!(spec.program, PathBuf::from("cmd.exe"));
        assert_eq!(spec.args, vec!["/C".to_string()]);
        assert_eq!(
            spec.raw_arg.as_deref(),
            Some(r#""C:\Users\me\AppData\Roaming\npm\dsh.cmd" web --no-open"#),
            "内层命令行必须整体作为一个 raw 参数，避免 std 的引号转义与 cmd.exe 打架"
        );
    }

    /// dsh 的 .exe 启动器：直接执行，同样补 web。
    #[test]
    fn dsh_exe_launcher_is_used_directly_with_web() {
        let spec = build_start_spec(
            Path::new(r"C:\Program Files\DSH\dsh.exe"),
            &["--port".to_string(), "0".to_string()],
            Path::new(r"C:\Users\me"),
            "inst-2",
            10,
            Path::new(r"C:\tmp\x.log"),
            1024,
            1,
        );
        assert_eq!(spec.program, PathBuf::from(r"C:\Program Files\DSH\dsh.exe"));
        assert_eq!(
            spec.args,
            vec!["web", "--port", "0"],
            "web 必须排在用户参数之前"
        );
        assert!(spec.raw_arg.is_none());
    }

    /// **非 dsh 启动器按原样传参，绝不前置 web。**
    /// 集成测试用 node 当启动器；若这里给它塞一个 web，node 会把 web 当脚本文件加载，
    /// 得到 Cannot find module .../web 而一行脚本都不跑——三条集成测试就全是假失败。
    #[test]
    fn non_dsh_launcher_is_passed_through_verbatim() {
        let spec = build_start_spec(
            Path::new("node"),
            &["-e".to_string(), "console.log(1)".to_string()],
            Path::new(r"C:\Users\me"),
            "inst-3",
            10,
            Path::new(r"C:\tmp\y.log"),
            1024,
            1,
        );
        assert_eq!(spec.program, PathBuf::from("node"));
        assert_eq!(
            spec.args,
            vec!["-e", "console.log(1)"],
            "非 dsh 启动器不得被插入 web"
        );
        assert!(spec.raw_arg.is_none());
    }

    /// FR-3.1：设置里的默认启动参数（D-B 裁定为 ["--no-open"]）必须先于本次请求的参数。
    #[test]
    fn launch_args_from_settings_come_first() {
        let merged = merge_launch_args(
            &["--no-open".to_string()],
            &["--port".to_string(), "3099".to_string()],
        );
        assert_eq!(merged, vec!["--no-open", "--port", "3099"]);
        assert_eq!(merge_launch_args(&[], &[]), Vec::<String>::new());
    }

    use std::time::{Duration, Instant};

    use crate::adapters::fake::fake_probe::FakeProbe;

    /// 记录型事件出口：把三次 emit 都存下来供断言，不依赖 Tauri、不依赖 channel。
    #[derive(Default)]
    struct RecordingEmitter {
        state: Mutex<Vec<InstanceStatePayload>>,
        output: Mutex<Vec<InstanceOutputPayload>>,
        url: Mutex<Vec<InstanceUrlPayload>>,
    }

    impl RecordingEmitter {
        fn outputs(&self) -> Vec<InstanceOutputPayload> {
            self.output.lock().unwrap().clone()
        }

        fn states(&self) -> Vec<InstanceStatePayload> {
            self.state.lock().unwrap().clone()
        }

        fn urls(&self) -> Vec<InstanceUrlPayload> {
            self.url.lock().unwrap().clone()
        }
    }

    impl InstanceEmitter for RecordingEmitter {
        fn emit_state(&self, payload: InstanceStatePayload) {
            self.state.lock().unwrap().push(payload);
        }

        fn emit_output(&self, payload: InstanceOutputPayload) {
            self.output.lock().unwrap().push(payload);
        }

        fn emit_url(&self, payload: InstanceUrlPayload) {
            self.url.lock().unwrap().push(payload);
        }
    }

    /// 造一对「runner + 可读的记录型出口」。
    fn recording_runner() -> (Arc<InstanceRunner>, Arc<RecordingEmitter>) {
        let emitter = Arc::new(RecordingEmitter::default());
        (Arc::new(InstanceRunner::new(emitter.clone())), emitter)
    }

    /// 这些用例要真跑 node，**默认忽略**，在验收步骤里用 --ignored 显式运行。
    /// 理由：§1.5 的验证命令每个任务都要跑，不能让干净环境因为没装 node 而变红。
    fn node_available() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// FR-3.2：脚本 1500 毫秒后才退出，第一行输出必须在 1000 毫秒内到达。
    /// 如果实现把输出缓冲到进程结束，这条用例会以「第一行输出到得太晚」失败。
    /// 需要本机 node，因此默认忽略（见 node_available 上方的说明）。
    #[tokio::test]
    #[ignore = "需要本机 PATH 中有 node：cargo test -- --ignored 显式运行"]
    async fn output_is_streamed_line_by_line_not_buffered() {
        assert!(node_available(), "本测试需要本机 PATH 中有 node");
        let dir = temp_dir("stream");
        let (runner, emitter) = recording_runner();
        let probe = FakeProbe::new(Vec::new());

        let spec = build_start_spec(
            Path::new("node"),
            &[
                "-e".to_string(),
                "console.log('first'); console.error('oops'); setTimeout(() => { console.log('second'); }, 1500);"
                    .to_string(),
            ],
            &dir,
            "inst-stream",
            100,
            &dir.join("instance.log"),
            1024 * 1024,
            1,
        );

        // 非 dsh 启动器 → 命令行就是 node -e "<脚本>"，不会被插入 web。
        let id = start(&runner, &probe, spec).await.expect("spawn 应当成功");

        // T-I5 的宽限窗口让 start 在 800ms 后才返回，而脚本 1500ms 后才退出：
        // 此刻若已有输出，就证明输出没有被缓冲到进程结束（FR-3.2）。
        assert!(
            !emitter.outputs().is_empty(),
            "start 返回时脚本仍在运行；若输出被缓冲到这里会一行都没有"
        );
        assert_eq!(id, "inst-stream");

        // 轮询记录型出口：既拿到两行，也拿到第一行到达的时刻。
        let began = Instant::now();
        let mut first_output_at = None;
        let mut outputs: Vec<InstanceOutputPayload>;
        loop {
            outputs = emitter.outputs();
            if !outputs.is_empty() && first_output_at.is_none() {
                first_output_at = Some(began.elapsed());
            }
            if outputs.len() >= 2 {
                break;
            }
            assert!(
                began.elapsed() < Duration::from_secs(5),
                "5 秒内应收齐两行输出：{outputs:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let at = first_output_at.expect("必须收到输出事件");
        assert!(
            at < Duration::from_millis(1000),
            "第一行输出在 {at:?} 才到达；脚本 1500 毫秒后才退出，说明输出被缓冲了（违反 FR-3.2）"
        );
        // 两个管道各有一个读者，到达顺序不保证；按内容与 is_stderr 断言，不假设 stdout 先到。
        let first = outputs
            .iter()
            .find(|o| o.text == "first")
            .expect("必须收到 stdout 的 first");
        let oops = outputs
            .iter()
            .find(|o| o.text == "oops")
            .expect("必须收到 stderr 的 oops");
        assert!(!first.is_stderr);
        assert!(oops.is_stderr, "stderr 必须被标记（FR-3.2 要求含错误信息）");
        assert_eq!(outputs[0].seq, 0, "序号从 0 开始");
        assert_eq!(outputs[1].seq, 1, "序号必须连续");
        assert!(
            outputs[0].timestamp_ms <= outputs[1].timestamp_ms,
            "事件顺序必须与时间戳同序（修复 2）"
        );
        assert_eq!(first.instance_id, "inst-stream");

        let entry = runner.get("inst-stream").expect("实例必须在登记表里");
        let (lines, next, truncated) = entry.buffer.lock().unwrap().page(0);
        assert_eq!(lines.len(), 2, "两行都要进环形缓冲");
        assert_eq!(next, 2);
        assert!(!truncated);

        let log = std::fs::read_to_string(dir.join("instance.log")).unwrap();
        assert!(log.contains("first"), "输出必须同时落盘：{log:?}");
    }

    /// 等子进程跑完输出窗口后，取记录型出口里收到的全部地址（保留重复，交给断言判定）。
    async fn collect_urls_until_exit(emitter: &Arc<RecordingEmitter>, millis: u64) -> Vec<String> {
        tokio::time::sleep(Duration::from_millis(millis)).await;
        emitter.urls().into_iter().map(|p| p.url).collect()
    }

    /// E2E-4：地址必须从**实时输出**里识别出来，并且同一地址只推一次。
    /// 需要本机 node，因此默认忽略。
    #[tokio::test]
    #[ignore = "需要本机 PATH 中有 node：cargo test -- --ignored 显式运行"]
    async fn url_event_is_emitted_once_from_live_output() {
        assert!(node_available(), "本测试需要本机 PATH 中有 node");
        let dir = temp_dir("url-event");
        let (runner, emitter) = recording_runner();
        let probe = FakeProbe::new(Vec::new());

        let spec = build_start_spec(
            Path::new("node"),
            &[
                "-e".to_string(),
                "console.log('  Web UI: http://127.0.0.1:3080'); console.log('  Web UI: http://127.0.0.1:3080'); setTimeout(() => {}, 2000);"
                    .to_string(),
            ],
            &dir,
            "inst-url",
            100,
            &dir.join("u.log"),
            1024 * 1024,
            1,
        );

        start(&runner, &probe, spec).await.expect("spawn 应当成功");

        let urls = collect_urls_until_exit(&emitter, 1500).await;
        assert_eq!(
            urls,
            vec!["http://127.0.0.1:3080".to_string()],
            "同一地址只上报一次"
        );
    }

    /// 同一地址同时写在 stdout 与 stderr 上，也必须只上报一次。
    /// 这条用例钉死「两个流共用**同一个**提取器」——各建一个就会报两次（违反 §5.3）。
    /// 需要本机 node，因此默认忽略。
    #[tokio::test]
    #[ignore = "需要本机 PATH 中有 node：cargo test -- --ignored 显式运行"]
    async fn url_on_both_streams_is_still_reported_once() {
        assert!(node_available(), "本测试需要本机 PATH 中有 node");
        let dir = temp_dir("url-event-both-streams");
        let (runner, emitter) = recording_runner();
        let probe = FakeProbe::new(Vec::new());

        let spec = build_start_spec(
            Path::new("node"),
            &[
                "-e".to_string(),
                "console.log('http://127.0.0.1:3080'); console.error('http://127.0.0.1:3080'); setTimeout(() => {}, 2000);"
                    .to_string(),
            ],
            &dir,
            "inst-url2",
            100,
            &dir.join("u2.log"),
            1024 * 1024,
            1,
        );

        start(&runner, &probe, spec).await.expect("spawn 应当成功");

        let urls = collect_urls_until_exit(&emitter, 1500).await;
        assert_eq!(urls.len(), 1, "两条流上的同一个地址只能上报一次：{urls:?}");
    }

    /// 用真的 .cmd 桩证明「cmd.exe /C 包裹」这条路径通，并且两条管道并发时行管线仍同序。
    /// 桩名不是 dsh，所以不会被插入 web；它自己会睡 2 秒，活过 T-I5 的启动宽限窗口。
    #[tokio::test]
    async fn real_cmd_stub_is_spawned_through_cmd_exe() {
        let dir = temp_dir("runner-cmd-stub");
        let stub = dir.join("stub.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\nfor /L %%i in (1,1,10) do (\r\n  echo out-%%i\r\n  echo err-%%i>&2\r\n)\r\nping -n 3 127.0.0.1 > nul\r\n",
        )
        .unwrap();

        let (runner, emitter) = recording_runner();
        let probe = FakeProbe::new(Vec::new());
        let spec = build_start_spec(
            &stub,
            &[],
            &dir,
            "inst-stub",
            10,
            &dir.join("s.log"),
            1024,
            1,
        );

        start(&runner, &probe, spec)
            .await
            .expect("必须能通过 cmd.exe 启动 .cmd");

        // 登记表已在 spawn 成功后写入（修复 1），此时进程还活着（桩还要 ping 2 秒）。
        let managed = runner.managed_pids();
        assert!(
            managed
                .iter()
                .any(|m| m.instance_id == "inst-stub" && m.pid != 0),
            "spawn 成功后必须登记受管实例：{managed:?}"
        );

        let mut outputs = Vec::new();
        let began = Instant::now();
        while outputs.len() < 20 {
            outputs = emitter.outputs();
            assert!(
                began.elapsed() < Duration::from_secs(5),
                "必须收到 20 行输出：{outputs:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // 两个管道到达顺序不保证（各有一个读者），只要求集合完整且 seq/时间戳同序。
        let mut actual: Vec<String> = outputs.iter().map(|o| o.text.clone()).collect();
        actual.sort_unstable();
        let mut expected: Vec<String> = (1..=10)
            .flat_map(|n| [format!("out-{n}"), format!("err-{n}")])
            .collect();
        expected.sort_unstable();
        assert_eq!(actual, expected, "20 行输出必须全部送达");
        for (index, output) in outputs.iter().enumerate() {
            assert_eq!(output.seq, index as u64, "20 行必须按序号顺序送达");
            if index > 0 {
                assert!(output.timestamp_ms >= outputs[index - 1].timestamp_ms);
            }
        }
    }

    /// 造一条状态为 running 的登记项，用于不 spawn 的测试。
    fn test_entry(
        managed: Arc<Mutex<ManagedRegistry>>,
        instance_id: &str,
        pid: u32,
        dir: &Path,
    ) -> Arc<InstanceEntry> {
        Arc::new(InstanceEntry {
            instance_id: instance_id.to_string(),
            pid,
            started_at_ms: 1_700_000_000_000,
            log_path: dir.join("entry.log"),
            buffer: Arc::new(Mutex::new(OutputBuffer::new(1000))),
            state: Mutex::new(InstanceState::Running),
            line_lock: Mutex::new(()),
            extractor: Mutex::new(crate::m3::url_extract::UrlExtractor::new()),
            managed,
        })
    }

    /// 修复 2：stdout/stderr 两个读者并发时，seq、时间戳、落盘与事件必须同序。
    #[test]
    fn concurrent_pipes_are_serialized_into_one_order() {
        let dir = temp_dir("atomic-lines");
        let log = Arc::new(Mutex::new(
            crate::m0::logging::LogWriter::open(dir.join("atomic.log"), 1 << 20, 1).unwrap(),
        ));
        let (runner, emitter) = recording_runner();
        let entry = test_entry(runner.managed_registry(), "inst-atomic", 1, &dir);

        let mut handles = Vec::new();
        for (is_stderr, tag) in [(false, "out"), (true, "err")] {
            let entry = entry.clone();
            let emitter = emitter.clone();
            let log = log.clone();
            handles.push(std::thread::spawn(move || {
                for n in 0..100 {
                    entry.record_line(
                        emitter.as_ref(),
                        log.as_ref(),
                        format!("{tag}-{n}"),
                        is_stderr,
                    );
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let outputs = emitter.outputs();
        assert_eq!(outputs.len(), 200);
        for (index, output) in outputs.iter().enumerate() {
            assert_eq!(output.seq, index as u64, "事件顺序必须等于序号顺序");
            if index > 0 {
                assert!(
                    output.timestamp_ms >= outputs[index - 1].timestamp_ms,
                    "时间戳必须与序号同序"
                );
            }
        }

        let (lines, next, _) = entry.buffer.lock().unwrap().page(0);
        assert_eq!(next, 200);
        assert_eq!(lines.len(), 200);
        for (line, output) in lines.iter().zip(outputs.iter()) {
            assert_eq!(line.seq, output.seq);
            assert_eq!(line.timestamp_ms, output.timestamp_ms);
            assert_eq!(line.text, output.text);
        }

        let logged: Vec<String> = std::fs::read_to_string(dir.join("atomic.log"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        let expected: Vec<String> = outputs
            .iter()
            .map(|o| format!("[{}] {}", o.timestamp_ms, o.text))
            .collect();
        assert_eq!(logged, expected, "日志顺序必须与事件顺序一致");
    }

    /// 修复 3：stopped 必须在两个管道读者读到 EOF 之后才发出，
    /// 否则 T-I5 用最后若干行输出构造的失败上下文会读早。
    #[tokio::test]
    async fn final_output_arrives_before_the_stopped_state() {
        let dir = temp_dir("drain-before-stopped");
        let (runner, emitter) = recording_runner();
        let probe = FakeProbe::new(Vec::new());
        // 桩要活过 T-I5 的启动宽限窗口，否则会被正确地判成启动失败；ping 约 2 秒。
        let spec = build_start_spec(
            Path::new("cmd.exe"),
            &[
                "/C".to_string(),
                "echo first&echo second&ping -n 3 127.0.0.1 > nul".to_string(),
            ],
            &dir,
            "inst-drain",
            10,
            &dir.join("drain.log"),
            1024,
            1,
        );

        start(&runner, &probe, spec).await.expect("spawn 应当成功");

        let began = Instant::now();
        while !emitter
            .states()
            .iter()
            .any(|s| s.state == InstanceState::Stopped)
        {
            assert!(
                began.elapsed() < Duration::from_secs(5),
                "5 秒内应看到 stopped：{:?}",
                emitter.states()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // 看到 stopped 时，最后一行输出必须已经送达（两个读者在 stopped 之前已 join）。
        let outputs = emitter.outputs();
        let mut texts: Vec<&str> = outputs.iter().map(|o| o.text.as_str()).collect();
        texts.sort_unstable();
        assert_eq!(texts, vec!["first", "second"], "stopped 之前必须收齐输出");
        assert!(
            runner.managed_pids().is_empty(),
            "进程退出后受管登记必须清除：{:?}",
            runner.managed_pids()
        );
    }

    use crate::error::{
        INSTANCE_ALREADY_RUNNING, INSTANCE_FORCED_STOP_REQUIRED, INSTANCE_NOT_MANAGED,
        INSTANCE_STOP_TIMEOUT,
    };
    use crate::ports::process::{ProcInfo, ProcessProbe};

    fn web_proc(pid: u32) -> ProcInfo {
        ProcInfo {
            pid,
            parent_pid: Some(1),
            cmdline: vec![
                r"C:\Software\nodejs\node.exe".to_string(),
                r"C:\Users\me\AppData\Roaming\npm/node_modules/@deepseek-ai/dsh/lib/bin.js"
                    .to_string(),
                "web".to_string(),
            ],
            started_at_epoch_secs: Some(1_700_000_000),
            exe: None,
        }
    }

    /// FR-4.2：已有实例在运行时再次点击启动必须被拒绝，且**不得静默另起**。
    #[tokio::test]
    async fn refuses_to_start_when_an_instance_is_already_running() {
        let dir = temp_dir("already-running");
        let probe = FakeProbe::new(vec![web_proc(4242)]);
        let (runner, _emitter) = recording_runner();
        let spec = build_start_spec(
            Path::new("node"),
            &[],
            &dir,
            "inst-x",
            10,
            &dir.join("x.log"),
            1024,
            1,
        );

        let err = start(&runner, &probe, spec).await.unwrap_err();

        assert_eq!(err.code, INSTANCE_ALREADY_RUNNING);
        assert!(
            err.reason.contains("4242"),
            "文案要指明是哪个实例：{}",
            err.reason
        );
        assert!(
            err.next_step.contains("停止"),
            "下一步要可操作：{}",
            err.next_step
        );
        assert!(runner.running_ids().is_empty(), "拒绝路径不得留下登记");
    }

    /// 修复 1：本工具实例（cmd.exe 外壳 + node.exe 子进程）在预检查里必须被认成受管，
    /// 否则二次启动的拒绝文案会误说是「由用户自行启动」。
    #[tokio::test]
    async fn already_running_error_names_our_own_instance_as_managed() {
        let dir = temp_dir("already-running-managed");
        let (runner, _emitter) = recording_runner();
        runner.insert(test_entry(
            runner.managed_registry(),
            "inst-own",
            7001,
            &dir,
        ));
        let mut child = web_proc(7002);
        child.parent_pid = Some(7001);
        let probe = FakeProbe::new(vec![child]);
        let spec = build_start_spec(
            Path::new("node"),
            &[],
            &dir,
            "inst-x",
            10,
            &dir.join("x.log"),
            1024,
            1,
        );

        let err = start(&runner, &probe, spec).await.unwrap_err();

        assert_eq!(err.code, INSTANCE_ALREADY_RUNNING);
        assert!(
            err.reason.contains("由本工具启动"),
            "外壳的子进程必须被认成本工具实例：{}",
            err.reason
        );
    }

    /// 设计文档 §2.4 边界 2：外部实例拿不到输出，必须如实报错而不是给空内容。
    #[test]
    fn list_output_rejects_instances_this_tool_did_not_start() {
        let (runner, _emitter) = recording_runner();
        let err = list_output(&runner, "pid-4242", 0).unwrap_err();
        assert_eq!(err.code, INSTANCE_NOT_MANAGED);
        assert!(
            err.reason.contains("pid-4242"),
            "原因要指明是哪个实例：{}",
            err.reason
        );
        assert!(
            err.next_step.contains("本工具启动"),
            "下一步要告诉用户怎么办：{}",
            err.next_step
        );
    }

    #[tokio::test]
    async fn taskkill_records_the_gentle_then_forced_order() {
        let kill = TaskKill::recording();
        assert!(kill.calls().is_empty());
        kill.kill_tree(1234, false).await.unwrap();
        kill.kill_tree(1234, true).await.unwrap();
        assert_eq!(
            kill.calls(),
            vec![(1234, false), (1234, true)],
            "强制与否必须能被记录，供断言使用"
        );
    }

    #[test]
    fn taskkill_program_is_the_system_one() {
        let program = taskkill_program();
        assert!(
            program
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with("taskkill.exe")
        );
    }

    fn child_proc(pid: u32, parent: u32) -> ProcInfo {
        ProcInfo {
            pid,
            parent_pid: Some(parent),
            cmdline: vec!["node".to_string(), "worker.js".to_string()],
            started_at_epoch_secs: Some(1_700_000_000),
            exe: None,
        }
    }

    /// 造一条 running 的登记项并登记到 runner（字段齐全，复用既有 test_entry，见 brief 偏差）。
    fn registered(runner: &InstanceRunner, pid: u32, id: &str) -> Arc<InstanceEntry> {
        let entry = test_entry(runner.managed_registry(), id, pid, Path::new(r"."));
        runner.insert(entry.clone());
        entry
    }

    fn stop_req(id: &str, force: bool, confirm_external: bool) -> StopRequest {
        StopRequest {
            instance_id: id.to_string(),
            force,
            confirm_external,
        }
    }

    const SHORT: std::time::Duration = std::time::Duration::from_millis(150);

    /// §3.9 D-3 第二档：外部实例没有更强确认时，**一次终止命令都不许发**。
    #[tokio::test]
    async fn external_instance_without_confirmation_is_refused_without_any_kill() {
        let (runner, _emitter) = recording_runner();
        let probe = FakeProbe::new(vec![web_proc(4242)]);
        let kill = TaskKill::recording();

        let err = stop(
            &runner,
            &probe,
            &kill,
            stop_req("pid-4242", false, false),
            SHORT,
            SHORT,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, INSTANCE_NOT_MANAGED);
        assert!(
            err.reason.contains("不是由本工具启动"),
            "外部实例未确认时必须保留既有拒绝文案：{}",
            err.reason
        );
        assert!(
            err.reason.contains("4242"),
            "文案要指明影响哪个实例：{}",
            err.reason
        );
        assert!(
            kill.calls().is_empty(),
            "未确认的外部实例绝不允许发出终止命令：{:?}",
            kill.calls()
        );
        assert!(probe.snapshot_calls() >= 1, "拒绝前必须已经枚举确认过目标");
        assert!(probe.is_alive(4242), "拒绝路径不得影响外部实例");
    }

    /// 第二档：外部实例确认后，也只用**不带 /F** 的进程树终止。
    #[tokio::test]
    async fn external_instance_with_confirmation_uses_the_gentle_kill() {
        let (runner, _emitter) = recording_runner();
        let probe = FakeProbe::new(vec![web_proc(4242)]).with_alive(Vec::new());
        let kill = TaskKill::recording();

        let result = stop(
            &runner,
            &probe,
            &kill,
            stop_req("pid-4242", false, true),
            SHORT,
            SHORT,
        )
        .await
        .unwrap();

        assert!(result.stopped);
        assert!(result.residual_pids.is_empty());
        assert!(!result.forced);
        assert_eq!(
            kill.calls(),
            vec![(4242, false)],
            "确认外部实例后第一次也必须不带 /F"
        );
        assert!(probe.snapshot_calls() >= 1, "停止后必须再次枚举复核");
    }

    /// 第一档：本工具实例正常停止，第一次不带 /F，停止后复核为空。
    #[tokio::test]
    async fn managed_stop_leaves_no_residual() {
        let (runner, emitter) = recording_runner();
        let entry = registered(&runner, 5000, "inst-s1");
        let probe = FakeProbe::new(Vec::new()).with_alive(Vec::new());
        let kill = TaskKill::recording();

        let result = stop(
            &runner,
            &probe,
            &kill,
            stop_req("inst-s1", false, false),
            SHORT,
            SHORT,
        )
        .await
        .unwrap();

        assert!(result.stopped);
        assert!(result.residual_pids.is_empty(), "E2E-7 要求无残留");
        assert!(!result.forced);
        assert_eq!(
            kill.calls(),
            vec![(5000, false)],
            "先礼后兵：第一次必须不带 /F"
        );
        assert!(probe.snapshot_calls() >= 1, "停止后必须再次枚举复核");
        assert_eq!(
            entry.state(),
            InstanceState::Stopped,
            "停止后界面状态与实际一致（FR-4.3）"
        );
        assert!(
            runner.managed_pids().is_empty(),
            "停止后受管登记必须清除：{:?}",
            runner.managed_pids()
        );
        assert!(
            emitter
                .states()
                .iter()
                .any(|s| s.instance_id == "inst-s1" && s.state == InstanceState::Stopped),
            "停止后必须推送 instance:state stopped：{:?}",
            emitter.states()
        );
    }

    /// 第三档：超时不得自动强制，必须单独确认；第二次才带 /F。
    #[tokio::test]
    async fn timeout_requires_a_separate_forced_confirmation() {
        let (runner, _emitter) = recording_runner();
        registered(&runner, 6000, "inst-s2");
        let kill = TaskKill::recording();

        let stuck = FakeProbe::new(vec![web_proc(6000)]).with_alive(vec![6000]);
        let err = stop(
            &runner,
            &stuck,
            &kill,
            stop_req("inst-s2", false, false),
            SHORT,
            SHORT,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code, INSTANCE_FORCED_STOP_REQUIRED,
            "超时不得自动强制（FR-4.5）"
        );
        assert_eq!(
            kill.calls(),
            vec![(6000, false)],
            "超时后不允许自动补一次 /F"
        );
        assert!(
            err.next_step.contains("强制"),
            "下一步要引导单独确认：{}",
            err.next_step
        );
        assert!(
            err.next_step.contains("丢失"),
            "要提示可能丢失正在进行的任务：{}",
            err.next_step
        );
        assert!(!err.retryable, "强制确认不是「重试」");
        assert!(
            stuck.snapshot_calls() >= 1,
            "超时路径也必须再次枚举复核（不能因为超时就跳过复核）"
        );

        let gone = FakeProbe::new(Vec::new()).with_alive(Vec::new());
        let result = stop(
            &runner,
            &gone,
            &kill,
            stop_req("inst-s2", true, false),
            SHORT,
            SHORT,
        )
        .await
        .unwrap();
        assert!(result.stopped);
        assert!(result.forced);
        assert_eq!(
            kill.calls(),
            vec![(6000, false), (6000, true)],
            "第二次才带 /F"
        );
    }

    /// 复核有残留时如实报告，不谎报成功。
    #[tokio::test]
    async fn residual_children_are_reported() {
        let (runner, _emitter) = recording_runner();
        registered(&runner, 7000, "inst-s3");
        let probe = FakeProbe::new(vec![child_proc(7001, 7000)]).with_alive(vec![7001]);
        let kill = TaskKill::recording();

        let result = stop(
            &runner,
            &probe,
            &kill,
            stop_req("inst-s3", true, false),
            SHORT,
            SHORT,
        )
        .await
        .unwrap();

        assert!(!result.stopped, "有残留时 stopped 必须为 false");
        assert_eq!(
            result.residual_pids,
            vec![7001],
            "残留 pid 要如实填进 residualPids"
        );
    }

    /// 「连复核都做不了」才是 instance.stop_timeout；可观测的残留是正常返回值（§3.9 T-I4 要点 6）。
    #[tokio::test]
    async fn unreachable_recheck_is_reported_as_stop_timeout() {
        let (runner, _emitter) = recording_runner();
        registered(&runner, 8000, "inst-s4");
        let probe = FakeProbe::new(Vec::new())
            .with_alive(Vec::new())
            .failing_snapshot();
        let kill = TaskKill::recording();

        let err = stop(
            &runner,
            &probe,
            &kill,
            stop_req("inst-s4", false, false),
            SHORT,
            SHORT,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, INSTANCE_STOP_TIMEOUT);
        assert!(err.retryable, "instance.stop_timeout 是可重试错误（§3.2）");
        assert_eq!(kill.calls(), vec![(8000, false)]);
    }

    /// 标识既不在登记表、也不在枚举结果里：如实报错，也不发终止命令。
    #[tokio::test]
    async fn unknown_instance_id_is_reported_without_any_kill() {
        let (runner, _emitter) = recording_runner();
        let probe = FakeProbe::new(Vec::new());
        let kill = TaskKill::recording();

        let err = stop(
            &runner,
            &probe,
            &kill,
            stop_req("pid-9999", false, true),
            SHORT,
            SHORT,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, INSTANCE_NOT_MANAGED);
        assert!(!err.retryable, "找不到实例不是可重试错误");
        assert!(
            err.reason.contains("找不到实例 pid-9999（可能已退出）"),
            "原因要如实说明可能已退出：{}",
            err.reason
        );
        assert!(
            err.next_step.contains("刷新运行实例列表后再试"),
            "下一步要可操作：{}",
            err.next_step
        );
        assert!(kill.calls().is_empty());
        assert!(probe.snapshot_calls() >= 1, "必须先枚举确认目标是否存在");
    }

    /// 登记项在停止后保留（供读输出）：状态已落幕的 inst-* 再停一次必须零终止命令。
    /// 旧 pid 可能已被系统复用，taskkill 它会误杀无关进程。
    #[tokio::test]
    async fn stopping_a_stopped_instance_makes_zero_kill_calls() {
        let (runner, _emitter) = recording_runner();
        let entry = registered(&runner, 9001, "inst-done");
        entry.set_state(InstanceState::Stopped);
        // 旧 pid 已被别的进程复用：确实有进程活着，但不是要停的实例。
        let probe = FakeProbe::new(vec![web_proc(9001)]).with_alive(vec![9001]);
        let kill = TaskKill::recording();

        let err = stop(
            &runner,
            &probe,
            &kill,
            stop_req("inst-done", false, false),
            SHORT,
            SHORT,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, INSTANCE_NOT_MANAGED);
        assert!(!err.retryable);
        assert!(
            err.reason.contains("找不到实例 inst-done（可能已退出）"),
            "已停止的实例要如实说找不到：{}",
            err.reason
        );
        assert!(
            kill.calls().is_empty(),
            "已停止的实例绝不允许再次发出终止命令：{:?}",
            kill.calls()
        );
        assert!(probe.is_alive(9001), "拒绝路径不得影响复用了旧 pid 的进程");
    }

    /// 进程活着但不是 DSH Web：文案不得谎称「已不在运行」，且零终止命令。
    #[tokio::test]
    async fn live_pid_that_is_not_a_dsh_web_instance_is_reported_truthfully() {
        let (runner, _emitter) = recording_runner();
        let mut not_dsh = web_proc(9300);
        not_dsh.cmdline = vec![r"C:\Windows\System32\notepad.exe".to_string()];
        let probe = FakeProbe::new(vec![not_dsh]);
        let kill = TaskKill::recording();

        let err = stop(
            &runner,
            &probe,
            &kill,
            stop_req("pid-9300", false, true),
            SHORT,
            SHORT,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, INSTANCE_NOT_MANAGED);
        assert!(!err.retryable);
        assert!(
            err.reason.contains("不是可识别的 DSH Web 实例"),
            "要说明它不是可识别的 DSH Web 进程：{}",
            err.reason
        );
        assert!(
            !err.reason.contains("已不在运行"),
            "进程还活着，不得说成没有运行：{}",
            err.reason
        );
        assert!(kill.calls().is_empty(), "非 DSH 进程一次终止命令都不许发");
    }

    /// 终止命令无法启动时也必须先复核：树没了算成功，树还在才要求强制确认，复核失败才是 stop_timeout。
    #[tokio::test]
    async fn taskkill_spawn_failure_is_decided_by_the_recheck() {
        let (runner, _emitter) = recording_runner();
        registered(&runner, 9002, "inst-s6");
        let kill = TaskKill::spawn_failing();

        // 树还活着：不是 stop_timeout，而是要求单独确认强制终止。
        let alive = FakeProbe::new(vec![web_proc(9002)]).with_alive(vec![9002]);
        let err = stop(
            &runner,
            &alive,
            &kill,
            stop_req("inst-s6", false, false),
            SHORT,
            SHORT,
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.code, INSTANCE_FORCED_STOP_REQUIRED,
            "taskkill 起不来但树还在：结论应是要求强制确认"
        );
        assert!(
            alive.snapshot_calls() >= 1,
            "taskkill 起不来也必须做停止后复核"
        );

        // 复核本身失败：这才是 stop_timeout（可重试）。
        let unreachable = FakeProbe::new(Vec::new())
            .with_alive(Vec::new())
            .failing_snapshot();
        let err = stop(
            &runner,
            &unreachable,
            &kill,
            stop_req("inst-s6", false, false),
            SHORT,
            SHORT,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, INSTANCE_STOP_TIMEOUT);
        assert!(err.retryable, "instance.stop_timeout 是可重试错误（§3.2）");

        // 树已经不在：复核为空即成功，不得因为 taskkill 起不来就报错。
        let gone = FakeProbe::new(Vec::new()).with_alive(Vec::new());
        let result = stop(
            &runner,
            &gone,
            &kill,
            stop_req("inst-s6", false, false),
            SHORT,
            SHORT,
        )
        .await
        .unwrap();
        assert!(result.stopped);
        assert!(result.residual_pids.is_empty());
        assert!(!result.forced);
        assert!(
            gone.snapshot_calls() >= 1,
            "taskkill 起不来也必须做停止后复核"
        );
    }

    // ---- T-I5：启动失败归类（FR-3.6） ----

    #[test]
    fn port_in_use_output_is_classified() {
        let lines = vec![
            "starting dsh web".to_string(),
            "Error: listen EADDRINUSE: address already in use 127.0.0.1:3080".to_string(),
        ];
        let err = classify_start_failure(&lines, 1);
        assert_eq!(err.code, crate::error::INSTANCE_PORT_IN_USE);
        assert!(
            err.raw
                .as_deref()
                .unwrap_or_default()
                .contains("EADDRINUSE"),
            "必须带最后若干行输出作上下文（FR-3.6）"
        );
        assert!(
            err.next_step.contains("--port"),
            "建议要可操作：{}",
            err.next_step
        );
        assert!(err.retryable, "instance.port_in_use 是可重试错误（§3.2）");
    }

    #[test]
    fn any_other_immediate_exit_is_classified_as_exited_immediately() {
        let lines: Vec<String> = (0..30).map(|i| format!("line-{i}")).collect();
        let err = classify_start_failure(&lines, 3);
        assert_eq!(err.code, crate::error::INSTANCE_EXITED_IMMEDIATELY);
        let raw = err.raw.unwrap_or_default();
        assert_eq!(
            raw.lines().count(),
            FAILURE_CONTEXT_LINES,
            "只回带最后若干行，不是全部输出"
        );
        assert!(raw.contains("line-29"), "要保留最新的行");
        assert!(!raw.contains("line-5"), "太早的行不该出现在上下文里");
        assert!(
            err.next_step.contains("运行配置档"),
            "建议要提到运行配置档：{}",
            err.next_step
        );
        assert!(
            err.retryable,
            "instance.exited_immediately 是可重试错误（§3.2）"
        );
    }

    /// FR-3.6：启动后立即退出必须变成一次**失败呈现**，而不是一个「看起来成功」的实例。
    /// 需要本机 node，因此默认忽略（纯函数版本的两个 classify 用例不受影响，照常跑）。
    #[tokio::test]
    #[ignore = "需要本机 PATH 中有 node：cargo test -- --ignored 显式运行"]
    async fn an_immediately_exiting_process_is_reported_as_a_start_failure() {
        assert!(node_available(), "本测试需要本机 PATH 中有 node");
        let dir = temp_dir("early-exit");
        let (runner, _emitter) = recording_runner();
        let probe = FakeProbe::new(Vec::new());

        let spec = build_start_spec(
            Path::new("node"),
            &[
                "-e".to_string(),
                "console.log('boom'); process.exit(7);".to_string(),
            ],
            &dir,
            "inst-early",
            100,
            &dir.join("e.log"),
            1024,
            1,
        );

        let err = start(&runner, &probe, spec).await.unwrap_err();

        assert_eq!(err.code, crate::error::INSTANCE_EXITED_IMMEDIATELY);
        assert!(
            err.raw.as_deref().unwrap_or_default().contains("boom"),
            "错误上下文要包含最后几行输出：{:?}",
            err.raw
        );
        assert_eq!(
            runner.get("inst-early").unwrap().state(),
            InstanceState::Failed,
            "失败实例的状态必须是 failed"
        );
    }

    /// FR-3.6：启动宽限窗口是 800ms（Lead 裁定 1），不是随手一个数字。
    #[test]
    fn startup_grace_window_is_800ms() {
        assert_eq!(STARTUP_GRACE, std::time::Duration::from_millis(800));
    }

    /// 宽限窗口的另一侧：活过窗口的进程必须被正常接受为 running，不能误判成启动失败。
    /// 需要本机 node，因此默认忽略。
    #[tokio::test]
    #[ignore = "需要本机 PATH 中有 node：cargo test -- --ignored 显式运行"]
    async fn process_outliving_the_grace_window_starts_successfully() {
        assert!(node_available(), "本测试需要本机 PATH 中有 node");
        let dir = temp_dir("grace-outside");
        let (runner, _emitter) = recording_runner();
        let probe = FakeProbe::new(Vec::new());
        let spec = build_start_spec(
            Path::new("node"),
            &[
                "-e".to_string(),
                "console.log('alive'); setTimeout(() => {}, 1200);".to_string(),
            ],
            &dir,
            "inst-alive",
            100,
            &dir.join("a.log"),
            1024,
            1,
        );

        let began = Instant::now();
        let id = start(&runner, &probe, spec)
            .await
            .expect("活过宽限窗口的进程必须启动成功");
        assert_eq!(id, "inst-alive");
        assert!(
            began.elapsed() >= STARTUP_GRACE,
            "start 必须等满宽限窗口才放心返回成功：{:?}",
            began.elapsed()
        );
        assert_eq!(
            runner.get("inst-alive").unwrap().state(),
            InstanceState::Running
        );
    }

    /// spawn 本身失败归为 instance.spawn_failed（§3.2、FR-3.6），且不留下运行中登记。
    #[tokio::test]
    async fn missing_program_is_reported_as_spawn_failed() {
        let dir = temp_dir("spawn-failed");
        let (runner, _emitter) = recording_runner();
        let probe = FakeProbe::new(Vec::new());
        let spec = build_start_spec(
            Path::new("dsh-manager-no-such-program"),
            &[],
            &dir,
            "inst-missing",
            10,
            &dir.join("m.log"),
            1024,
            1,
        );

        let err = start(&runner, &probe, spec).await.unwrap_err();

        assert_eq!(err.code, crate::error::INSTANCE_SPAWN_FAILED);
        assert!(!err.retryable, "instance.spawn_failed 不可重试（§3.2）");
        assert!(err.raw.is_some(), "spawn 失败的底层原文必须保留");
        assert!(
            err.next_step.contains("dsh --version"),
            "下一步要可操作：{}",
            err.next_step
        );
        assert!(
            runner.running_ids().is_empty(),
            "spawn 失败不得留下运行中登记"
        );
    }

    /// FR-4.4 前半：stopInstanceOnExit 为真时停本工具实例，为假时一个都不停。
    #[test]
    fn exit_targets_follow_the_setting() {
        let (runner, _emitter) = recording_runner();
        registered(&runner, 8001, "inst-a");
        registered(&runner, 8002, "inst-b");
        runner
            .get("inst-b")
            .unwrap()
            .set_state(InstanceState::Stopped);

        assert_eq!(
            exit_stop_targets(&runner, true),
            vec!["inst-a".to_string()],
            "已停止的不再处理"
        );
        assert!(
            exit_stop_targets(&runner, false).is_empty(),
            "关掉设置就一个都不停"
        );
    }

    /// FR-4.4 后半：外部实例在任何设置下都不被自动终止。
    #[tokio::test]
    async fn external_instances_are_never_stopped_on_exit() {
        let (runner, _emitter) = recording_runner();
        registered(&runner, 8001, "inst-a");
        let probe = FakeProbe::new(vec![web_proc(4242), web_proc(8001)]).with_alive(Vec::new());
        let kill = TaskKill::recording();

        let outcome = try_stop_all_on_exit(&runner, &probe, &kill, true).await;

        assert_eq!(outcome, ExitOutcome::AllStopped(vec!["inst-a".to_string()]));
        let pids: Vec<u32> = kill.calls().into_iter().map(|(pid, _)| pid).collect();
        assert!(
            !pids.contains(&4242),
            "外部实例 4242 绝不允许被自动终止（FR-4.4）：{pids:?}"
        );
    }

    #[tokio::test]
    async fn exit_does_nothing_when_the_setting_is_off() {
        let (runner, _emitter) = recording_runner();
        registered(&runner, 8001, "inst-a");
        let probe = FakeProbe::new(vec![web_proc(8001)]).with_alive(Vec::new());
        let kill = TaskKill::recording();

        assert_eq!(
            try_stop_all_on_exit(&runner, &probe, &kill, false).await,
            ExitOutcome::NothingToDo
        );
        assert!(kill.calls().is_empty(), "关掉设置时连一次终止命令都不许发");
    }

    #[tokio::test]
    async fn exit_does_nothing_when_no_managed_instance_is_running() {
        let (runner, _emitter) = recording_runner();
        let probe = FakeProbe::new(Vec::new()).with_alive(Vec::new());
        let kill = TaskKill::recording();

        assert_eq!(
            try_stop_all_on_exit(&runner, &probe, &kill, true).await,
            ExitOutcome::NothingToDo
        );
        assert!(kill.calls().is_empty());
    }

    /// §3.9「应用退出时的例外」第 1、3 步：先礼；停不下来就把决定权交回给界面，
    /// **未经用户确认，kill.calls() 里绝不能出现 force: true**。
    #[tokio::test]
    async fn exit_asks_the_user_instead_of_forcing() {
        let (runner, _emitter) = recording_runner();
        registered(&runner, 8001, "inst-stuck");
        let probe = FakeProbe::new(vec![web_proc(8001)]).with_alive(vec![8001]);
        let kill = TaskKill::recording();

        let outcome = try_stop_all_on_exit(&runner, &probe, &kill, true).await;

        assert_eq!(
            outcome,
            ExitOutcome::NeedsDecision(vec![UnstoppableInstance {
                instance_id: "inst-stuck".to_string(),
                pid: 8001,
            }]),
            "停不下来时必须把决定权交给用户，而不是自己拿主意"
        );
        assert_eq!(kill.calls(), vec![(8001, false)], "先礼这一步不许带 /F");
        assert!(
            !kill.calls().iter().any(|(_, force)| *force),
            "未经用户确认不得强制终止（FR-4.5 的单独确认）：{:?}",
            kill.calls()
        );
    }

    /// 用户点了「强制终止」之后才允许补 /F。
    #[tokio::test]
    async fn forcing_only_happens_after_the_user_says_so() {
        let (runner, _emitter) = recording_runner();
        registered(&runner, 8001, "inst-stuck");
        let kill = TaskKill::recording();
        let pending = vec![UnstoppableInstance {
            instance_id: "inst-stuck".to_string(),
            pid: 8001,
        }];

        let gone = FakeProbe::new(Vec::new()).with_alive(Vec::new());
        let stopped = force_stop_pending(&runner, &gone, &kill, &pending).await;

        assert_eq!(stopped, vec!["inst-stuck".to_string()]);
        assert_eq!(kill.calls(), vec![(8001, true)], "这一步才允许带 /F");
    }

    /// 三选一确认框的返回值映射。默认按钮是「强制终止」，但**必须由用户点**。
    #[test]
    fn exit_choice_labels_map_to_three_actions() {
        assert_eq!(map_exit_choice(BTN_FORCE), ExitChoice::Force);
        assert_eq!(map_exit_choice(BTN_KEEP), ExitChoice::KeepAndExit);
        assert_eq!(map_exit_choice(BTN_CANCEL), ExitChoice::CancelExit);
        assert_eq!(
            map_exit_choice("用户直接关掉了对话框"),
            ExitChoice::CancelExit
        );
    }

    /// 选「保留运行并退出」时如实记一条，并且**下次启动能读出来**（§3.9 第 4 步）。
    #[test]
    fn skipped_instances_are_remembered_for_the_next_launch() {
        let dir = temp_dir("exit-pending");
        let marker = pending_exit_file(&dir.join("channels.json"));
        let pending = vec![UnstoppableInstance {
            instance_id: "inst-stuck".to_string(),
            pid: 8001,
        }];

        remember_skipped_instances(&marker, &pending).unwrap();
        assert!(marker.is_file(), "选「保留运行并退出」必须留下可读的存档");
        let text = std::fs::read_to_string(&marker).unwrap();
        assert!(
            text.contains("instanceId") && text.contains("8001"),
            "存档要如实带实例标识与 pid：{text}"
        );

        assert_eq!(
            take_startup_notice(&marker).unwrap(),
            pending,
            "下次启动要能读出来提示"
        );
        assert!(!marker.is_file(), "提示过一次就清掉，不反复打扰");
        assert_eq!(take_startup_notice(&marker).unwrap(), Vec::new());
    }

    /// Lead 裁定 3：选「保留运行并退出」要留一条操作记录，而不只是一份存档文件。
    #[test]
    fn keep_running_decision_is_recorded_in_the_oplog() {
        let entry = keep_running_oplog_entry(&[UnstoppableInstance {
            instance_id: "inst-stuck".to_string(),
            pid: 8001,
        }]);
        assert_eq!(entry.action, "instance.keep_running");
        assert_eq!(entry.result, crate::m0::oplog::OplogResult::Ok);
        assert!(
            entry.target.contains("inst-stuck") && entry.target.contains("8001"),
            "操作记录要如实带实例标识与 pid：{}",
            entry.target
        );
    }
}

/// 命令 9 的实现：按 `fromSeq` 取输出分页。
///
/// **外部实例拿不到输出**（设计文档 §2.4 边界 2）：登记表里没有它就返回 `instance.not_managed`，
/// 不用空内容冒充「没有输出」。
pub fn list_output(
    runner: &InstanceRunner,
    instance_id: &str,
    from_seq: u64,
) -> CmdResult<(Vec<crate::m3::output_buffer::OutputLine>, u64, bool)> {
    let entry = runner.get(instance_id).ok_or_else(|| {
        AppError::new(
            crate::error::INSTANCE_NOT_MANAGED,
            format!("实例 {instance_id} 不是由本工具启动，无法获取它的输出"),
            "外部实例的输出无法获取；如果要查看输出，请用本工具启动一个实例。",
        )
    })?;
    Ok(entry
        .buffer
        .lock()
        .expect("输出缓冲锁被毒化")
        .page(from_seq))
}

/// 命令 8 的出参。`residualPids` 非空时 `stopped` 必为 `false`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopResult {
    pub stopped: bool,
    pub residual_pids: Vec<u32>,
    pub forced: bool,
}

/// 停止请求。三档确认强度由 `confirm_external` 与 `force` 表达（§3.9 D-3）。
#[derive(Debug, Clone)]
pub struct StopRequest {
    pub instance_id: String,
    pub force: bool,
    pub confirm_external: bool,
}

/// 非强制停止的等待时限（先礼后兵）。
pub const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// 强制终止后的等待时限。
pub const FORCE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

enum StopTarget {
    Managed { pid: u32 },
    External { pid: u32 },
}

/// 「找不到」的统一定论：标识不存在，或登记项已经落幕（stopped / failed）。
fn not_found(instance_id: &str) -> AppError {
    AppError::new(
        crate::error::INSTANCE_NOT_MANAGED,
        format!("找不到实例 {instance_id}（可能已退出）"),
        "刷新运行实例列表后再试。",
    )
}

/// 只有仍在运行的状态才允许发终止命令：停止后的登记项会保留旧 pid 供读输出，
/// 一次迟到的停止会 taskkill 到被系统复用的无关进程。
fn is_live_state(state: InstanceState) -> bool {
    matches!(
        state,
        InstanceState::Starting | InstanceState::Running | InstanceState::Stopping
    )
}

fn resolve_target(
    runner: &InstanceRunner,
    probe: &dyn crate::ports::process::ProcessProbe,
    instance_id: &str,
) -> CmdResult<StopTarget> {
    if let Some(entry) = runner.get(instance_id) {
        return if is_live_state(entry.state()) {
            Ok(StopTarget::Managed { pid: entry.pid })
        } else {
            // 已停止/失败的登记项不会再发终止命令，如实说找不到这个运行中的实例。
            Err(not_found(instance_id))
        };
    }

    let pid: u32 = instance_id
        .strip_prefix("pid-")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| not_found(instance_id))?;

    let listed = crate::m3::instance_discovery::discover(
        probe,
        &runner.managed_pids(),
        crate::m3::now_ms(),
    )?;
    if listed.iter().any(|i| i.pid == pid && !i.managed) {
        return Ok(StopTarget::External { pid });
    }
    if probe.is_alive(pid) {
        // 进程活着但不在可停止的外部实例列表里：不能说「已不在运行」。
        return Err(AppError::new(
            crate::error::INSTANCE_NOT_MANAGED,
            format!("进程 {pid} 仍存在，但它不是可识别的 DSH Web 实例"),
            "刷新「运行实例」列表后再试；本工具只停止列表里的 DSH Web 实例。",
        ));
    }
    Err(not_found(instance_id))
}

/// `root` 及其全部子孙里**仍存活**的 pid，升序。这就是 E2E-7 的「复核无残留」。
fn live_tree(probe: &dyn crate::ports::process::ProcessProbe, root: u32) -> CmdResult<Vec<u32>> {
    let snapshot = probe.snapshot()?;
    let mut tree = vec![root];
    let mut changed = true;
    while changed {
        changed = false;
        for p in &snapshot {
            if let Some(parent) = p.parent_pid
                && tree.contains(&parent)
                && !tree.contains(&p.pid)
            {
                tree.push(p.pid);
                changed = true;
            }
        }
    }
    let mut alive: Vec<u32> = tree
        .into_iter()
        .filter(|pid| probe.is_alive(*pid))
        .collect();
    alive.sort_unstable();
    Ok(alive)
}

async fn wait_gone(
    probe: &dyn crate::ports::process::ProcessProbe,
    pid: u32,
    wait: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + wait;
    loop {
        if !probe.is_alive(pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL_INTERVAL.min(wait)).await;
    }
}

/// 命令 8 的实现。四步顺序见任务说明，任何一步都不得省略。
pub async fn stop(
    runner: &InstanceRunner,
    probe: &dyn crate::ports::process::ProcessProbe,
    kill: &TaskKill,
    req: StopRequest,
    stop_wait: std::time::Duration,
    force_wait: std::time::Duration,
) -> CmdResult<StopResult> {
    let target = resolve_target(runner, probe, &req.instance_id)?;

    let pid = match target {
        StopTarget::External { pid } => {
            if !req.confirm_external {
                return Err(AppError::new(
                    crate::error::INSTANCE_NOT_MANAGED,
                    format!(
                        "实例 {} 不是由本工具启动（pid {pid}），停止它需要更强确认",
                        req.instance_id
                    ),
                    "该实例可能正在执行任务。确认要停止它时，请勾选「我知道这个实例不是本工具启动的」后再试。",
                ));
            }
            pid
        }
        StopTarget::Managed { pid } => pid,
    };

    // 先礼后兵：force == false 时**不带** /F。
    // taskkill 起不来不等于停止失败：冻结边界是「复核做不了才报 stop_timeout」，
    // 所以命令发不出去也要继续往下按复核结果定论（§3.9 T-I4 要点 6）。
    let kill_failed = kill.kill_tree(pid, req.force).await.is_err();

    // 终止命令没发出去就没有可等待的对象，直接看根进程是否还在。
    let wait = if req.force { force_wait } else { stop_wait };
    let gone = if kill_failed {
        !probe.is_alive(pid)
    } else {
        wait_gone(probe, pid, wait).await
    };

    // 无论哪条路径，停止后**必须**再次枚举复核（E2E-7「无残留」）。
    // 复核本身做不了才是 instance.stop_timeout；可观测的残留是正常返回值（§3.9 T-I4 要点 6）。
    let residual_pids = live_tree(probe, pid).map_err(|e| {
        AppError::new(
            crate::error::INSTANCE_STOP_TIMEOUT,
            format!(
                "实例 {} 已收到停止命令，但复核进程表失败，无法确认是否还有残留",
                req.instance_id
            ),
            "重新点击「停止」再试一次；若反复失败，请在「设置」页导出诊断包反馈。",
        )
        .with_raw(format!("{}：{}", e.code, e.reason))
        .retryable(true)
    })?;

    // taskkill 没启动起来时停止命令根本没发出去，不能复用「等待时限内没有退出」的说法。
    if (kill_failed && !residual_pids.is_empty()) || (!gone && !req.force) {
        let reason = if kill_failed {
            format!(
                "实例 {} 仍在运行（仍有 {} 个进程存活），终止命令未能执行",
                req.instance_id,
                residual_pids.len()
            )
        } else {
            format!(
                "实例 {} 在等待时限内没有退出（仍有 {} 个进程存活）",
                req.instance_id,
                residual_pids.len()
            )
        };
        return Err(AppError::new(
            crate::error::INSTANCE_FORCED_STOP_REQUIRED,
            reason,
            "它可能正在执行任务。确认要强制终止时，请单独确认「强制终止」——该操作可能丢失正在进行的任务。",
        ));
    }

    let stopped = residual_pids.is_empty();
    if stopped && let Some(entry) = runner.get(&req.instance_id) {
        entry.set_state(InstanceState::Stopped);
        runner
            .emitter()
            .emit_state(state_payload(&entry, InstanceState::Stopped));
    }

    Ok(StopResult {
        stopped,
        residual_pids,
        forced: req.force,
    })
}

/// 应用退出时应当停止的实例标识。
///
/// **只包含本工具启动的实例**：外部实例从来不进登记表，所以「外部实例在任何设置下都不自动终止」
/// （FR-4.4）在这里是结构性保证，不需要额外的归属判断。
pub fn exit_stop_targets(runner: &InstanceRunner, stop_instance_on_exit: bool) -> Vec<String> {
    if !stop_instance_on_exit {
        return Vec::new();
    }
    runner.running_ids()
}

/// 退出时的停止时限：短一些，别让用户等太久。
pub const EXIT_STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// 退出流程的走向（§3.9「应用退出时的例外」第 1 ~ 3 步）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitOutcome {
    /// 设置里关掉了「退出时停止实例」，或者本来就没有运行中的本工具实例：直接退出。
    NothingToDo,
    /// 全部停干净了：直接退出。
    AllStopped(Vec<String>),
    /// 有实例没停下来：**必须**交给用户在确认框里三选一，不许自作主张。
    NeedsDecision(Vec<UnstoppableInstance>),
}

/// 一个「先礼之后仍在运行」的实例。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnstoppableInstance {
    pub instance_id: String,
    pub pid: u32,
}

/// 确认框的三个按钮文案。**lib.rs 与断言用的是同一组常量**，避免文案漂移导致映射错位。
pub const BTN_FORCE: &str = "强制终止";
pub const BTN_KEEP: &str = "保留运行并退出";
pub const BTN_CANCEL: &str = "取消退出";

/// 用户在确认框里的选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitChoice {
    Force,
    KeepAndExit,
    CancelExit,
}

/// 把确认框返回的按钮文案映射成三选一。**纯函数**，可离线单测。
///
/// 认不出来的文案一律按「取消退出」处理——退出这种不可逆动作上，保守才是对的。
pub fn map_exit_choice(label: &str) -> ExitChoice {
    match label {
        BTN_FORCE => ExitChoice::Force,
        BTN_KEEP => ExitChoice::KeepAndExit,
        _ => ExitChoice::CancelExit,
    }
}

/// 把未停止实例列成一句给用户看的话（确认框与操作记录共用，避免两处文案漂移）。
pub fn describe_instances(pending: &[UnstoppableInstance]) -> String {
    pending
        .iter()
        .map(|p| format!("{}（PID {}）", p.instance_id, p.pid))
        .collect::<Vec<_>>()
        .join("、")
}

/// 选「保留运行并退出」时如实写进操作记录的那一条（§3.9 第 4 步、Lead 裁定 3）。
pub fn keep_running_oplog_entry(pending: &[UnstoppableInstance]) -> crate::m0::oplog::OplogEntry {
    crate::m0::oplog::OplogEntry::ok("instance.keep_running", &describe_instances(pending))
}

/// 退出流程第 1 步：对每个本工具实例做**不带 /F** 的停止，然后如实汇报走向。
///
/// **本函数永远不发 /F**（§3.9：强制终止必须由用户在确认框里单独确认）。
pub async fn try_stop_all_on_exit(
    runner: &InstanceRunner,
    probe: &dyn crate::ports::process::ProcessProbe,
    kill: &TaskKill,
    stop_instance_on_exit: bool,
) -> ExitOutcome {
    let targets = exit_stop_targets(runner, stop_instance_on_exit);
    if targets.is_empty() {
        return ExitOutcome::NothingToDo;
    }

    let mut stopped = Vec::new();
    let mut pending = Vec::new();
    for id in targets {
        let pid = runner.get(&id).map(|e| e.pid);
        let req = StopRequest {
            instance_id: id.clone(),
            force: false,
            confirm_external: true,
        };
        let ok = matches!(
            stop(runner, probe, kill, req, EXIT_STOP_WAIT, FORCE_WAIT).await,
            Ok(result) if result.stopped
        );
        match (ok, pid) {
            (true, _) => stopped.push(id),
            (false, Some(pid)) => pending.push(UnstoppableInstance {
                instance_id: id,
                pid,
            }),
            (false, None) => {}
        }
    }

    if pending.is_empty() {
        ExitOutcome::AllStopped(stopped)
    } else {
        ExitOutcome::NeedsDecision(pending)
    }
}

/// 退出流程第 3 步之一：用户点了「强制终止」（唯一允许发 /F 的地方）。
pub async fn force_stop_pending(
    runner: &InstanceRunner,
    probe: &dyn crate::ports::process::ProcessProbe,
    kill: &TaskKill,
    pending: &[UnstoppableInstance],
) -> Vec<String> {
    let mut stopped = Vec::new();
    for item in pending {
        let req = StopRequest {
            instance_id: item.instance_id.clone(),
            force: true,
            confirm_external: true,
        };
        if matches!(
            stop(runner, probe, kill, req, EXIT_STOP_WAIT, FORCE_WAIT).await,
            Ok(result) if result.stopped
        ) {
            stopped.push(item.instance_id.clone());
        }
    }
    stopped
}

/// 「下次启动要提示的未停止实例」的存档位置：与通道缓存同目录，可随时删（删了只是不提示）。
pub fn pending_exit_file(cache_file: &Path) -> PathBuf {
    cache_file.with_file_name("exit-pending.json")
}

/// 退出流程第 4 步：用户选了「保留运行并退出」——**如实记一条**，供下次启动提示。
pub fn remember_skipped_instances(marker: &Path, pending: &[UnstoppableInstance]) -> CmdResult<()> {
    let text = serde_json::to_string_pretty(pending).map_err(|e| {
        AppError::new(
            crate::error::PATH_PERMISSION_DENIED,
            "无法序列化未停止实例的记录",
            "请重试；若反复出现，请在「设置」页导出诊断包反馈。",
        )
        .with_raw(e.to_string())
    })?;
    std::fs::write(marker, text).map_err(|e| {
        AppError::new(
            crate::error::PATH_PERMISSION_DENIED,
            format!("无法写出未停止实例的记录：{}", marker.display()),
            "确认该目录可写后重试。",
        )
        .with_raw(e.to_string())
    })
}

/// 下次启动时读出提示并**清空**它（提示过一次就不再打扰）。文件不存在或读不出来一律返回空表。
pub fn take_startup_notice(marker: &Path) -> CmdResult<Vec<UnstoppableInstance>> {
    if !marker.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(marker).map_err(|e| {
        AppError::new(
            crate::error::PATH_PERMISSION_DENIED,
            format!("无法读取未停止实例的记录：{}", marker.display()),
            "确认该文件可读后重试。",
        )
        .with_raw(e.to_string())
    })?;
    let _ = std::fs::remove_file(marker);
    Ok(serde_json::from_str(&text).unwrap_or_default())
}
