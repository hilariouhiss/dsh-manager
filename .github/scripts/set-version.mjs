#!/usr/bin/env node
// 把 tag（如 v1.0.0）的版本号写进三处清单，供 CI 构建使用。
// 用法：node .github/scripts/set-version.mjs v1.0.0
import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";

const tag = process.argv[2] ?? "";
const version = tag.replace(/^v/, "");
if (!/^\d+\.\d+\.\d+$/.test(version)) {
  console.error(`[set-version] tag 必须形如 v1.0.0，实际收到：${JSON.stringify(tag)}`);
  process.exit(1);
}

const root = process.cwd();
const read = (p) => readFile(path.join(root, p), "utf8");
const write = (p, text) => writeFile(path.join(root, p), text, "utf8");

// 保持原有缩进与结尾换行，让 diff 只落在版本号那一行。
async function syncJson(relPath) {
  const raw = await read(relPath);
  const data = JSON.parse(raw);
  const before = data.version;
  if (before === version) {
    console.log(`[set-version] ${relPath} 已是 ${version}`);
    return;
  }
  data.version = version;
  const trailing = raw.endsWith("\n") ? "\n" : "";
  await write(relPath, JSON.stringify(data, null, 2) + trailing);
  console.log(`[set-version] ${relPath}: ${before} -> ${version}`);
}

async function syncCargo() {
  const relPath = "src-tauri/Cargo.toml";
  const raw = await read(relPath);
  // 只改 [package] 段里的 version，避免碰到依赖里的 version = "2"。
  const packageSectionEnd = raw.indexOf("\n[");
  const head = raw.slice(0, packageSectionEnd === -1 ? raw.length : packageSectionEnd);
  const rest = raw.slice(head.length);
  const nextHead = head.replace(/^version = "[^"]*"$/m, `version = "${version}"`);
  if (!nextHead.includes(`version = "${version}"`)) {
    console.error(`[set-version] ${relPath} 中找不到 [package] 的 version 行`);
    process.exit(1);
  }
  if (nextHead === head) {
    console.log(`[set-version] ${relPath} 已是 ${version}`);
    return;
  }
  const before = head.match(/^version = "([^"]*)"$/m)?.[1] ?? "?";
  await write(relPath, nextHead + rest);
  console.log(`[set-version] ${relPath}: ${before} -> ${version}`);
}

await syncJson("package.json");
await syncJson("src-tauri/tauri.conf.json");
await syncCargo();
console.log(`[set-version] 完成：${version}`);
