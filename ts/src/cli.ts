// CLI entry point: solve a detached model JSON and print one line of JSON.
// Usage:
//   node src/cli.ts model.json [--lp] [--time 5] [--gap 1e-4] [--threads 1] [--no-presolve]

import { readFileSync } from "node:fs";
import { runModelJson, benchOptions, type ModelJson, type BenchOptions } from "./bench.ts";

function parseArgs(argv: readonly string[], opts: BenchOptions): string | null {
  let path: string | null = null;
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--lp") {
      opts.lp = true;
    } else if (a === "--time") {
      i += 1;
      opts.time = Number(argv[i]);
    } else if (a === "--gap") {
      i += 1;
      opts.gap = Number(argv[i]);
    } else if (a === "--threads") {
      i += 1;
      opts.threads = Number(argv[i]);
    } else if (a === "--no-presolve") {
      opts.presolve = false;
    } else if (a.startsWith("--")) {
      // ignore unknown flags
    } else {
      path = a;
    }
  }
  return path;
}

export function main(argv: readonly string[]): void {
  const opts = benchOptions();
  const path = parseArgs(argv, opts);
  if (path === null) {
    console.log(JSON.stringify({ error: "usage: <model.json> [--lp] [--time N]" }));
    return;
  }
  let text: string;
  try {
    text = readFileSync(path, "utf8");
  } catch (e) {
    console.log(JSON.stringify({ error: `read ${path}` }));
    return;
  }
  const v = JSON.parse(text) as ModelJson;
  console.log(JSON.stringify(runModelJson(v, opts)));
}

main(process.argv.slice(2));
