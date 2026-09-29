// Offline stand-in for testing LaunchServices + MCP; never part of an app bundle/release.
import { appendFileSync, chmodSync } from 'node:fs';
import { createServer } from 'node:net';

const [endpoint, starts, behavior] = process.argv.slice(2);
appendFileSync(starts, `${process.pid}\n`);
setTimeout(() => process.exit(0), 60000).unref();
if (behavior === 'hang') {
  setTimeout(() => process.exit(0), 45000);
} else {
  const server = createServer(socket => {
    let buffer = Buffer.alloc(0);
    socket.on('error', () => {});
    socket.on('data', chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= 4 && buffer.length >= buffer.readUInt32LE(0) + 4) {
        const length = buffer.readUInt32LE(0);
        if (length > 1024 * 1024) { socket.destroy(); return; }
        const request = JSON.parse(buffer.subarray(4, length + 4));
        buffer = buffer.subarray(length + 4);
        const result = { authenticated: false, authorization_valid: false, models_ready: false,
          supported_formats: ['pdf'], protocol_version: 1 };
        const response = Buffer.from(JSON.stringify({ version: 1, id: request.id, ok: true, result }));
        const prefix = Buffer.alloc(4);
        prefix.writeUInt32LE(response.length);
        socket.write(Buffer.concat([prefix, response]));
      }
    });
  });
  server.listen(endpoint, () => chmodSync(endpoint, 0o600));
}
