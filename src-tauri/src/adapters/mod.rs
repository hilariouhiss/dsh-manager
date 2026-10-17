//! 适配层：唯一接触外部世界的地方。应用层不得 use 本模块（设计文档 §2.3）。

pub mod bun;
pub mod npm;
pub mod npm_registry;
pub mod pnpm;

#[cfg(test)]
pub mod fake;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use semver::Version;
use tokio::process::Command;

use crate::error::{AppError, CmdResult, INSTALL_FAILED, PATH_PERMISSION_DENIED};
use crate::ports::package_manager::{
    DSH_PACKAGE_NAME, InstallOutcome, PackageManager, PmInfo, PmKind,
};

/// 传给 CreateProcess 的 CREATE_NO_WINDOW：探测子进程不得弹出控制台窗口。
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 在给定目录序列中按 Windows 可执行扩展名顺序解析命令。纯文件系统查找，不启动进程。
pub(crate) fn resolve_in<I>(dirs: I, name: &str) -> Option<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    for dir in dirs {
        for ext in [".cmd", ".exe", ".bat", ""] {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 用进程环境变量 PATH 解析命令。
pub(crate) fn resolve_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    resolve_in(std::env::split_paths(&path), name)
}

/// 按 cmd.exe 规则给一个参数加引号：含空白或 cmd 元字符时用双引号包裹。
fn quote_cmd_arg(arg: &str) -> String {
    let needs = arg.is_empty() || arg.chars().any(|c| " \t&|<>^()".contains(c));
    if needs {
        format!("\"{arg}\"")
    } else {
        arg.to_string()
    }
}

fn is_cmd_launcher(exe: &Path) -> bool {
    exe.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
}

/// 构造一条外部命令。
///
/// Windows 上 .cmd / .bat 不能由 CreateProcess 直接执行，必须经 cmd.exe /C；
/// .exe（如 bun.exe）直接执行。内层命令行整体作为**一个** raw 参数交给 cmd.exe，
/// 外层再包一对引号并加 /S：让 cmd.exe 只剥掉最外层引号。否则可执行文件路径与
/// 参数同时带空格时，cmd.exe 会剥错引号（多个带空格参数时的经典故障）。
pub(crate) fn build_command(exe: &Path, args: &[String]) -> Command {
    if !is_cmd_launcher(exe) {
        let mut cmd = Command::new(exe);
        cmd.args(args);
        return cmd;
    }
    let mut inner = quote_cmd_arg(&exe.to_string_lossy());
    for a in args {
        inner.push(' ');
        inner.push_str(&quote_cmd_arg(a));
    }
    let mut cmd = Command::new("cmd.exe");
    cmd.arg("/d");
    cmd.arg("/s");
    cmd.arg("/c");
    cmd.raw_arg(format!("\"{inner}\""));
    cmd
}

/// 取最后一行非空文本。npm 会把警告写在 stdout 里，最后一行才是答案。
pub(crate) fn last_nonempty_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
}

