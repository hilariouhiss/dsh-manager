//! M3 实例发现：从进程快照里挑出 DSH Web 形态实例（§3.9 D-6）。
//!
//! 单一事实来源是**进程枚举**，不是后端内存记录（原则 P5）。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::error::CmdResult;
use crate::m3::InstanceState;
use crate::m3::instance_runner::{ManagedInstance, ManagedRegistry};
use crate::ports::process::{ProcInfo, ProcessProbe};

/// 特征 1：bin.js 的路径尾部（规范化反斜杠为正斜杠、大小写不敏感之后比较）。
pub const DSH_BIN_SUFFIX: &str = "/@deepseek-ai/dsh/lib/bin.js";
/// 特征 2a：bin.js 之后的第一个非选项参数。
pub const WEB_POSITIONAL: &str = "web";
/// 特征 2b：紧邻的配置档组合。
pub const PROFILE_FLAG: &str = "--profile";
/// 特征 2c：--profile=web 的等号写法。
pub const PROFILE_EQ_PREFIX: &str = "--profile=";

/// 派生视图：由进程枚举得出（§3.6）。startedAtMs / uptimeMs / url 都可能为 null，
/// 界面在 null 时如实显示「无法取得」，不留空、不猜测（FR-4.1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceInfo {
    pub instance_id: String,
    pub managed: bool,
    pub pid: u32,
    pub started_at_ms: Option<i64>,
    pub uptime_ms: Option<i64>,
    pub url: Option<String>,
    pub state: InstanceState,
}

/// 规范化一个命令行 token：去首尾引号、反斜杠换正斜杠。
fn normalize(token: &str) -> String {
    token.trim_matches('"').replace('\\', "/")
}

/// 若该命令行是 DSH 的调用，返回 bin.js 那个 token 的下标（取最后一个，容忍包装层）。
pub fn find_bin_index(cmdline: &[String]) -> Option<usize> {
    let mut found = None;
    for (i, token) in cmdline.iter().enumerate() {
        if normalize(token)
            .to_ascii_lowercase()
            .ends_with(DSH_BIN_SUFFIX)
        {
            found = Some(i);
        }
    }
    found
}

/// §3.9 D-6 规则 2：是否 Web 形态。
pub fn is_web_form(cmdline: &[String]) -> bool {
    let Some(idx) = find_bin_index(cmdline) else {
        return false;
    };
    let rest = &cmdline[idx + 1..];

    if let Some(first) = rest.iter().find(|t| !t.starts_with('-'))
        && first.eq_ignore_ascii_case(WEB_POSITIONAL)
    {
        return true;
    }
    for window in rest.windows(2) {
        if window[0].eq_ignore_ascii_case(PROFILE_FLAG)
            && window[1].eq_ignore_ascii_case(WEB_POSITIONAL)
        {
            return true;
        }
    }
    rest.iter().any(|t| {
        t.to_ascii_lowercase().starts_with(PROFILE_EQ_PREFIX)
            && t[PROFILE_EQ_PREFIX.len()..].eq_ignore_ascii_case(WEB_POSITIONAL)
    })
}

/// 取 --flag value 或 --flag=value 的值。
fn flag_value(cmdline: &[String], flag: &str) -> Option<String> {
    for (i, token) in cmdline.iter().enumerate() {
        if token.eq_ignore_ascii_case(flag) {
            return cmdline.get(i + 1).map(|v| v.trim_matches('"').to_string());
        }
    }
    let prefix = format!("{flag}=");
    cmdline.iter().find_map(|t| {
        if t.to_ascii_lowercase().starts_with(&prefix) {
            Some(t[prefix.len()..].trim_matches('"').to_string())
        } else {
            None
        }
    })
}

/// §3.9「地址获取顺序」：只从 --host / --port 取。
/// --port 0（让系统挑端口）或根本没有 --port 时返回 None——如实显示「无法取得」。
pub fn address_from_cmdline(cmdline: &[String]) -> Option<String> {
    let port: u16 = flag_value(cmdline, "--port")?.parse().ok()?;
    if port == 0 {
        return None;
    }
    let host = flag_value(cmdline, "--host").unwrap_or_else(|| "127.0.0.1".to_string());
    if host.contains(':') && !host.starts_with('[') {
        Some(format!("http://[{host}]:{port}"))
    } else {
        Some(format!("http://{host}:{port}"))
    }
}

/// §3.9 规则 3：本进程自身及其全部祖先进程的 pid。
pub fn ancestor_pids(snapshot: &[ProcInfo], self_pid: u32) -> HashSet<u32> {
    let mut out = HashSet::new();
    out.insert(self_pid);
    let mut current = self_pid;
    // 进程树有限；上限用于防御异常数据造成的环。
    for _ in 0..64 {
        let parent = snapshot
            .iter()
            .find(|p| p.pid == current)
            .and_then(|p| p.parent_pid);
        match parent {
            Some(pid) if pid != 0 && out.insert(pid) => current = pid,
            _ => break,
        }
    }
    out
}

