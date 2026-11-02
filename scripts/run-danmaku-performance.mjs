import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { performance } from "node:perf_hooks";
import vm from "node:vm";

const require = createRequire(import.meta.url);
const typescript = require("../web/node_modules/typescript");
const repositoryRoot = fileURLToPath(new URL("..", import.meta.url));

// The parser moved into `danmaku/parse.ts` when parsing, scheduling, rendering, and loading were
// split. The baseline still points at the pre-split file so old revisions keep working.
const HISTORICAL_PARSER_PATH = "web/src/features/player/danmaku.ts";
const currentPath = process.env.LUX_DANMAKU_CURRENT_PATH
  || "web/src/features/player/danmaku/parse.ts";
const baselineRef = process.env.LUX_DANMAKU_BASELINE_REF || "HEAD";
const batchCount = Number.parseInt(process.env.LUX_DANMAKU_BENCH_BATCHES || "5", 10);
const sampleCount = Number.parseInt(process.env.LUX_DANMAKU_BENCH_SAMPLES || "30", 10);
const oversizedBytes = Number.parseInt(
  process.env.LUX_DANMAKU_OVERSIZED_BYTES || String(17 * 1024 * 1024),
  10,
);

const USAGE = `Usage: node scripts/run-danmaku-performance.mjs [--help] [--check]

Benchmarks the current Web danmaku parser against the parser of a Git revision.

  --help    Print this help and exit without touching Git or TypeScript.
  --check   Resolve and load the baseline and current parsers, then exit without benchmarking.
            Use it to verify the configured paths after a refactor.

Environment:
  LUX_DANMAKU_BASELINE_REF     Git revision for the baseline parser (default: HEAD).
  LUX_DANMAKU_BASELINE_PATH    Parser path inside that revision. Default: the pre-split
                               web/src/features/player/danmaku.ts, falling back to
                               web/src/features/player/danmaku/parse.ts.
  LUX_DANMAKU_CURRENT_PATH     Parser path in the working tree
                               (default: web/src/features/player/danmaku/parse.ts).
  LUX_DANMAKU_BENCH_BATCHES    Batches per run, alternating parser order (default: 5).
  LUX_DANMAKU_BENCH_SAMPLES    Samples per batch (default: 30).
  LUX_DANMAKU_OVERSIZED_BYTES  Size of the oversized ASCII payload (default: 17825792 = 17 MiB,
                               above the client's 16 MiB budget).
`;