/// 执行一条只读命令。**spawn 失败返回 None**（视为该包管理器不可用，不构造错误码）。
pub(crate) async fn run_capture(exe: &Path, args: &[&str]) -> Option<(i32, String, String)> {
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let mut cmd = build_command(exe, &owned);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.creation_flags(CREATE_NO_WINDOW);
    let out = cmd.output().await.ok()?;
    Some((
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// `<pm> root -g` 的输出解析。
async fn root_from_command(exe: &Path, args: &[&str]) -> Option<PathBuf> {
    let (code, stdout, _stderr) = run_capture(exe, args).await?;
    if code != 0 {
        return None;
    }
    last_nonempty_line(&stdout).map(PathBuf::from)
}

/// bun 没有 root -g 子命令，改用安装根目录规则：
/// %BUN_INSTALL%\install\global\node_modules，未设置 BUN_INSTALL 时默认 %USERPROFILE%\.bun。
///
/// > 待核实：bun 全局安装目录的确切布局（本机 bun 未装过全局包，无法实测）。若与实测不符，只需改本函数。
fn bun_global_root_from(
    bun_install: Option<&OsStr>,
    user_profile: Option<&OsStr>,
) -> Option<PathBuf> {
    let base = match bun_install {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(user_profile?).join(".bun"),
    };
    Some(base.join("install").join("global").join("node_modules"))
}

fn bun_global_root() -> Option<PathBuf> {
    bun_global_root_from(
        std::env::var_os("BUN_INSTALL").as_deref(),
        std::env::var_os("USERPROFILE").as_deref(),
    )
}

/// 探测一个已知路径的包管理器。
pub(crate) async fn probe_kind(kind: PmKind, exe: PathBuf) -> Option<PmInfo> {
    let (code, stdout, _stderr) = run_capture(&exe, &["--version"]).await?;
    if code != 0 {
        return None;
    }
    let version = last_nonempty_line(&stdout)?;
    let global_root = match kind {
        PmKind::Npm | PmKind::Pnpm => root_from_command(&exe, &["root", "-g"]).await?,
        PmKind::Bun => bun_global_root()?,
    };
    Some(PmInfo {
        kind,
        exe,
        version,
        global_root,
    })
}

/// 在 PATH 中探测一个包管理器。
pub(crate) async fn probe(kind: PmKind) -> Option<PmInfo> {
    let name = match kind {
        PmKind::Npm => "npm",
        PmKind::Pnpm => "pnpm",
        PmKind::Bun => "bun",
    };
    probe_kind(kind, resolve_on_path(name)?).await
}

fn unreadable_manifest(manifest: &Path, e: &std::io::Error) -> AppError {
    AppError::new(
        PATH_PERMISSION_DENIED,
        format!(
            "无法读取本机 DSH 的包描述文件：{}（{e}）",
            manifest.display()
        ),
        "确认该路径对当前用户可读（可能需要以同一用户身份运行本工具），然后重试。",
    )
    .with_raw(e.to_string())
}

/// 从 `<global_root>/@deepseek-ai/dsh/package.json` 读取已安装版本（npm/bun 布局）。
pub(crate) fn read_installed_version(global_root: &Path) -> CmdResult<Option<Version>> {
    read_installed_version_at(&global_root.join(DSH_PACKAGE_NAME).join("package.json"))
}

/// 从指定 package.json 读取已安装版本。
///
/// - 文件不存在（NotFound）→ Ok(None)（未安装）
/// - 其他元数据/读取错误（如权限不足）→ Err，**不得**降级为「未安装」（原则 P4）
/// - 文件存在但解析不了 / 版本串非法 → Err
pub(crate) fn read_installed_version_at(manifest: &Path) -> CmdResult<Option<Version>> {
    match std::fs::metadata(manifest) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable_manifest(manifest, &e)),
        Ok(_) => {}
    }
    let text = std::fs::read_to_string(manifest).map_err(|e| unreadable_manifest(manifest, &e))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
        AppError::new(
            PATH_PERMISSION_DENIED,
            format!("本机 DSH 的包描述文件不是合法 JSON：{}", manifest.display()),
            "该安装可能已损坏；可先用对应包管理器重新安装 `@deepseek-ai/dsh`，再回到本页重试。",
        )
        .with_raw(e.to_string())
    })?;
    let raw = json
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            AppError::new(
                PATH_PERMISSION_DENIED,
                format!(
                    "本机 DSH 的包描述文件缺少 version 字段：{}",
                    manifest.display()
                ),
                "该安装可能已损坏；可先用对应包管理器重新安装 `@deepseek-ai/dsh`，再回到本页重试。",
            )
        })?;
    Version::parse(raw).map(Some).map_err(|e| {
        AppError::new(
            PATH_PERMISSION_DENIED,
            format!("本机 DSH 的版本号不是合法的语义化版本：{raw}"),
            "该安装可能已损坏；可先用对应包管理器重新安装 `@deepseek-ai/dsh`，再回到本页重试。",
        )
        .with_raw(e.to_string())
    })
}