/// 枚举 DSH Web 形态实例。没有实例时返回**空数组**且 Ok（FR-4.1：界面显示「未运行」，不得报错）。
///
/// managed 是本工具已启动实例的登记；进程**自身或任一祖先进程**的 pid 命中即视为受管——
/// 工具 spawn 的是 cmd.exe 外壳，枚举看到的却是它下面的 node.exe（修复 1）。
/// now_ms 由调用方注入，便于测试。
pub fn discover(
    probe: &dyn ProcessProbe,
    managed: &[ManagedInstance],
    now_ms: i64,
) -> CmdResult<Vec<InstanceInfo>> {
    let snapshot = probe.snapshot()?;
    let excluded = ancestor_pids(&snapshot, std::process::id());

    let mut out = Vec::new();
    for proc in &snapshot {
        if excluded.contains(&proc.pid) || !is_web_form(&proc.cmdline) {
            continue;
        }
        let matched = if managed.is_empty() {
            None
        } else {
            let chain = ancestor_pids(&snapshot, proc.pid);
            managed.iter().find(|m| chain.contains(&m.pid))
        };
        let (instance_id, is_managed, started_at_ms) = match matched {
            Some(m) => (m.instance_id.clone(), true, Some(m.started_at_ms)),
            None => (
                format!("pid-{}", proc.pid),
                false,
                proc.started_at_epoch_secs.map(|s| s as i64 * 1000),
            ),
        };
        out.push(InstanceInfo {
            instance_id,
            managed: is_managed,
            pid: proc.pid,
            started_at_ms,
            uptime_ms: started_at_ms.map(|s| (now_ms - s).max(0)),
            url: address_from_cmdline(&proc.cmdline),
            state: InstanceState::Running,
        });
    }
    out.sort_by_key(|i| i.pid);
    Ok(out)
}

/// M1（安装执行）需要的能力：列出当前运行的 Web 形态实例。
///
/// M1 **只允许**依赖本 trait，不得 use M3 的实现细节（设计文档 §2.3 的分层与 §3.3 的模块边界）。
/// 它是「安装前运行检测」的唯一注入点，让 M1 的测试可以塞一个假实现。
pub trait InstanceLister: Send + Sync {
    fn list_web_instances(&self) -> CmdResult<Vec<InstanceInfo>>;
}

/// M3 侧对外的唯一出口：把进程端口与受管实例登记表组合成 InstanceLister。
pub struct ProcessLister {
    probe: Arc<dyn ProcessProbe>,
    managed: Arc<Mutex<ManagedRegistry>>,
}

impl ProcessLister {
    pub fn new(probe: Arc<dyn ProcessProbe>, managed: Arc<Mutex<ManagedRegistry>>) -> Self {
        Self { probe, managed }
    }
}

