// Real official updater -> NSIS -> newly installed executable, entirely offline.
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { spawn, spawnSync } from "node:child_process";
import { readFile, unlink } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
const [probe, installer, root] = process.argv
  .slice(2)
  .map((path) => resolve(path));
const key = join(root, "updater-test-key");
const cli = fileURLToPath(
  new URL("../ui/node_modules/@tauri-apps/cli/tauri.js", import.meta.url),
);
function sign(args) {
  const result = spawnSync(process.execPath, [cli, "signer", ...args], {
    input: "\n",
    encoding: "utf8",
    timeout: 15000,
    windowsHide: true,
  });
  assert.equal(result.status, 0, "Test key/signature generation failed");
}
const server = createServer();
try {
  sign(["generate", "--ci", "--write-keys", key]);
  sign(["sign", "--private-key-path", key, "--password", "", installer]);
  const bytes = await readFile(installer);
  const signature = (await readFile(`${installer}.sig`, "utf8")).trim();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const requested = new Set();
  server.on("request", (request, response) => {
    requested.add(request.url);
    const body =
      request.url === "/latest.json"
        ? Buffer.from(
            JSON.stringify({
              version: "9.0.0",
              url: `${base}/package.exe`,
              signature,
            }),
          )
        : request.url === "/package.exe"
          ? bytes
          : null;
    response.writeHead(body ? 200 : 404, {
      "Content-Length": body?.length ?? 0,
    });
    response.end(body);
  });
  const code = await new Promise((resolve, reject) => {
    const child = spawn(probe, [], {
      stdio: "ignore",
      windowsHide: true,
      env: {
        ...process.env,
        SIXA_UPDATER_PROBE_ROOT: root,
        SIXA_UPDATER_PROBE_URL: `${base}/latest.json`,
      },
    });
    const timer = setTimeout(() => {
      child.kill();
      reject(Error("Native updater timed out"));
    }, 30000);
    child.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.on("exit", (code) => {
      clearTimeout(timer);
      resolve(code);
    });
  });
  assert.equal(code, 0, "Official updater failed to launch installer");
  const marker = join(root, "native update 中文", "restarted.txt");
  const deadline = Date.now() + 60000;
  let version;
  do {
    version = await readFile(marker, "utf8").catch(() => "");
    if (version === "SIXA_UPDATED_9.0.0") break;
    await delay(200);
  } while (Date.now() < deadline);
  assert.equal(
    version,
    "SIXA_UPDATED_9.0.0",
    "Installer did not restart the newly installed executable",
  );
  assert.deepEqual([...requested].sort(), ["/latest.json", "/package.exe"]);
  assert.equal(
    await readFile(join(root, "user-data-preserved.txt"), "utf8"),
    "models tasks settings",
  );
  console.log(
    "PASS official updater: check, signed download, /P /UPDATE /R install, new version restart and data preserved",
  );
} finally {
  server.closeAllConnections();
  server.close();
  for (const file of [key, `${key}.pub`, `${installer}.sig`])
    await unlink(file).catch(() => {});
}
