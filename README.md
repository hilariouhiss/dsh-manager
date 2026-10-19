# Tauri + Vue + Typescript

This template should help get you started developing with Tauri, Vue and Typescript on Vite+.

## Recommended IDE Setup

- [VS Code](https://code.visualstudio.com/) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## 发布

推送形如 `v1.0.0` 的 tag 会触发 [.github/workflows/release.yml](.github/workflows/release.yml)：在 `windows-latest` 上构建 NSIS / MSI 安装包，并创建 GitHub Release。

```powershell
git tag v1.0.0
git push origin v1.0.0
```

版本号以 tag 为准：工作流先运行 [.github/scripts/set-version.mjs](.github/scripts/set-version.mjs)，把 `1.0.0` 写进 `package.json`、`src-tauri/tauri.conf.json` 与 `src-tauri/Cargo.toml`（只改 CI 工作区，不回写仓库），因此打 tag 前不必手工改版本号。本地想同步这三处可运行：

```powershell
node .github/scripts/set-version.mjs v1.0.0
```

产物形如 `dsh-manager_1.0.0_x64-setup.exe`（NSIS，推荐）与 `dsh-manager_1.0.0_x64_en-US.msi`。安装包未做代码签名，首次运行会触发 SmartScreen 提示。
