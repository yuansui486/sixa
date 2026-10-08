// Regenerates only the tiny, signed, offline test app. Never uses the production key.
import {
  mkdtempSync,
  readFileSync,
  writeFileSync,
  unlinkSync,
  rmdirSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";
import { spawnSync } from "node:child_process";
const directory = fileURLToPath(
  new URL("../../src-tauri/src/fixtures/updater/", import.meta.url),
);
const cli = fileURLToPath(
  new URL("../../ui/node_modules/@tauri-apps/cli/tauri.js", import.meta.url),
);
const temp = mkdtempSync(join(tmpdir(), "sixa-fixture-key-"));
const key = join(temp, "key");
function run(args) {
  const result = spawnSync(process.execPath, [cli, "signer", ...args], {
    timeout: 15000,
    encoding: "utf8",
    input: "\n",
  });
  if (result.status !== 0) throw Error("测试密钥或签名生成失败");
}
function entry(name, value, mode) {
  const body = Buffer.from(value),
    header = Buffer.alloc(512);
  header.write(name, 0, 100);
  for (const [offset, width, number] of [
    [100, 8, mode],
    [108, 8, 0],
    [116, 8, 0],
    [124, 12, body.length],
    [136, 12, 0],
  ])
    header.write(
      number.toString(8).padStart(width - 1, "0") + "\0",
      offset,
      width,
    );
  header.fill(32, 148, 156);
  header.write("0", 156);
  header.write("ustar\0", 257);
  header.write("00", 263);
  header.write(
    [...header]
      .reduce((sum, value) => sum + value, 0)
      .toString(8)
      .padStart(6, "0") + "\0 ",
    148,
    8,
  );
  return Buffer.concat([
    header,
    body,
    Buffer.alloc((512 - (body.length % 512)) % 512),
  ]);
}
const archive = join(directory, "mac-fixture.app.tar.gz");
writeFileSync(
  archive,
  gzipSync(
    Buffer.concat([
      entry(
        "SixaFixture.app/Contents/Info.plist",
        '<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>sixa-fixture</string><key>CFBundleIdentifier</key><string>cn.shierkeji.sixa.fixture</string><key>CFBundleShortVersionString</key><string>9.0.0</string></dict></plist>',
        0o644,
      ),
      entry(
        "SixaFixture.app/Contents/MacOS/sixa-fixture",
        "#!/bin/sh\nprintf 'SIXA_UPDATED_9.0.0\\n'\n",
        0o755,
      ),
      entry("SixaFixture.app/Contents/Resources/version.txt", "9.0.0", 0o644),
      Buffer.alloc(1024),
    ]),
  ),
);
try {
  run(["generate", "--ci", "--write-keys", key]);
  run(["sign", "--private-key-path", key, "--password", "", archive]);
  writeFileSync(join(directory, "mac-fixture.pub"), readFileSync(`${key}.pub`));
} finally {
  for (const file of [key, `${key}.pub`]) {
    try {
      unlinkSync(file);
    } catch {}
  }
  rmdirSync(temp);
}
console.log("离线 Mac 安装样本已生成；测试私钥已删除。");