impl InstanceLister for ProcessLister {
    fn list_web_instances(&self) -> CmdResult<Vec<InstanceInfo>> {
        let managed = self.managed.lock().expect("受管登记锁被毒化").snapshot();
        discover(self.probe.as_ref(), &managed, crate::m3::now_ms())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::adapters::fake::fake_probe::FakeProbe;
    use crate::m3::now_ms;

    const NODE: &str = r"C:\Software\nodejs\node.exe";
    const BIN: &str = r"C:\Users\me\AppData\Roaming\npm/node_modules/@deepseek-ai/dsh/lib/bin.js";
    const CODEGRAPH: &str = r"C:\Users\me\AppData\Roaming\npm\node_modules\@colbymchenry\codegraph\lib\dist\bin\codegraph.js";

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    fn proc(pid: u32, parent: Option<u32>, started: Option<u64>, cmdline: &[&str]) -> ProcInfo {
        ProcInfo {
            pid,
            parent_pid: parent,
            cmdline: args(cmdline),
            started_at_epoch_secs: started,
            exe: None,
        }
    }

    /// §3.9 D-6：五种命令行里只有前两种是 Web 形态实例。
    #[test]
    fn only_web_dsh_processes_match() {
        // 1. 本工具实例（与实测命令行同形）
        let own = args(&[NODE, BIN, "web", "--port", "3080", "--no-open"]);
        // 2. 外部 web 实例（--profile web 写法）
        let external = args(&[
            NODE,
            BIN,
            "--profile",
            "web",
            "--host",
            "10.0.0.7",
            "--port",
            "8080",
        ]);
        // 3. dsh tui
        let tui = args(&[NODE, BIN, "tui"]);
        // 4. codegraph serve
        let codegraph = args(&[
            NODE,
            CODEGRAPH,
            "serve",
            "--mcp",
            "--path",
            r"C:\Mine\dsh-manager",
        ]);
        // 5. 无关 node
        let other = args(&[NODE, "-e", "console.log(1)"]);

        assert!(is_web_form(&own), "实测命令行必须被识别");
        assert!(is_web_form(&external), "--profile web 写法必须被识别");
        assert!(!is_web_form(&tui));
        assert!(!is_web_form(&codegraph));
        assert!(!is_web_form(&other));
    }

    #[test]
    fn bin_token_may_be_quoted_or_use_backslashes() {
        let quoted = args(&[
            NODE,
            r#""C:\Users\me\AppData\Roaming\npm\node_modules\@deepseek-ai\dsh\lib\bin.js""#,
            "web",
        ]);
        assert!(is_web_form(&quoted), "带引号且全反斜杠的写法也要认");
        let mixed = args(&[NODE, BIN, "--profile=web", "--port", "3080"]);
        assert!(is_web_form(&mixed), "--profile=web 等价写法也要认");
    }

    #[test]
    fn address_comes_only_from_host_and_port_flags() {
        assert_eq!(
            address_from_cmdline(&args(&[NODE, BIN, "web", "--port", "3080"])),
            Some("http://127.0.0.1:3080".to_string())
        );
        assert_eq!(
            address_from_cmdline(&args(&[
                NODE, BIN, "web", "--host", "10.0.0.7", "--port", "8080"
            ])),
            Some("http://10.0.0.7:8080".to_string())
        );
        assert_eq!(
            address_from_cmdline(&args(&[
                NODE, BIN, "web", "--host", "::1", "--port", "8080"
            ])),
            Some("http://[::1]:8080".to_string()),
            "IPv6 字面量要加方括号"
        );
        assert_eq!(
            address_from_cmdline(&args(&[NODE, BIN, "web", "--port", "0"])),
            None,
            "--port 0 表示让系统挑端口，地址不可得"
        );
        assert_eq!(
            address_from_cmdline(&args(&[NODE, BIN, "web"])),
            None,
            "没有 --port 时地址不可得（实测的本会话实例就是这种）"
        );
        assert_eq!(
            address_from_cmdline(&args(&[NODE, BIN, "web", "--port=3080"])),
            Some("http://127.0.0.1:3080".to_string())
        );
    }

    #[test]
    fn ancestors_and_self_are_collected() {
        let snapshot = vec![
            proc(10, None, None, &["explorer.exe"]),
            proc(20, Some(10), None, &[NODE, BIN, "web"]),
            proc(30, Some(20), None, &["dsh-manager.exe"]),
        ];
        let excluded = ancestor_pids(&snapshot, 30);
        assert!(excluded.contains(&30), "自身必须被排除");
        assert!(
            excluded.contains(&20) && excluded.contains(&10),
            "祖先必须被排除"
        );
        assert!(!excluded.contains(&999));
    }

    fn five_process_table() -> Vec<ProcInfo> {
        vec![
            proc(
                100,
                Some(1),
                Some(1_700_000_000),
                &[NODE, BIN, "web", "--port", "3080"],
            ),
            proc(
                200,
                Some(1),
                Some(1_700_000_100),
                &[
                    NODE,
                    BIN,
                    "--profile",
                    "web",
                    "--host",
                    "10.0.0.7",
                    "--port",
                    "8080",
                ],
            ),
            proc(300, Some(1), Some(1_700_000_200), &[NODE, BIN, "tui"]),
            proc(
                400,
                Some(1),
                Some(1_700_000_300),
                &[NODE, CODEGRAPH, "serve", "--mcp"],
            ),
            proc(
                500,
                Some(1),
                Some(1_700_000_400),
                &[NODE, "-e", "console.log(1)"],
            ),
        ]
    }

    #[test]
    fn discovers_exactly_the_two_web_instances() {
        let probe = FakeProbe::new(five_process_table());
        let managed = vec![ManagedInstance {
            pid: 100,
            instance_id: "inst-1".to_string(),
            started_at_ms: 1_700_000_000_000,
        }];

        let list = discover(&probe, &managed, 1_700_000_500_000).unwrap();

        assert_eq!(list.len(), 2, "五种命令行里只允许识别出前两种：{:?}", list);
        assert_eq!(list[0].pid, 100);
        assert!(list[0].managed);
        assert_eq!(list[0].instance_id, "inst-1");
        assert_eq!(list[0].url.as_deref(), Some("http://127.0.0.1:3080"));
        assert_eq!(list[0].uptime_ms, Some(500_000));

        assert_eq!(list[1].pid, 200);
        assert!(
            !list[1].managed,
            "用户自行启动的实例 managed 必须为 false（FR-4.3）"
        );
        assert_eq!(list[1].instance_id, "pid-200");
        assert_eq!(list[1].url.as_deref(), Some("http://10.0.0.7:8080"));
    }

    #[test]
    fn port_zero_and_missing_start_time_give_null() {
        let probe = FakeProbe::new(vec![proc(
            200,
            Some(1),
            None,
            &[NODE, BIN, "web", "--port", "0"],
        )]);
        let list = discover(&probe, &[], now_ms()).unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].started_at_ms, None);
        assert_eq!(
            list[0].uptime_ms, None,
            "启动时间读不到时运行时长必须为 null，不得填 0"
        );
        assert_eq!(list[0].url, None, "--port 0 时地址不可得");
    }

    #[test]
    fn empty_snapshot_is_ok_and_empty() {
        let probe = FakeProbe::new(Vec::new());
        let list = discover(&probe, &[], now_ms()).expect("没有实例不是错误");
        assert!(
            list.is_empty(),
            "没有实例时返回空数组，界面显示「未运行」（FR-4.1）"
        );
    }

    #[test]
    fn own_process_is_excluded_even_when_it_matches() {
        let me = std::process::id();
        let probe = FakeProbe::new(vec![proc(
            me,
            Some(1),
            Some(1_700_000_000),
            &[NODE, BIN, "web", "--port", "3080"],
        )]);
        assert!(
            discover(&probe, &[], 1_700_000_500_000).unwrap().is_empty(),
            "不得把本工具自己当成实例"
        );
    }

    /// 修复 1：工具 spawn 的 cmd.exe 外壳下面才是 node.exe，必须按祖先链命中登记，
    /// 并沿用登记的 inst-* 标识与启动时间；无关的 web 进程仍必须是 pid-<pid>。
    #[test]
    fn managed_child_behind_a_cmd_shim_keeps_the_registered_id() {
        let probe = FakeProbe::new(vec![
            proc(
                10,
                Some(1),
                Some(1_700_000_000),
                &["cmd.exe", "/C", r#""C:\npm\dsh.cmd" web --port 3080"#],
            ),
            proc(
                20,
                Some(10),
                Some(1_700_000_010),
                &[NODE, BIN, "web", "--port", "3080"],
            ),
            proc(
                30,
                Some(1),
                Some(1_700_000_020),
                &[NODE, BIN, "web", "--port", "3099"],
            ),
        ]);
        let managed = vec![ManagedInstance {
            pid: 10,
            instance_id: "inst-shim".to_string(),
            started_at_ms: 1_700_000_000_000,
        }];

        let list = discover(&probe, &managed, 1_700_000_100_000).unwrap();

        assert_eq!(
            list.len(),
            2,
            "外壳进程本身不是 web 形态，只有两个 node：{list:?}"
        );
        assert_eq!(list[0].pid, 20);
        assert!(list[0].managed, "外壳的 node 子进程必须被认成本工具实例");
        assert_eq!(list[0].instance_id, "inst-shim");
        assert_eq!(list[0].started_at_ms, Some(1_700_000_000_000));
        assert_eq!(list[0].uptime_ms, Some(100_000));

        assert_eq!(list[1].pid, 30);
        assert!(!list[1].managed, "无关 web 进程不得被认成受管");
        assert_eq!(list[1].instance_id, "pid-30");
    }

    /// M1 只依赖 InstanceLister 这个 trait，且必须能以 trait 对象注入（子计划 B 的 T-V6 就靠它替换假实现）。
    #[test]
    fn instance_lister_is_the_only_surface_m1_needs() {
        let registry = Arc::new(Mutex::new(ManagedRegistry::default()));
        registry.lock().unwrap().register(ManagedInstance {
            pid: 100,
            instance_id: "inst-lister".to_string(),
            started_at_ms: 1_700_000_000_000,
        });
        let lister = ProcessLister::new(Arc::new(FakeProbe::new(five_process_table())), registry);
        let dynamic: &dyn InstanceLister = &lister;
        let list = dynamic.list_web_instances().unwrap();
        assert_eq!(list.len(), 2);
        assert!(
            list[0].managed,
            "lister 必须用上 runner 的受管登记（修复 1）"
        );
        assert_eq!(list[0].instance_id, "inst-lister");
    }

    /// §5 T-I1 验收（E2E-7 前半）：本机当前运行的 dsh web 实例被列出。
    #[test]
    #[ignore = "本机验收：需要本机正在运行 dsh web 实例"]
    fn lists_the_running_web_instance_on_this_machine() {
        let probe = crate::adapters::sysinfo_probe::SysinfoProbe::new();
        let list = discover(&probe, &[], now_ms()).expect("进程枚举不得失败");
        eprintln!("{}", serde_json::to_string_pretty(&list).unwrap());
        assert!(!list.is_empty(), "本机当前应有运行中的 dsh web 实例");
        assert!(
            list.iter().any(|i| i.started_at_ms.is_some()),
            "至少一个实例能读到启动时间"
        );
    }
}
