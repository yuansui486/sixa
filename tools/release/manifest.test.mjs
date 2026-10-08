import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile, mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  prepare,
  verifySignature,
  compareVersions,
  stableVersion,
  baseUrl,
} from "./manifest.mjs";
const fixture = new URL(
  "../../src-tauri/src/fixtures/updater/",
  import.meta.url,
);
const bytes = await readFile(new URL("package.txt", fixture));
const signature = await readFile(new URL("package.txt.sig", fixture), "utf8");
const publicKey = await readFile(new URL("public.key", fixture), "utf8");
test("Tauri 签名兼容，拒绝损坏包和被篡改的可信注释", () => {
  verifySignature(bytes, signature, publicKey);
  assert.throws(() =>
    verifySignature(
      Buffer.concat([bytes, Buffer.from("tampered")]),
      signature,
      publicKey,
    ),
  );
  const changed = Buffer.from(
    Buffer.from(signature, "base64")
      .toString()
      .replace(/^trusted comment: /m, "trusted comment: forged "),
  ).toString("base64");
  assert.throws(() => verifySignature(bytes, changed, publicKey));
});
test("版本比较不会把 1.0.10 当成 1.0.9 之前的版本", () => {
  assert.equal(compareVersions("1.0.10", "1.0.9"), 1);
  assert.equal(compareVersions("1.0.9", "1.0.9"), 0);
  for (const value of ["v1.0.9", "1.0.9-beta", "../evil", "1.00.9"])
    assert.throws(() => stableVersion(value));
});
test("三个平台产物完整且签名正确才生成清单；输出可重复", async () => {
  const root = await mkdtemp(join(tmpdir(), "sixa-manifest-"));
  try {
    for (const [artifact, suffixes] of [
      ["sixa-windows-x64", [".exe"]],
      ["sixa-macos-arm64", [".app.tar.gz", ".dmg"]],
      ["sixa-macos-x64", [".app.tar.gz", ".dmg"]],
    ]) {
      const directory = join(root, artifact);
      await mkdir(directory);
      for (const suffix of suffixes) {
        const name = join(directory, `Sixa_1.0.9_test${suffix}`);
        await writeFile(name, bytes);
        if (suffix !== ".dmg") await writeFile(`${name}.sig`, signature);
      }
    }
    const options = {
      source: root,
      version: "1.0.9",
      publicKey,
      notes: "中文更新说明",
      date: "2026-10-08T01:00:00Z",
    };
    const manifest = await prepare({
      ...options,
      output: join(root, "ready1"),
    });
    await prepare({ ...options, output: join(root, "ready2") });
    assert.deepEqual(Object.keys(manifest.platforms).sort(), [
      "darwin-aarch64",
      "darwin-x86_64",
      "windows-x86_64",
    ]);
    assert.equal(
      manifest.platforms["darwin-aarch64"].url,
      `${baseUrl}/releases/1.0.9/Sixa_1.0.9_macos_arm64.app.tar.gz`,
    );
    assert.equal(
      await readFile(join(root, "ready1", "SHA256SUMS.txt"), "utf8"),
      await readFile(join(root, "ready2", "SHA256SUMS.txt"), "utf8"),
    );
    await writeFile(
      join(root, "sixa-macos-arm64", "Sixa_1.0.9_test.app.tar.gz"),
      "broken",
    );
    await assert.rejects(
      prepare({ ...options, output: join(root, "bad") }),
      /签名/,
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
