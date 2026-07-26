// Fold the mock build into one HTML file so it opens straight from disk.
import { readFileSync, writeFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

const dir = "dist-mock";
const assets = join(dir, "assets");
const files = readdirSync(assets);

const css = files.filter((f) => f.endsWith(".css")).map((f) => readFileSync(join(assets, f), "utf8")).join("\n");
const js = files.filter((f) => f.endsWith(".js")).map((f) => readFileSync(join(assets, f), "utf8")).join("\n");

const html = `<!doctype html>
<html lang="en"><head><meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>ccmon — preview</title>
<style>${css}</style>
</head><body><div id="app"></div><script>${js}</script></body></html>`;

writeFileSync(join(dir, "preview.html"), html);
console.log(`wrote ${dir}/preview.html (${(html.length / 1024).toFixed(1)} kB)`);
