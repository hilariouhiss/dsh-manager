//! M3 实例运行：spawn、流式输出、状态事件、停止与复核。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde::Serialize;
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
        let drain = async {
            let _ = stdout_task.await;
            let _ = stderr_task.await;
        };
        if tokio::time::timeout(OUTPUT_DRAIN_TIMEOUT, drain)
            .await
            .is_err()
        {
            log::warn!(
                "实例 {} 的进程已退出，但输出管道在 {:?} 内未读到 EOF；仍宣布 stopped",
                supervised.instance_id,
                OUTPUT_DRAIN_TIMEOUT
            );
        }
        supervised.set_state(InstanceState::Stopped);
        emitter.emit_state(state_payload(&supervised, InstanceState::Stopped));
    });

    Ok(entry.instance_id.clone())
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
                "console.log('  Web UI: http://127.0.0.1:3080'); console.log('  Web UI: http://127.0.0.1:3080'); setTimeout(() => {}, 1200);"
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
                "console.log('http://127.0.0.1:3080'); console.error('http://127.0.0.1:3080'); setTimeout(() => {}, 1200);"
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
        let spec = build_start_spec(
            Path::new("cmd.exe"),
            &["/C".to_string(), "echo first&echo second".to_string()],
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

    use crate::error::{INSTANCE_ALREADY_RUNNING, INSTANCE_NOT_MANAGED};
    use crate::ports::process::ProcInfo;

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
