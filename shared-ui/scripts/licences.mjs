// The checker runs through npx so it is never part of the tree it audits (its own spdx and glob
// dependencies carry CC and BlueOak licences). MIT-0 is MIT without the attribution duty, needed by one
// jsdom transitive (@csstools/color-helpers).
// Licence gate for the shared packages and everything the consumer installs: any package whose licence is
// not on the allow list fails the run. The same allow list as deny.toml for Rust, minus the Rust-only ones.
import { spawnSync } from 'node:child_process';

const ALLOWED = ['MIT', 'Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause', 'ISC', '0BSD', 'Unicode-3.0', 'MIT-0'];

// Shipped = what a consumer installs from our packages (production dependency closure).
// Everything = also the build, test and showcase tooling.
const runs = [
  ['shipped packages (production closure)', ['--production']],
  ['everything installed (build, test, showcase tooling)', []],
];

let failed = false;
for (const [name, extra] of runs) {
  const r = spawnSync(
    'npx',
    ['--yes', 'license-checker-rseidelsohn@4.4.2', '--start', '.', ...extra, '--excludePrivatePackages', '--onlyAllow', ALLOWED.join(';'), '--summary'],
    { encoding: 'utf8' },
  );
  const out = `${r.stdout}${r.stderr}`.trim();
  console.log(`== ${name}: ${r.status === 0 ? 'ok' : 'FAILED'}`);
  console.log(out);
  if (r.status !== 0) failed = true;
}
process.exit(failed ? 1 : 0);
