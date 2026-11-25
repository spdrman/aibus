// Cross-consumer check for iasbuilt/webapp, runnable on a laptop with just a webapp checkout.
//
//   node scripts/webapp-consumer-check.mjs /path/to/webapp
//
// Packs both packages, installs the tarballs into a scratch project that uses the webapp's own React,
// @types/react and TypeScript versions and its tsconfig compiler options, type-checks a probe the way
// the webapp would import the primitives, then renders it with react-dom/server. Fails on any type error
// or on markup that lost a role or label. The webapp's source is read, never changed.
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const webapp = resolve(process.argv[2] ?? '');
if (!process.argv[2]) throw new Error('usage: webapp-consumer-check.mjs <path to webapp checkout>');
const root = resolve(new URL('..', import.meta.url).pathname);
const pkg = JSON.parse(readFileSync(join(webapp, 'package.json'), 'utf8'));
const strip = (s) => s.replace(/^[\^~]/, '');
const tsconfig = JSON.parse(readFileSync(join(webapp, 'tsconfig.json'), 'utf8').replace(/^\s*\/\/.*$/gm, '').replace(/,(\s*[}\]])/g, '$1'));

const dir = mkdtempSync(join(tmpdir(), 'webapp-consumer-'));
const run = (cmd, args, cwd) => execFileSync(cmd, args, { cwd, stdio: 'inherit' });

const tarballs = ['design-tokens', 'react-primitives'].map((p) => {
  const out = execFileSync('npm', ['pack', '--silent', '--pack-destination', dir], { cwd: join(root, 'packages', p), encoding: 'utf8' });
  return join(dir, out.trim().split('\n').pop());
});

writeFileSync(
  join(dir, 'package.json'),
  JSON.stringify({
    name: 'webapp-consumer-probe',
    private: true,
    type: 'module',
    dependencies: {
      react: pkg.dependencies.react,
      'react-dom': pkg.dependencies['react-dom'],
      '@types/react': pkg.devDependencies['@types/react'],
      '@types/react-dom': pkg.devDependencies['@types/react-dom'],
      typescript: pkg.dependencies.typescript,
      tsx: '4.20.6',
      '@types/node': pkg.devDependencies['@types/node'],
    },
  }),
);
run('npm', ['install', '--ignore-scripts', '--no-audit', '--no-fund', ...tarballs], dir);

const { paths, baseUrl, outDir, ...options } = tsconfig.compilerOptions;
writeFileSync(join(dir, 'tsconfig.json'), JSON.stringify({ compilerOptions: { ...options, allowImportingTsExtensions: true, types: ['node'] }, include: ['probe.tsx'] }));
writeFileSync(
  join(dir, 'probe.tsx'),
  `import { renderToString } from 'react-dom/server';
import { Button, Checkbox, Dialog, FormRow, Tabs, TextBox } from '@sharedui/react-primitives';
import { colorTokens } from '@sharedui/design-tokens';

const html = renderToString(
  <>
    <Button variant="danger" size="sm">Delete</Button>
    <TextBox label="Asset name" helperText="Shown in lists" />
    <Checkbox label="Active" />
    <FormRow label="Owner">{(id) => <input id={id} />}</FormRow>
    <Tabs label="Asset views" items={[{ id: 'a', label: 'Details', content: 'x' }, { id: 'b', label: 'History', content: 'y' }]} />
    <Dialog open={false} onClose={() => {}} title="Unused">closed</Dialog>
  </>,
);
for (const needle of ['role="tablist"', 'for="', 'type="checkbox"', 'ui-button--danger']) {
  if (!html.includes(needle)) throw new Error('missing ' + needle + ' in ' + html);
}
if (String(colorTokens.light['text-primary']) === String(colorTokens.dark['text-primary'])) throw new Error('themes identical');
console.log('webapp consumer probe ok');
`,
);
run('npx', ['tsc', '-p', 'tsconfig.json', '--noEmit'], dir);
run('npx', ['tsx', 'probe.tsx'], dir);
console.log(`webapp consumer check passed (react ${pkg.dependencies.react}, typescript ${pkg.dependencies.typescript}, scratch ${dir})`);