function git(...args) {
  return execFileSync("git", args, {
    cwd: repositoryRoot,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function gitFailure(error) {
  const stderr = typeof error?.stderr === "string" ? error.stderr.trim() : "";
  if (stderr) return stderr.split("\n").slice(-3).join(" | ");
  return error instanceof Error ? error.message : String(error);
}

/** Fails loudly when Git itself is unusable, so “path missing” cannot mask a broken environment. */
function assertRevisionExists(ref) {
  try {
    git("rev-parse", "--verify", `${ref}^{commit}`);
  } catch (error) {
    throw new Error(`cannot resolve baseline revision ${ref}: ${gitFailure(error)}`);
  }
}

function revisionHasPath(ref, path) {
  try {
    git("cat-file", "-e", `${ref}:${path}`);
    return true;
  } catch {
    return false;
  }
}

function resolveBaselinePath(ref) {
  const configured = process.env.LUX_DANMAKU_BASELINE_PATH;
  assertRevisionExists(ref);
  if (configured) return configured;
  for (const candidate of [HISTORICAL_PARSER_PATH, currentPath]) {
    if (revisionHasPath(ref, candidate)) return candidate;
  }
  throw new Error(
    `baseline revision ${ref} contains neither ${HISTORICAL_PARSER_PATH} nor ${currentPath}; `
    + "set LUX_DANMAKU_BASELINE_PATH to the parser path in that revision",
  );
}

function loadParser(source, label) {
  const output = typescript.transpileModule(source, {
    compilerOptions: {
      module: typescript.ModuleKind.CommonJS,
      target: typescript.ScriptTarget.ES2022,
    },
  }).outputText;
  const module = { exports: {} };
  vm.runInNewContext(output, {
    Blob,
    RegExp,
    TextDecoder,
    TextEncoder,
    Uint8Array,
    console,
    exports: module.exports,
    module,
    performance,
  });
  if (typeof module.exports.parseBilibiliDanmaku !== "function") {
    throw new Error(`${label} does not export parseBilibiliDanmaku`);
  }
  return module.exports;
}

function loadCurrentParser() {
  const source = readFileSync(new URL(`../${currentPath}`, import.meta.url), "utf8");
  return loadParser(source, `current parser ${currentPath}`);
}

function loadBaselineParser(ref, path) {
  let source;
  try {
    source = git("show", `${ref}:${path}`);
  } catch (error) {
    throw new Error(
      `cannot read ${path} at ${ref}: ${gitFailure(error)}; `
      + "set LUX_DANMAKU_BASELINE_PATH to an existing path",
    );
  }
  return loadParser(source, `baseline parser ${path} at ${ref}`);
}

function median(values) {
  const ordered = [...values].sort((left, right) => left - right);
  return ordered[Math.floor(ordered.length / 2)];
}

function benchmark(parser, input) {
  for (let index = 0; index < 8; index += 1) {
    try {
      parser.parseBilibiliDanmaku(input);
    } catch {
      // Warm up both successful and rejected parser paths without changing output.
    }
  }
  const samples = [];
  let result;
  for (let index = 0; index < sampleCount; index += 1) {
    const start = performance.now();
    try {
      result = parser.parseBilibiliDanmaku(input).length;
    } catch (error) {
      result = error.code;
    }
    samples.push(performance.now() - start);
  }
  samples.sort((left, right) => left - right);
  return {
    result,
    p50Ms: Number(samples[Math.floor(samples.length * 0.5)].toFixed(3)),
    p95Ms: Number(samples[Math.floor(samples.length * 0.95)].toFixed(3)),
  };
}

function medianMetric(results, key) {
  return Number(median(results.map((result) => result[key])).toFixed(3));
}

function run({ checkOnly }) {
  if (!Number.isInteger(batchCount) || batchCount < 1) throw new Error("invalid LUX_DANMAKU_BENCH_BATCHES");
  if (!Number.isInteger(sampleCount) || sampleCount < 10) throw new Error("invalid LUX_DANMAKU_BENCH_SAMPLES");
  if (!Number.isInteger(oversizedBytes) || oversizedBytes < 1) {
    throw new Error("invalid LUX_DANMAKU_OVERSIZED_BYTES");
  }

  const baselinePath = resolveBaselinePath(baselineRef);
  const currentParser = loadCurrentParser();
  const baselineParser = loadBaselineParser(baselineRef, baselinePath);
  const resolved = {
    baselineRef,
    baselinePath,
    currentPath,
    baselineIsHistoricalPath: baselinePath === HISTORICAL_PARSER_PATH,
  };

  if (checkOnly) {
    const smoke = '<i><d p="1,1,25,16777215,0,0,0,0">check</d></i>';
    console.log(JSON.stringify({
      mode: "check",
      ...resolved,
      baselineEntries: baselineParser.parseBilibiliDanmaku(smoke).length,
      currentEntries: currentParser.parseBilibiliDanmaku(smoke).length,
    }, null, 2));
    return;
  }

  const entry = `<d p="1,1,25,0,0,0,0,0">${"弹幕".repeat(80)}</d>`;
  const parserInput = `<i>${entry.repeat(5_000)}</i>`;
  const oversizedInput = `<i>${"x".repeat(oversizedBytes)}</i>`;
  const observations = {
    parser: { baseline: [], current: [] },
    sizeCheck: { baseline: [], current: [] },
  };

  for (let batch = 0; batch < batchCount; batch += 1) {
    const first = batch % 2 === 0 ? "baseline" : "current";
    const parsers = first === "baseline"
      ? { baseline: baselineParser, current: currentParser }
      : { current: currentParser, baseline: baselineParser };
    for (const [name, parser] of Object.entries(parsers)) {
      observations.parser[name].push(benchmark(parser, parserInput));
      observations.sizeCheck[name].push(benchmark(parser, oversizedInput));
    }
  }

  console.log(JSON.stringify({
    arch: execFileSync("uname", ["-m"], { cwd: repositoryRoot, encoding: "utf8" }).trim(),
    ...resolved,
    batchCount,
    sampleCount,
    parserInputBytes: new TextEncoder().encode(parserInput).byteLength,
    oversizedInputBytes: new TextEncoder().encode(oversizedInput).byteLength,
    parser: {
      baseline: {
        result: observations.parser.baseline[0].result,
        p50Ms: medianMetric(observations.parser.baseline, "p50Ms"),
        p95Ms: medianMetric(observations.parser.baseline, "p95Ms"),
      },
      current: {
        result: observations.parser.current[0].result,
        p50Ms: medianMetric(observations.parser.current, "p50Ms"),
        p95Ms: medianMetric(observations.parser.current, "p95Ms"),
      },
    },
    sizeCheck: {
      baseline: {
        result: observations.sizeCheck.baseline[0].result,
        p50Ms: medianMetric(observations.sizeCheck.baseline, "p50Ms"),
        p95Ms: medianMetric(observations.sizeCheck.baseline, "p95Ms"),
      },
      current: {
        result: observations.sizeCheck.current[0].result,
        p50Ms: medianMetric(observations.sizeCheck.current, "p50Ms"),
        p95Ms: medianMetric(observations.sizeCheck.current, "p95Ms"),
      },
    },
  }, null, 2));
}

const args = process.argv.slice(2);
const unknown = args.filter((arg) => arg !== "--check");
if (args.includes("--help") || args.includes("-h")) {
  console.log(USAGE);
  process.exit(0);
}
if (unknown.length > 0) {
  console.error(`unknown argument: ${unknown.join(" ")}\n`);
  console.error(USAGE);
  process.exit(2);
}

try {
  run({ checkOnly: args.includes("--check") });
} catch (error) {
  console.error(`run-danmaku-performance: ${error instanceof Error ? error.message : error}`);
  process.exit(1);
}
