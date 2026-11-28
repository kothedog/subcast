// Lists interface strings and checks the dictionaries.
//   node scripts/i18n.mjs            report unwrapped Chinese strings and missing / unused entries
//   node scripts/i18n.mjs --keys     print every key as JSON (for translating)
//   node scripts/i18n.mjs --strict   exit 1 when anything is missing (for CI)
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const root = new URL("..", import.meta.url).pathname;
const CJK = /[\u3400-\u9fff]/;
const walk = (dir) => (existsSync(dir) ? readdirSync(dir).flatMap((f) => { const p = join(dir, f); return statSync(p).isDirectory() ? walk(p) : [p]; }) : []);
const front = walk(join(root, "src")).filter((f) => /\.(tsx?)$/.test(f) && !/\.test\.ts$/.test(f) && !f.includes("/locales/"));
const unescape = (s) => s.replace(/\\n/g, "\n").replace(/\\"/g, '"').replace(/\\`/g, "`").replace(/\\\\/g, "\\");

const keys = new Map(); // key -> where
const add = (k, where) => { if (k && CJK.test(k) && !keys.has(k)) keys.set(k, where); };
// Curated Chinese show names are only shown in Chinese interfaces, so only 繁體 needs them.
const zhOnly = new Set();
const unwrapped = [];

for (const file of front) {
  const rel = relative(root, file);
  const src = readFileSync(file, "utf8");
  for (const m of src.matchAll(/\b(?:t|tk)\(\s*(?:"((?:[^"\\\n]|\\.)*)"|`((?:[^`\\$]|\\.)*)`)/g)) add(unescape(m[1] ?? m[2]), rel);
  // tx("context", "源文") looks up "源文@context" first, so both keys are needed.
  for (const m of src.matchAll(/\btx\(\s*"(\w+)",\s*"((?:[^"\\\n]|\\.)*)"/g)) { add(unescape(m[2]), rel); add(`${unescape(m[2])}@${m[1]}`, rel); }
  if (rel.endsWith("lib/picks.ts")) {
    for (const m of src.matchAll(/\bintro: "([^"]+)"/g)) add(m[1], rel);
    for (const m of src.matchAll(/\bzh: "([^"]+)"/g)) if (!keys.has(m[1])) { add(m[1], rel); zhOnly.add(m[1]); }
    continue;
  }
  let skipping = false; // between "// i18n-ignore-start" and "// i18n-ignore-end" (prompts sent to models)
  src.split("\n").forEach((line, i) => {
    if (line.includes("i18n-ignore-start")) skipping = true;
    if (line.includes("i18n-ignore-end")) { skipping = false; return; }
    const code = line.replace(/\/\/.*$/, "");
    if (skipping || /^\s*(\*|\/\*)/.test(line) || line.includes("i18n-ignore") || !CJK.test(code)) return;
    const stripped = code.replace(/\btx\(\s*"\w+",\s*(?=["`])/g, "t(").replace(/\b(?:t|tk)\(\s*(?:"(?:[^"\\]|\\.)*"|`(?:[^`\\]|\\.)*`)/g, "");
    if (CJK.test(stripped.replace(/\/[^/\n]*[\u3400-\u9fff][^/\n]*\/[gimsuy]*/g, ""))) unwrapped.push(`${rel}:${i + 1}: ${line.trim().slice(0, 140)}`);
  });
}

// Rust messages that reach the screen. Tests, SQL, regexes and proper names stay out.
const RUST_SKIP = /INSERT |^章节：\{\}$|^\[\\w|\(\?i\)|感谢观看|谢谢观看|请不吝点赞|字幕by|字幕由|^ · 文稿$/;
const rustFiles = [
  ...walk(join(root, "src-tauri/src")),
  ...walk(join(root, "crates/subcast-core/src")),
].filter((f) => f.endsWith(".rs"));
for (const file of rustFiles) {
  const src = readFileSync(file, "utf8").split("#[cfg(test)]")[0];
  let skipping = false;
  for (const line of src.split("\n")) {
    if (line.includes("i18n-ignore-start")) skipping = true;
    if (line.includes("i18n-ignore-end")) { skipping = false; continue; }
    if (skipping || /^\s*\/\//.test(line) || /title_for|cover_for|=> "/.test(line) || line.includes("i18n-ignore")) continue;
    for (const m of line.matchAll(/"((?:[^"\\]|\\.)*[\u3400-\u9fff](?:[^"\\]|\\.)*)"/g)) if (!RUST_SKIP.test(m[1])) add(unescape(m[1]), relative(root, file));
  }
}

if (process.argv.includes("--keys")) { console.log(JSON.stringify([...keys.keys()].filter((k) => process.argv.includes("--all") || !zhOnly.has(k)), null, 1)); process.exit(0); }

let bad = unwrapped.length;
if (unwrapped.length) console.log(`Unwrapped Chinese strings (${unwrapped.length}):\n  ${unwrapped.join("\n  ")}`);
for (const lang of ["en", "ja", "zh-TW"]) {
  const text = readFileSync(join(root, `src/locales/${lang}.ts`), "utf8");
  const have = new Set([...text.matchAll(/^\s*("(?:[^"\\]|\\.)*")\s*:/gm)].map((m) => JSON.parse(m[1])));
  const missing = [...keys.keys()].filter((k) => !have.has(k) && (lang === "zh-TW" || !zhOnly.has(k)));
  const unused = [...have].filter((k) => !keys.has(k) || (lang !== "zh-TW" && zhOnly.has(k)));
  if (missing.length) { bad += missing.length; console.log(`\n${lang}: ${missing.length} missing\n  ${missing.slice(0, 40).map((k) => JSON.stringify(k)).join("\n  ")}${missing.length > 40 ? "\n  …" : ""}`); }
  if (unused.length) console.log(`\n${lang}: ${unused.length} unused\n  ${unused.slice(0, 20).map((k) => JSON.stringify(k)).join("\n  ")}`);
}
console.log(`\n${keys.size} keys.`);
if (process.argv.includes("--strict") && bad) process.exit(1);
