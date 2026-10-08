import OSS from "ali-oss";
import { readFile, readdir, stat } from "node:fs/promises";
import { join } from "node:path";
import {
  compareVersions,
  sha256,
  stableVersion,
  baseUrl,
  verifySignature,
} from "./manifest.mjs";

const directory = process.argv[2];
if (
  !directory ||
  !process.env.OSS_ACCESS_KEY_ID ||
  !process.env.OSS_ACCESS_KEY_SECRET
)
  throw Error("缺少发布目录或 OSS 发布凭据");
const client = new OSS({
  region: "oss-cn-beijing",
  bucket: "tct12",
  secure: true,
  accessKeyId: process.env.OSS_ACCESS_KEY_ID,
  accessKeySecret: process.env.OSS_ACCESS_KEY_SECRET,
  timeout: 120000,
});
const prefix = "12box/sixa/updates";
const manifestBytes = await readFile(join(directory, "latest.json"));
const manifest = JSON.parse(manifestBytes);
stableVersion(manifest.version);
const config = JSON.parse(
  await readFile(
    new URL("../../src-tauri/tauri.conf.json", import.meta.url),
    "utf8",
  ),
);
if (manifest.version !== config.version)
  throw Error("清单版本与构建版本不一致");
for (const platform of ["windows-x86_64", "darwin-aarch64", "darwin-x86_64"]) {
  const item = manifest.platforms[platform];
  const expected = `${baseUrl}/releases/${manifest.version}/`;
  if (!item?.url?.startsWith(expected)) throw Error("平台更新包地址不正确");
  const name = item.url.slice(expected.length);
  if (!name || name.includes("/") || name.includes("%"))
    throw Error("更新包文件名不正确");
  verifySignature(
    await readFile(join(directory, name)),
    item.signature,
    config.plugins.updater.pubkey,
  );
}
async function head(key) {
  try {
    return (await client.head(key)).res.headers;
  } catch (error) {
    if (error.status === 404) return null;
    throw error;
  }
}
const stableKey = `${prefix}/stable/latest.json`;
const previous = await head(stableKey);
if (previous) {
  const current = JSON.parse(
    (await client.get(stableKey)).content.toString("utf8"),
  );
  if (compareVersions(manifest.version, current.version) < 0)
    throw Error("拒绝用旧版本覆盖稳定清单");
  if (
    manifest.version === current.version &&
    previous["x-oss-meta-sha256"] !== sha256(manifestBytes)
  )
    throw Error("同一稳定版本的清单已存在且内容不同，请发布新版本号");
}
const immutable = `${prefix}/releases/${manifest.version}`;
for (const name of (await readdir(directory)).sort()) {
  const file = join(directory, name);
  if (!(await stat(file)).isFile()) throw Error("发布目录中不能包含子目录");
  const bytes = await readFile(file);
  const digest = sha256(bytes);
  const key = `${immutable}/${name}`;
  const existing = await head(key);
  if (existing) {
    if (
      existing["x-oss-meta-sha256"] !== digest ||
      Number(existing["content-length"]) !== bytes.length
    )
      throw Error(`禁止覆盖已发布文件：${name}`);
  } else {
    await client.put(key, file, {
      headers: {
        "x-oss-forbid-overwrite": "true",
        "x-oss-meta-sha256": digest,
        "Cache-Control": "public, max-age=31536000, immutable",
      },
    });
  }
  const verified = await head(key);
  if (
    !verified ||
    verified["x-oss-meta-sha256"] !== digest ||
    Number(verified["content-length"]) !== bytes.length
  )
    throw Error(`OSS 对象校验失败：${name}`);
  console.log(`已核对 ${name}`);
}
// The workflow serializes publication across ALL tags. If-Match also protects
// against a concurrent operator changing the live manifest outside Actions.
const conditions = previous
  ? { "If-Match": previous.etag }
  : { "x-oss-forbid-overwrite": "true" };
await client.put(stableKey, manifestBytes, {
  mime: "application/json",
  headers: {
    ...conditions,
    "x-oss-meta-sha256": sha256(manifestBytes),
    "Cache-Control": "no-cache, max-age=0, must-revalidate",
  },
});
// Small public-read check only; never download installers or models from OSS.
const publicResponse = await fetch(`${baseUrl}/stable/latest.json`, {
  headers: { "Cache-Control": "no-cache" },
});
if (
  !publicResponse.ok ||
  sha256(Buffer.from(await publicResponse.arrayBuffer())) !==
    sha256(manifestBytes)
)
  throw Error("公开版本清单校验失败，请检查 OSS 公共读权限");
console.log(`已发布私匣 ${manifest.version}`);
