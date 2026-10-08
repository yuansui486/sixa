import {
  createHash,
  createPublicKey,
  timingSafeEqual,
  verify,
} from "node:crypto";
import {
  readFile,
  readdir,
  mkdir,
  copyFile,
  writeFile,
} from "node:fs/promises";
import { resolve, join, basename } from "node:path";
import { fileURLToPath } from "node:url";

export const baseUrl =
  "https://tct12.oss-cn-beijing.aliyuncs.com/12box/sixa/updates";
export function stableVersion(value) {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(value))
    throw Error(`不是稳定版本号：${value}`);
  return value;
}
export function compareVersions(a, b) {
  const x = stableVersion(a).split(".").map(BigInt),
    y = stableVersion(b).split(".").map(BigInt);
  for (let i = 0; i < 3; i++) {
    if (x[i] !== y[i]) return x[i] > y[i] ? 1 : -1;
  }
  return 0;
}
export const sha256 = (bytes) =>
  createHash("sha256").update(bytes).digest("hex");
export function verifySignature(bytes, signature, publicKey) {
  const keyLines = Buffer.from(publicKey.trim(), "base64")
    .toString("utf8")
    .trim()
    .split(/\r?\n/);
  const sigLines = Buffer.from(signature.trim(), "base64")
    .toString("utf8")
    .trim()
    .split(/\r?\n/);
  const key = Buffer.from(keyLines[1] ?? "", "base64"),
    sig = Buffer.from(sigLines[1] ?? "", "base64");
  if (
    key.length !== 42 ||
    sig.length !== 74 ||
    !sigLines[2]?.startsWith("trusted comment: ") ||
    !timingSafeEqual(key.subarray(2, 10), sig.subarray(2, 10))
  )
    throw Error("更新签名格式或密钥不匹配");
  const algorithm = sig.subarray(0, 2).toString("ascii");
  if (!["ED", "Ed"].includes(algorithm)) throw Error("不支持的更新签名算法");
  const pub = createPublicKey({
    key: Buffer.concat([
      Buffer.from("302a300506032b6570032100", "hex"),
      key.subarray(10),
    ]),
    format: "der",
    type: "spki",
  });
  const payload =
    algorithm === "ED"
      ? createHash("blake2b512").update(bytes).digest()
      : bytes;
  if (
    !verify(null, payload, pub, sig.subarray(10)) ||
    !verify(
      null,
      Buffer.concat([sig.subarray(10), Buffer.from(sigLines[2].slice(17))]),
      pub,
      Buffer.from(sigLines[3] ?? "", "base64"),
    )
  )
    throw Error("更新包签名校验失败");
}
async function files(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  return (
    await Promise.all(
      entries.map((entry) =>
        entry.isDirectory()
          ? files(join(directory, entry.name))
          : entry.isFile()
            ? [join(directory, entry.name)]
            : [],
      ),
    )
  ).flat();
}
function one(paths, suffix) {
  const matches = paths.filter((path) => path.endsWith(suffix));
  if (matches.length !== 1)
    throw Error(`需要且只能有一个 ${suffix} 文件，找到 ${matches.length} 个`);
  return matches[0];
}
export async function prepare({
  source,
  output,
  version,
  publicKey,
  notes,
  date,
}) {
  stableVersion(version);
  if (!Number.isFinite(Date.parse(date))) throw Error("缺少提交时间");
  await mkdir(output, { recursive: true });
  if ((await readdir(output)).length)
    throw Error("发布输出目录必须为空，避免混入旧安装包");
  const platforms = {};
  for (const [artifact, platform, label, suffix] of [
    ["sixa-windows-x64", "windows-x86_64", "windows_x64_setup", ".exe"],
    ["sixa-macos-arm64", "darwin-aarch64", "macos_arm64", ".app.tar.gz"],
    ["sixa-macos-x64", "darwin-x86_64", "macos_x64", ".app.tar.gz"],
  ]) {
    const paths = await files(join(source, artifact));
    const bundle = one(paths, suffix);
    if (suffix === ".exe" && !basename(bundle).includes(`_${version}_`))
      throw Error("Windows 安装程序版本与标签不一致");
    const signature = (await readFile(`${bundle}.sig`, "utf8")).trim();
    verifySignature(await readFile(bundle), signature, publicKey);
    const name = `Sixa_${version}_${label}${suffix}`;
    await copyFile(bundle, join(output, name));
    await copyFile(`${bundle}.sig`, join(output, `${name}.sig`));
    platforms[platform] = {
      url: `${baseUrl}/releases/${version}/${name}`,
      signature,
    };
    if (platform.startsWith("darwin")) {
      const dmg = one(paths, ".dmg");
      if (!basename(dmg).includes(`_${version}_`))
        throw Error("Mac 安装程序版本与标签不一致");
      await copyFile(dmg, join(output, `Sixa_${version}_${label}.dmg`));
    }
  }
  const manifest = {
    version,
    notes: notes.trim(),
    pub_date: new Date(date).toISOString(),
    platforms,
  };
  await writeFile(
    join(output, "latest.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
  const sums = [];
  for (const name of (await readdir(output)).sort())
    sums.push(`${sha256(await readFile(join(output, name)))}  ${name}`);
  await writeFile(join(output, "SHA256SUMS.txt"), `${sums.join("\n")}\n`);
  return manifest;
}
if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const [source, output, version] = process.argv.slice(2);
  const config = JSON.parse(
    await readFile(
      new URL("../../src-tauri/tauri.conf.json", import.meta.url),
      "utf8",
    ),
  );
  if (version !== config.version) throw Error("标签版本与应用配置不同");
  await prepare({
    source,
    output,
    version,
    publicKey: config.plugins.updater.pubkey,
    notes: await readFile(
      new URL(
        `../../docs/releases/${stableVersion(version)}.md`,
        import.meta.url,
      ),
      "utf8",
    ),
    date: process.env.RELEASE_DATE,
  });
}
