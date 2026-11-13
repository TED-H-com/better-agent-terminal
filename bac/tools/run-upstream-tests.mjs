import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { isMain, main } from './common.mjs';
export function testScripts(scripts) {
  const pipeline = scripts['verify:tauri-pre-ci'];
  if (!pipeline) throw new Error('Missing verify:tauri-pre-ci; review upstream test entrypoint');
  const names = [...new Set([...pipeline.matchAll(/\bpnpm\s+run\s+(test:[\w:-]+)/g)].map(m => m[1]).concat('test:sidecar'))];
  for (const name of names) if (!scripts[name]) throw new Error(`Missing test script: ${name}`);
  return names;
}
if (isMain(import.meta.url)) main(() => {
  const scripts = JSON.parse(readFileSync('package.json', 'utf8')).scripts;
  const names = testScripts(scripts);
  if (process.argv.includes('--list')) { console.log(names.join('\n')); return; }
  let failed = 0;
  const wrapperIndex = process.argv.indexOf('--wrapper');
  const wrapper = wrapperIndex === -1 ? null : process.argv[wrapperIndex + 1];
  if (wrapperIndex !== -1 && !wrapper) throw new Error('--wrapper requires an executable');
  for (const name of names) {
    console.log(`\n::group::${name}`);
    const start = Date.now();
    const result = spawnSync(wrapper || 'pnpm', wrapper ? [name] : ['run', name], { stdio: 'inherit' });
    const code = result.status ?? 1;
    console.log(`BAC_TEST_RESULT ${name} exit=${code} seconds=${((Date.now() - start) / 1000).toFixed(1)}`);
    console.log('::endgroup::');
    if (code) failed++;
  }
  console.log(`BAC_TEST_TOTAL pass=${names.length - failed} fail=${failed} skip=0`);
  if (failed) throw new Error(`${failed} upstream test scripts failed`);
});
