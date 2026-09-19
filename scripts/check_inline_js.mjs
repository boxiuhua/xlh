// 用法:node scripts/check_inline_js.mjs <Rust 源文件>...
// 抽出源文件中每个 <script>…</script> 块,用 vm.Script 做语法检查(不执行)。
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

let failed = false;
for (const file of process.argv.slice(2)) {
  const src = readFileSync(file, 'utf8');
  const re = /<script>([\s\S]*?)<\/script>/g;
  let m, n = 0;
  while ((m = re.exec(src)) !== null) {
    n += 1;
    try {
      new vm.Script(m[1], { filename: `${file}#script${n}` });
    } catch (e) {
      failed = true;
      console.error(`${file} 第 ${n} 个 <script> 语法错误: ${e.message}`);
    }
  }
  if (n === 0) {
    failed = true;
    console.error(`${file} 中没有找到 <script> 块`);
  }
  console.log(`${file}: 检查了 ${n} 个 <script> 块`);
}
process.exit(failed ? 1 : 0);