/// 执行全局安装。**只有安装流程会调用它；来源识别路径永不调用**（FR-2.4）。
pub(crate) async fn run_install(
    pm: &PmInfo,
    verb: &[&str],
    tarball: &Path,
) -> CmdResult<InstallOutcome> {
    let mut args: Vec<String> = verb.iter().map(|s| (*s).to_string()).collect();
    args.push(tarball.to_string_lossy().into_owned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let label = pm.kind.label_zh();
    let (code, stdout, stderr) = run_capture(&pm.exe, &refs).await.ok_or_else(|| {
        AppError::new(
            INSTALL_FAILED,
            format!("无法启动 {label} 执行安装：{}", pm.exe.display()),
            "确认该包管理器可执行且未被安全软件拦截，然后重试。",
        )
        .retryable(true)
    })?;
    let mut output = stdout;
    if !stderr.is_empty() {
        output.push_str(&stderr);
    }
    if code != 0 {
        return Err(AppError::new(
            INSTALL_FAILED,
            format!("{label} 安装子进程以退出码 {code} 结束"),
            format!(
                "原始输出见下方；也可在命令行手工执行 {label} {} 完成安装。",
                refs.join(" ")
            ),
        )
        .with_raw(output)
        .retryable(true));
    }
    Ok(InstallOutcome {
        exit_code: code,
        output,
    })
}

/// 三个适配器的组合体：装配点注入的就是它。
pub struct AllPackageManagers;

#[async_trait]
impl PackageManager for AllPackageManagers {
    async fn available(&self) -> Vec<PmInfo> {
        let mut out = npm::NpmAdapter.available().await;
        out.extend(pnpm::PnpmAdapter.available().await);
        out.extend(bun::BunAdapter.available().await);
        out
    }

    async fn installed_version(&self, pm: &PmInfo) -> CmdResult<Option<Version>> {
        match pm.kind {
            PmKind::Npm => npm::NpmAdapter.installed_version(pm).await,
            PmKind::Pnpm => pnpm::PnpmAdapter.installed_version(pm).await,
            PmKind::Bun => bun::BunAdapter.installed_version(pm).await,
        }
    }

    async fn install_from_tarball(&self, pm: &PmInfo, tarball: &Path) -> CmdResult<InstallOutcome> {
        match pm.kind {
            PmKind::Npm => npm::NpmAdapter.install_from_tarball(pm, tarball).await,
            PmKind::Pnpm => pnpm::PnpmAdapter.install_from_tarball(pm, tarball).await,
            PmKind::Bun => bun::BunAdapter.install_from_tarball(pm, tarball).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use semver::Version;

    use super::fake::fake_pm::{FakePm, fake_pm_info};
    use super::*;
    use crate::error::INSTALL_FAILED;
    use crate::ports::package_manager::{DSH_PACKAGE_NAME, PmKind};

    /// 每个用例一个独立临时目录，避免并行测试互相踩。
    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dsh-manager-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn resolve_in_finds_cmd_launcher() {
        let dir = temp_dir("resolve");
        std::fs::write(dir.join("npm.cmd"), "@echo off\r\n").unwrap();
        assert_eq!(
            resolve_in(vec![dir.clone()], "npm"),
            Some(dir.join("npm.cmd"))
        );
    }

    #[test]
    fn resolve_in_returns_none_when_missing() {
        let dir = temp_dir("resolve-miss");
        assert_eq!(resolve_in(vec![dir], "pnpm"), None);
    }

    #[tokio::test]
    async fn run_capture_executes_a_cmd_stub_through_cmd_exe() {
        let dir = temp_dir("cmd-stub");
        let stub = dir.join("npm.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\necho npm warn deprecated old-pkg\r\necho 11.6.2\r\n",
        )
        .unwrap();
        let (code, stdout, _stderr) = run_capture(&stub, &["--version"])
            .await
            .expect("必须能执行 .cmd");
        assert_eq!(code, 0);
        assert_eq!(last_nonempty_line(&stdout).as_deref(), Some("11.6.2"));
    }

    #[tokio::test]
    async fn run_capture_reports_nonzero_exit_and_keeps_stderr() {
        let dir = temp_dir("cmd-stub-fail");
        let stub = dir.join("bad.cmd");
        std::fs::write(&stub, "@echo off\r\necho boom 1>&2\r\nexit /b 3\r\n").unwrap();
        let (code, _stdout, stderr) = run_capture(&stub, &[]).await.unwrap();
        assert_eq!(code, 3);
        assert!(stderr.contains("boom"), "stderr 原文必须回传：{stderr:?}");
    }

    #[test]
    fn quote_cmd_arg_wraps_only_when_needed() {
        assert_eq!(quote_cmd_arg("--version"), "--version");
        assert_eq!(
            quote_cmd_arg(r"C:\Program Files\nodejs\npm.cmd"),
            r#""C:\Program Files\nodejs\npm.cmd""#
        );
    }

    /// Windows 上 .cmd/.bat 必须经 cmd.exe；.exe 直接执行（本机 bun 是 bun.exe）。
    #[test]
    fn exe_launchers_run_directly_while_cmd_launchers_go_through_cmd_exe() {
        let npm = build_command(
            Path::new(r"C:\Program Files\nodejs\npm.cmd"),
            &["--version".to_string()],
        );
        assert_eq!(npm.as_std().get_program(), OsStr::new("cmd.exe"));
        let bun = build_command(
            Path::new(r"C:\Users\me\.bun\bin\bun.exe"),
            &["--version".to_string()],
        );
        assert_eq!(
            bun.as_std().get_program(),
            OsStr::new(r"C:\Users\me\.bun\bin\bun.exe")
        );
    }

    /// 一次探测只允许执行两条只读命令，且必须真的经过 cmd.exe 包裹。
    #[tokio::test]
    async fn probe_uses_only_read_only_commands() {
        let dir = temp_dir("probe-readonly");
        let stub = dir.join("npm.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\necho %* >> \"%~dp0calls.log\"\r\nif \"%1\"==\"--version\" echo 11.6.2\r\nif \"%1\"==\"root\" echo C:\\Users\\me\\AppData\\Roaming\\npm\\node_modules\r\n",
        )
        .unwrap();

        let info = probe_kind(PmKind::Npm, stub.clone())
            .await
            .expect("探测必须成功");

        assert_eq!(info.version, "11.6.2");
        assert_eq!(
            info.global_root,
            PathBuf::from(r"C:\Users\me\AppData\Roaming\npm\node_modules")
        );
        let calls = std::fs::read_to_string(dir.join("calls.log")).unwrap();
        let calls: Vec<&str> = calls
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            calls,
            vec!["--version", "root -g"],
            "探测只允许执行这两条只读命令"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("install") || c.starts_with("add")),
            "安装绝不能被探测路径调用：{calls:?}"
        );
    }

    /// 识别与读取版本路径不得触发任何写操作（FR-2.4）。
    #[tokio::test]
    async fn probe_path_never_calls_install_from_tarball() {
        let dir = temp_dir("fake-write");
        let fake = FakePm::new(vec![fake_pm_info(PmKind::Npm, dir)])
            .with_installed(PmKind::Npm, Some(Version::parse("0.2.1-alpha.4").unwrap()));
        let infos = fake.available().await;
        for info in &infos {
            fake.installed_version(info).await.unwrap();
        }
        assert!(
            fake.write_calls().is_empty(),
            "识别与读取版本路径不得调用 install_from_tarball：{:?}",
            fake.write_calls()
        );
        assert_eq!(fake.calls(), vec!["available", "installed_version:npm"]);
    }

    #[test]
    fn installed_version_is_none_when_package_absent() {
        let root = temp_dir("pm-absent");
        assert_eq!(
            read_installed_version(&root).unwrap(),
            None,
            "未安装必须是 Ok(None)，不是错误"
        );
    }

    #[test]
    fn installed_version_reads_package_json() {
        let root = temp_dir("pm-present");
        let pkg = root.join(DSH_PACKAGE_NAME);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh","version":"0.2.1-alpha.4"}"#,
        )
        .unwrap();
        assert_eq!(
            read_installed_version(&root).unwrap(),
            Some(Version::parse("0.2.1-alpha.4").unwrap())
        );
    }

    #[test]
    fn installed_version_errors_on_broken_metadata() {
        let root = temp_dir("pm-broken-json");
        let pkg = root.join(DSH_PACKAGE_NAME);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), "{ not json").unwrap();
        let err = read_installed_version(&root).unwrap_err();
        assert_eq!(err.code, PATH_PERMISSION_DENIED);
        assert!(err.raw.is_some(), "必须保留底层原文（原则 P4）");

        let root2 = temp_dir("pm-broken-semver");
        let pkg2 = root2.join(DSH_PACKAGE_NAME);
        std::fs::create_dir_all(&pkg2).unwrap();
        std::fs::write(pkg2.join("package.json"), r#"{"version":"not-a-version"}"#).unwrap();
        let err2 = read_installed_version(&root2).unwrap_err();
        assert_eq!(err2.code, PATH_PERMISSION_DENIED);
        assert!(
            err2.reason.contains("not-a-version"),
            "原因里要带上非法值：{}",
            err2.reason
        );
    }

    /// 元数据存在但不可读 → 必须是 path.permission_denied，不得降级为「未安装」（§3.2 / P4）。
    #[test]
    fn installed_version_errors_when_metadata_is_unreadable() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let root = temp_dir("pm-unreadable");
        let pkg = root.join(DSH_PACKAGE_NAME);
        std::fs::create_dir_all(&pkg).unwrap();
        let manifest = pkg.join("package.json");
        std::fs::write(&manifest, r#"{"version":"0.2.1-alpha.4"}"#).unwrap();
        // 以独占方式持有文件（不共享），随后读取必然失败；metadata 仍可访问。
        let _lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&manifest)
            .unwrap();

        let err = read_installed_version(&root).unwrap_err();
        assert_eq!(err.code, PATH_PERMISSION_DENIED);
        assert!(err.raw.is_some(), "必须保留底层原文（原则 P4）");
        assert!(
            err.reason.contains("package.json"),
            "原因必须带上具体文件：{}",
            err.reason
        );
    }

    /// §3.2：install.failed 必须可重试，且 raw 里保留子进程原文。
    #[tokio::test]
    async fn install_failure_is_retryable_and_keeps_output() {
        let dir = temp_dir("install-fail");
        let stub = dir.join("npm.cmd");
        std::fs::write(
            &stub,
            "@echo off\r\necho install boom 1>&2\r\nexit /b 7\r\n",
        )
        .unwrap();
        let pm = PmInfo {
            kind: PmKind::Npm,
            exe: stub,
            version: "11.6.2".to_string(),
            global_root: dir.clone(),
        };
        let err = run_install(&pm, &["install", "-g"], &dir.join("dsh.tgz"))
            .await
            .unwrap_err();
        assert_eq!(err.code, INSTALL_FAILED);
        assert!(err.retryable, "install.failed 必须可重试（主计划 §3.2）");
        let raw = err.raw.expect("失败必须保留子进程原文");
        assert!(
            raw.contains("install boom"),
            "raw 必须包含子进程输出：{raw:?}"
        );
    }

    #[test]
    fn bun_global_root_follows_bun_install() {
        let got = bun_global_root_from(Some(OsStr::new(r"D:\bun")), None);
        assert_eq!(
            got,
            Some(PathBuf::from(r"D:\bun\install\global\node_modules"))
        );
    }

    #[test]
    fn bun_global_root_defaults_under_user_profile() {
        let got = bun_global_root_from(None, Some(OsStr::new(r"C:\Users\me")));
        assert_eq!(
            got,
            Some(PathBuf::from(
                r"C:\Users\me\.bun\install\global\node_modules"
            ))
        );
    }

    /// §5 T-O1 验收：在本机读出准确版本与全局根目录。
    /// 依赖本机已安装 npm 与全局 `@deepseek-ai/dsh`，因此默认忽略。
    #[tokio::test]
    #[ignore = "本机验收：需要已安装 npm 与全局 @deepseek-ai/dsh"]
    async fn reads_the_real_npm_global_install_on_this_machine() {
        let pm = AllPackageManagers;
        let available = pm.available().await;
        eprintln!("available = {available:?}");
        for info in &available {
            let installed = pm
                .installed_version(info)
                .await
                .expect("读取本机已装版本不得失败");
            eprintln!(
                "{} exe={} version={} global_root={} installed={:?}",
                info.kind.label_zh(),
                info.exe.display(),
                info.version,
                info.global_root.display(),
                installed
            );
        }
        let npm = available
            .iter()
            .find(|i| i.kind == PmKind::Npm)
            .expect("本机必须能探测到 npm（§5 T-O1 的验收前提）");
        assert!(
            npm.global_root.is_dir(),
            "全局根目录必须真实存在：{}",
            npm.global_root.display()
        );
        assert!(
            npm.global_root.join(DSH_PACKAGE_NAME).is_dir(),
            "全局根目录必须包含 {DSH_PACKAGE_NAME}：{}",
            npm.global_root.display()
        );
        let installed = pm
            .installed_version(npm)
            .await
            .expect("读取本机已装版本不得失败");
        let version = installed.expect("本机 npm 全局已安装 `@deepseek-ai/dsh`");
        eprintln!("npm.exe = {}", npm.exe.display());
        eprintln!("npm.version = {}", npm.version);
        eprintln!("npm.global_root = {}", npm.global_root.display());
        eprintln!("installed @deepseek-ai/dsh = {version}");
        assert!(version >= Version::parse("0.0.1").unwrap());
    }
}
