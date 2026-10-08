import { readFile } from "node:fs/promises";
import { stableVersion } from "./manifest.mjs";
const config = JSON.parse(await readFile("src-tauri/tauri.conf.json", "utf8"));
const ui = JSON.parse(await readFile("ui/package.json", "utf8"));
const lock = JSON.parse(await readFile("ui/package-lock.json", "utf8"));
const cargo = /\[workspace.package\][\s\S]*?\bversion\s*=\s*"([^"]+)"/.exec(
  await readFile("Cargo.toml", "utf8"),
)?.[1];
const version = stableVersion(config.version);
if (
  [ui.version, lock.version, lock.packages[""].version, cargo].some(
    (v) => v !== version,
  )
)
  throw Error("应用、Cargo 和前端版本不一致");
if (process.env.RELEASE_TAG && process.env.RELEASE_TAG !== `v${version}`)
  throw Error("发布标签与源码版本不一致");
const rustTauri = /\[\[package\]\]\s+name = "tauri"\s+version = "([^"]+)"/.exec(
  await readFile("Cargo.lock", "utf8"),
)?.[1];
const jsTauri = lock.packages["node_modules/@tauri-apps/api"]?.version;
if (
  !rustTauri ||
  !jsTauri ||
  rustTauri.split(".").slice(0, 2).join(".") !==
    jsTauri.split(".").slice(0, 2).join(".")
)
  throw Error(
    `Tauri Rust 与前端 SDK 主次版本不一致：${rustTauri} / ${jsTauri}`,
  );
console.log(`私匣 ${version} 版本核对通过`);
