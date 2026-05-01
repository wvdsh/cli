// In dev mode the CLI launches `npx electron .`, so macOS reads
// CFBundleName/CFBundleDisplayName from node_modules/electron/dist/Electron.app/Contents/Info.plist
// for the dock tooltip and menu bar — neither app.setName() nor productName
// in package.json overrides those at runtime. Packaged builds aren't
// affected: electron-builder writes the right plist into the .app bundle.
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'darwin') process.exit(0);

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const plist = resolve(
  root,
  'node_modules/electron/dist/Electron.app/Contents/Info.plist'
);
if (!existsSync(plist)) process.exit(0);

const NAME = 'Wavedash Dev';
for (const key of ['CFBundleName', 'CFBundleDisplayName']) {
  execFileSync('/usr/bin/plutil', ['-replace', key, '-string', NAME, plist]);
}
console.log(`patched ${plist} → ${NAME}`);
