// Offline MCP protocol and native desktop smoke checks. Never install models or log in.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, mkdir, rm, writeFile, copyFile, readFile, chmod } from 'node:fs/promises';
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
try {
  const [mode, executable, sidecar] = process.argv.slice(2);
  if (mode === '--metadata') {
    const mcp = await client(executable);
    await mcp.close();
    console.log('PASS packaged MCP initialization, Chinese metadata, six tools, clean exit');
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
    for (const behavior of ['ready', 'hang']) {
      const directory = join(root, behavior);
      const app = join(directory, '私匣 测试.app');
      const macos = join(app, 'Contents', 'MacOS');
      const ipc = join(directory, 'ipc');
      const starts = join(directory, 'starts');
      launchedHosts.push(starts);
      await mkdir(macos, { recursive: true });
      await mkdir(ipc, { mode: 0o700 });
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
      if (behavior === 'ready') {
        const second = await client(bundledMcp, env);
        const statuses = await Promise.all([mcp.call('desensitization_status'), second.call('desensitization_status')]);
        for (const result of statuses) {
          assert.equal(result.isError ?? false, false);
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
    console.log('PASS LaunchServices auto-start, Chinese/space bundle path, concurrent calls, unauthenticated guidance, startup timeout and cooldown');
  } else {
    throw new Error('Usage: --metadata <mcp> | --desktop <debug-desktop> <debug-mcp> | --launch <debug-mcp> (macOS)');
  }
} finally {
  for (const child of children) await stop(child);
  for (const starts of launchedHosts) {
    const pids = await readFile(starts, 'utf8').catch(() => '');
    for (const pid of pids.trim().split('\n').filter(Boolean)) {
      try { process.kill(Number(pid), 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
    }
  }
  if (root) await rm(root, { recursive: true, force: true });
}
