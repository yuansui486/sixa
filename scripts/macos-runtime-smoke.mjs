// Exercise the signing restriction that unsigned inference probes cannot catch.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, copyFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

assert.equal(process.platform, 'darwin');
const [mode, executable, library, entitlements] = process.argv.slice(2);
function run(file, args, expected = 0) {
  const result = spawnSync(file, args, { encoding: 'utf8', timeout: 30000 });
  assert.ifError(result.error); // A loader deadlock must fail within the bound.
  assert.equal(result.status, expected, `${result.stdout}\n${result.stderr}`);
  return `${result.stdout}\n${result.stderr}`;
}
if (mode === 'desktop') {
  assert.match(run(resolve(executable), ['--check-inference-runtime']), /SIXA_RUNTIME_READY/);
  console.log('PASS final hardened desktop executable loads its bundled ONNX Runtime');
} else if (mode === 'inference') {
  const directory = mkdtempSync(join(tmpdir(), 'sixa-runtime-signing-'));
  try {
    const probe = join(directory, 'probe');
    copyFileSync(executable, probe);
    run('/usr/bin/codesign', ['--force', '--sign', '-', '--options', 'runtime', '--timestamp=none', probe]);
    const denied = spawnSync(probe, [resolve(library)], { encoding: 'utf8', timeout: 30000 });
    assert.ifError(denied.error);
    if (denied.status === 1) {
      assert.match(denied.stderr, /Team ID|library validation|code signature/i);
      console.log('PASS native library-validation denial returns promptly');
    } else {
      // Hosted runners may relax AMFI, unlike customers' Macs. Never label
      // an allowed load as a successful denial test; positive checks still run.
      assert.equal(denied.status, 0, denied.stderr);
      assert.match(denied.stdout, /"passed":true/);
      console.log('INFO runner allows ad-hoc libraries without entitlement; signing-denial check unavailable here');
    }
    run('/usr/bin/codesign', ['--force', '--sign', '-', '--options', 'runtime', '--timestamp=none', '--entitlements', resolve(entitlements), probe]);
    assert.match(run(probe, [resolve(library), '--retry-after-invalid']), /"passed":true/);
    console.log('PASS hardened inference: missing/corrupt runtime can retry; NER and OCR inference succeed with app entitlements');
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
} else {
  throw Error('Expected desktop or inference mode');
}
