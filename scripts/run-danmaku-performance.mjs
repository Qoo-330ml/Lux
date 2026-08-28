import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { performance } from "node:perf_hooks";
import vm from "node:vm";

const require = createRequire(import.meta.url);
const typescript = require("../web/node_modules/typescript");
const repositoryRoot = fileURLToPath(new URL("..", import.meta.url));
const parserPath = "web/src/features/player/danmaku.ts";
const baselineRef = process.env.LUX_DANMAKU_BASELINE_REF || "HEAD";
const batchCount = Number.parseInt(process.env.LUX_DANMAKU_BENCH_BATCHES || "5", 10);
const sampleCount = Number.parseInt(process.env.LUX_DANMAKU_BENCH_SAMPLES || "30", 10);

if (!Number.isInteger(batchCount) || batchCount < 1) throw new Error("invalid LUX_DANMAKU_BENCH_BATCHES");
if (!Number.isInteger(sampleCount) || sampleCount < 10) throw new Error("invalid LUX_DANMAKU_BENCH_SAMPLES");

function loadParser(source) {
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
    TextEncoder,
    Uint8Array,
    console,
    exports: module.exports,
    module,
    performance,
  });
  return module.exports;
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

const currentSource = readFileSync(new URL(`../${parserPath}`, import.meta.url), "utf8");
const baselineSource = execFileSync("git", ["show", `${baselineRef}:${parserPath}`], {
  cwd: repositoryRoot,
  encoding: "utf8",
});
const currentParser = loadParser(currentSource);
const baselineParser = loadParser(baselineSource);
const entry = `<d p="1,1,25,0,0,0,0,0">${"弹幕".repeat(80)}</d>`;
const parserInput = `<i>${entry.repeat(5_000)}</i>`;
const oversizedInput = `<i>${"x".repeat(4 * 1024 * 1024)}</i>`;
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
  baselineRef,
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
