// Runs a wasm32-wasip1 binary in Node: the cargo runner for the wasm tests of
// crates/ts_wasm (CI job `wasm`).
//
// usage: CARGO_TARGET_WASM32_WASIP1_RUNNER="node $PWD/scripts/wasm/test-runner.mjs" \
//          cargo test -p ts_wasm --target wasm32-wasip1
//
// Cargo runs a test binary in its crate's directory, so the runner path is
// absolute. The binary runs in a worker thread with a large stack, as
// npm/wasm/node.js runs the module: the checker recurses deeply, and the main
// thread's stack is about 1 MB. The exit status is the binary's; a trap (a
// panic aborts on wasm) exits with 1.
import { readFile } from "node:fs/promises";
import { Worker, isMainThread, parentPort, workerData } from "node:worker_threads";

if (isMainThread) {
    process.exitCode = 1;
    const worker = new Worker(new URL(import.meta.url), {
        workerData: process.argv.slice(2),
        resourceLimits: { stackSizeMb: 256 },
    });
    worker.on("message", status => (process.exitCode = status));
    worker.on("error", error => console.error(error));
} else {
    // Only here: node:wasi prints an ExperimentalWarning when it loads.
    const { WASI } = await import("node:wasi");
    const [file, ...args] = workerData;
    const wasi = new WASI({ version: "preview1", args: [file, ...args], env: process.env, returnOnExit: true });
    const module = await WebAssembly.compile(await readFile(file));
    const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
    parentPort.postMessage(wasi.start(instance));
}
