// Offline MCP protocol and native desktop smoke checks. Never install models or log in.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, mkdir, rm, writeFile, copyFile, readFile, chmod, stat } from 'node:fs/promises';
import { homedir } from 'node:os';
import { createConnection } from 'node:net';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createInterface } from 'node:readline';
import { setTimeout as delay } from 'node:timers/promises';

const children = new Set();
const toolNames = ['desensitization_status', 'desensitize_file', 'desensitize_batch',
  'get_desensitization_job', 'wait_desensitization_job', 'cancel_desensitization_job'];

function start(executable, args, env) {
  const child = spawn(resolve(executable), args, { env, stdio: ['pipe', 'pipe', 'pipe'] });
  child.on('error', error => { child.startError = error; });
  child.stderr.on('data', data => process.stderr.write(data));
  children.add(child);
  return child;
}

async function stop(child) {
  if (child.exitCode === null && child.signalCode === null) {
    const closed = once(child, 'close');
    child.kill('SIGKILL');
    await closed;
  }
  children.delete(child);
}

async function client(executable, env = process.env) {
  const child = start(executable, ['serve'], env);
  const pending = new Map();
  let id = 0;
  createInterface({ input: child.stdout }).on('line', line => {
    let message;
    try { message = JSON.parse(line); }
    catch { for (const item of pending.values()) item.reject(new Error(`Non-JSON MCP stdout: ${line}`)); return; }
    const item = pending.get(message.id);
    if (item) { pending.delete(message.id); item.resolve(message); }
  });
  child.on('close', () => {
    for (const item of pending.values()) item.reject(new Error('MCP exited before responding'));
  });
  function send(value) { child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', ...value })}\n`); }
  async function rpc(method, params = {}) {
    if (child.startError) throw child.startError;
    const request = ++id;
    let timer;
    try {
      return await new Promise((resolve, reject) => {
        timer = setTimeout(() => reject(new Error(`MCP timeout: ${method}`)), 20000);
        pending.set(request, { resolve, reject });
        send({ id: request, method, params });
      });
    } finally { clearTimeout(timer); pending.delete(request); }
  }
  const initialize = await rpc('initialize', {
    protocolVersion: '2025-06-18', capabilities: {}, clientInfo: { name: 'sixa-offline-smoke', version: '1' },
  });
  assert.equal(initialize.result.serverInfo.name, 'sixa');
  assert.equal(initialize.result.serverInfo.title, '私匣 · 本机文件脱敏');
  assert.match(initialize.result.instructions, /macOS/);
  send({ method: 'notifications/initialized' });
  const tools = (await rpc('tools/list')).result.tools;
  assert.deepEqual(tools.map(item => item.name).sort(), [...toolNames].sort());
  for (const tool of tools) {
    assert.match(tool.title, /[\u4e00-\u9fff]/);
    assert.ok(tool.description && tool.outputSchema && tool.annotations);
  }
  return {
    child,
    call: async (name, args = {}) => {
      const response = await rpc('tools/call', { name, arguments: args });
      assert.equal(response.error, undefined);
      assert.ok(response.result.structuredContent);
      return response.result;
    },
    async close() {
      const closed = once(child, 'close');
      child.stdin.end();
      const exited = await Promise.race([closed.then(() => true), delay(3000).then(() => false)]);
      assert.ok(exited, 'MCP must exit when its client disconnects');
      assert.equal(child.exitCode, 0);
      children.delete(child);
    },
  };
}

let root;
const launchedHosts = [];
let packagedExecutable;
try {
  const [mode, executable, sidecar] = process.argv.slice(2);
  if (mode === '--metadata') {
    const mcp = await client(executable);
    await mcp.close();
    console.log('PASS packaged MCP initialization, Chinese metadata, six tools, clean exit');
  } else if (mode === '--packaged' && process.platform === 'darwin') {
    assert.equal(process.env.CI, 'true', 'Packaged smoke uses an ephemeral CI user only');
    packagedExecutable = resolve(executable);
    const mcp = await client(sidecar);
    // The release sidecar must launch its own signed application bundle through LaunchServices.
    const status = await mcp.call('desensitization_status');
    assert.equal(status.isError ?? false, false, JSON.stringify(status.structuredContent));
    assert.equal(status.structuredContent.authenticated, false);
    assert.equal(status.structuredContent.models_ready, false);
    assert.equal(status.structuredContent.ready, false);
    root = await mkdtemp('/private/tmp/sixa-package-');
    const source = join(root, '中文 文件.txt');
    await writeFile(source, 'offline authorization check');
    const rejected = await mcp.call('desensitize_file', { source_path: source });
    assert.equal(rejected.structuredContent.code, 'AUTH_REQUIRED');
    assert.ok(rejected.structuredContent.recovery_action);
    await mcp.close();
    const reconnect = await client(sidecar);
    assert.equal((await reconnect.call('desensitization_status')).isError ?? false, false);
    await reconnect.close();
    const secrets = join(homedir(), 'Library/Application Support/LocalDesensitization/secrets');
    const keyPath = join(secrets, 'internal-key-v1');
    const key = await readFile(keyPath);
    assert.equal(key.length, 32);
    assert.equal((await stat(secrets)).mode & 0o777, 0o700);
    assert.equal((await stat(keyPath)).mode & 0o777, 0o600);
    assert.equal((await stat(keyPath)).uid, process.getuid());
    // Restart the final installed app with Keychain locked. Local keys and the
    // encrypted login state must suffice; no system authorization is allowed.
    execFileSync('/usr/bin/security', ['lock-keychain']);
    const listing = execFileSync('/bin/ps', ['-axo', 'pid=,comm='], { encoding: 'utf8' });
    for (const line of listing.split('\n')) {
      const match = /^\s*(\d+)\s+(.+)$/.exec(line);
      if (match && match[2].trim() === packagedExecutable) process.kill(Number(match[1]), 'SIGKILL');
    }
    await delay(500);
    execFileSync('/usr/bin/open', ['-a', resolve(packagedExecutable, '../../..')]);
    const restarted = await client(sidecar);
    const deadline = Date.now() + 20000;
    let ready = false;
    while (Date.now() < deadline) {
      const result = await restarted.call('desensitization_status');
      if (!result.isError) { ready = true; break; }
      await delay(200);
    }
    assert.ok(ready, 'installed app must restart even with Keychain locked');
    assert.deepEqual(await readFile(keyPath), key, 'restart must preserve the master key');
    key.fill(0);
    await restarted.close();
    console.log('PASS final signed app: auto-launch, desktop communication, login guidance, reconnect, private key permissions and restart with locked Keychain; no model downloads');
  } else if (mode === '--desktop' && process.platform === 'darwin') {
    root = await mkdtemp('/private/tmp/sixa-mcp-');
    const env = { ...process.env, SIXA_DESKTOP_TEST_ROOT: join(root, 'profile'),
      SIXA_DESKTOP_TEST_NO_UI: '1', SIXA_MCP_TEST_DIRECTORY: join(root, 'ipc') };
    await mkdir(env.SIXA_DESKTOP_TEST_ROOT);
    await writeFile(join(root, '中文 空格.pdf'), 'offline fixture; no document analysis is authorized');
    await writeFile(join(root, 'one.pdf'), 'offline fixture');
    let desktop = start(executable, [], env);
    const mcp = await client(sidecar, env);
    async function waitDesktop() {
      const until = Date.now() + 30000;
      while (Date.now() < until) {
        assert.equal(desktop.exitCode, null, 'desktop exited during startup');
        const result = await mcp.call('desensitization_status');
        if (!result.isError) {
          assert.equal(result.structuredContent.authenticated, false);
          assert.equal(result.structuredContent.ready, false);
          assert.equal(result.structuredContent.models_ready, false);
          return;
        }
        await delay(200);
      }
      throw new Error('Native desktop did not accept MCP status');
    }
    await waitDesktop();
    for (const [name, args, expected] of [
      ['desensitize_file', { source_path: join(root, '中文 空格.pdf') }, 'AUTH_REQUIRED'],
      ['desensitize_batch', { source_paths: [join(root, 'one.pdf')] }, 'AUTH_REQUIRED'],
      ['desensitize_file', { source_path: 'relative.pdf' }, 'INVALID_PATH'],
      ...toolNames.slice(3).map(name => [name, { job_id: '11111111-1111-4111-8111-111111111111' }, 'JOB_NOT_FOUND']),
    ]) {
      const result = await mcp.call(name, args);
      assert.equal(result.isError, true);
      assert.equal(result.structuredContent.code, expected);
      assert.ok(result.structuredContent.recovery_action);
    }
    // A hostile oversized frame must not terminate the listener or allocate a large body.
    const socket = createConnection(join(env.SIXA_MCP_TEST_DIRECTORY, 'mcp.sock'));
    await once(socket, 'connect');
    const closed = once(socket, 'close');
    socket.write(Buffer.from([1, 0, 16, 0]));
    await Promise.race([closed, delay(3000).then(() => { throw new Error('Oversized frame was not rejected'); })]);
    await waitDesktop();
    // Abrupt exit leaves a stale socket. Restart must recover it; the same MCP stays connected.
    await stop(desktop);
    desktop = start(executable, [], env);
    await waitDesktop();
    await mcp.close();
    await stop(desktop);
    console.log('PASS stdio → socket → desktop, auth gates, six tools, malformed frame, stale socket and reconnect');
  } else if (mode === '--launch' && process.platform === 'darwin') {
    root = await mkdtemp('/private/tmp/sixa-launch-');
    const quote = value => `'${value.replaceAll("'", "'\\''")}'`;
    for (const behavior of ['ready', 'hang', 'blocked']) {
      const directory = join(root, behavior);
      const app = join(directory, '私匣 测试.app');
      const macos = join(app, 'Contents', 'MacOS');
      const ipc = join(directory, 'ipc');
      const starts = join(directory, 'starts');
      launchedHosts.push(starts);
      await mkdir(macos, { recursive: true });
      await mkdir(ipc, { mode: 0o700 });
      if (behavior === 'blocked') {
        await writeFile(join(ipc, 'startup.lock'), 'pending', { mode: 0o600 });
      }
      const bundledMcp = join(macos, 'sixa-mcp');
      await copyFile(executable, bundledMcp);
      await chmod(bundledMcp, 0o755);
      await writeFile(join(app, 'Contents', 'Info.plist'), `<?xml version="1.0" encoding="UTF-8"?>
        <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
        <plist version="1.0"><dict><key>CFBundleExecutable</key><string>McpHost</string>
        <key>CFBundleIdentifier</key><string>cn.shierkeji.sixa.test.${behavior}.${process.pid}</string>
        <key>CFBundleName</key><string>私匣 MCP 测试</string>
        <key>CFBundlePackageType</key><string>APPL</string><key>LSUIElement</key><true/></dict></plist>`);
      const fixture = fileURLToPath(new URL('./mcp-fixture.mjs', import.meta.url));
      const launch = `#!/bin/sh\nexec ${[process.execPath, fixture, join(ipc, 'mcp.sock'), starts, behavior].map(quote).join(' ')}\n`;
      await writeFile(join(macos, 'McpHost'), launch, { mode: 0o755 });
      const env = { ...process.env, SIXA_MCP_TEST_DIRECTORY: ipc };
      const mcp = await client(bundledMcp, env);
      await assert.rejects(readFile(starts), { code: 'ENOENT' }, 'initialize must not open the desktop');
      if (behavior === 'blocked') {
        for (let attempt = 0; attempt < 3; attempt++) {
          // Independent client processes must respect the persisted failure gate.
          const probe = await client(bundledMcp, env);
          const result = await probe.call('desensitization_status');
          assert.equal(result.structuredContent.code, 'STARTUP_BLOCKED');
          assert.equal(result.structuredContent.retryable, false);
          assert.match(result.structuredContent.recovery_action, /手动打开/);
          await probe.close();
        }
        await assert.rejects(readFile(starts), { code: 'ENOENT' }, 'blocked startup must never relaunch or prompt');
      } else if (behavior === 'ready') {
        const second = await client(bundledMcp, env);
        const statuses = await Promise.all([mcp.call('desensitization_status'), second.call('desensitization_status')]);
        for (const result of statuses) {
          assert.equal(result.isError ?? false, false, JSON.stringify(result.structuredContent));
          assert.equal(result.structuredContent.authenticated, false);
          assert.equal(result.structuredContent.ready, false);
          assert.ok(result.structuredContent.next_action);
        }
        assert.equal((await readFile(starts, 'utf8')).trim().split('\n').length, 1, 'concurrent clients launched more than one host');
        await second.close();
      } else {
        const before = Date.now();
        const result = await mcp.call('desensitization_status');
        assert.equal(result.structuredContent.code, 'APP_NOT_RUNNING');
        assert.match(result.structuredContent.message, /15 秒/);
        assert.ok(Date.now() - before >= 14000, '5-second RPC timeout incorrectly consumed startup time');
        const retry = Date.now();
        const cooldown = await mcp.call('desensitization_status');
        assert.equal(cooldown.structuredContent.code, 'APP_NOT_RUNNING');
        assert.ok(Date.now() - retry < 3000, 'failed launch did not enter cooldown');
        assert.equal((await readFile(starts, 'utf8')).trim().split('\n').length, 1);
      }
      await mcp.close();
    }
    console.log('PASS LaunchServices auto-start, Chinese/space bundle path, concurrent calls, unauthenticated guidance, startup timeout, cooldown and persistent failure gate');
  } else {
    throw new Error('Usage: --metadata <mcp> | --desktop <debug-desktop> <debug-mcp> | --launch <debug-mcp> (macOS)');
  }
} finally {
  if (packagedExecutable) {
    const listing = execFileSync('/bin/ps', ['-axo', 'pid=,comm='], { encoding: 'utf8' });
    for (const line of listing.split('\n')) {
      const match = /^\s*(\d+)\s+(.+)$/.exec(line);
      if (match && match[2].trim() === packagedExecutable) {
        try { process.kill(Number(match[1]), 'SIGKILL'); } catch { /* already exited */ }
      }
    }
  }
  for (const child of children) await stop(child);
  for (const starts of launchedHosts) {
    const pids = await readFile(starts, 'utf8').catch(() => '');
    for (const pid of pids.trim().split('\n').filter(Boolean)) {
      try { process.kill(Number(pid), 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
    }
  }
  if (root) await rm(root, { recursive: true, force: true });
}
