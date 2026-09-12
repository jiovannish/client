import assert from "node:assert/strict";
import { test } from "bun:test";
import * as os from "node:os";
import * as path from "node:path";
import * as fs from "node:fs";
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { Jio } from "../index.js";

const require = createRequire(import.meta.url);

test("defaults to Jio and preserves explicit/environment precedence", () => {
  const stateDir = fs.mkdtempSync(path.join(os.tmpdir(), "jio-defaults-test-"));
  fs.chmodSync(stateDir, 0o700);
  const env = { ...process.env };
  delete env.JIO_ENDPOINT;
  delete env.JIO_HOST;
  delete env.JIO_CA_CERT;
  delete env.TEST_ENDPOINT;
  const script = `const { Jio } = require(${JSON.stringify(require.resolve("../index.js"))});
    const jio = new Jio({ apiKey: "0123456789abcdef0123456789abcdef",
      stateDir: ${JSON.stringify(stateDir)}, endpoint: process.env.TEST_ENDPOINT });
    console.log(jio.endpoint);`;
  try {
    for (const [settings, expected] of [
      [{}, "https://46.105.119.217"],
      [{ JIO_HOST: "http://127.0.0.1:9011" }, "http://127.0.0.1:9011"],
      [{ JIO_HOST: "http://127.0.0.1:9011", JIO_ENDPOINT: "http://127.0.0.1:9010" }, "http://127.0.0.1:9010"],
      [{ JIO_ENDPOINT: "http://127.0.0.1:9010", TEST_ENDPOINT: "http://127.0.0.1:9012" }, "http://127.0.0.1:9012"],
    ] as const) {
      assert.equal(execFileSync(process.execPath, ["-e", script], {
        env: { ...env, ...settings }, encoding: "utf8",
      }).trim(), expected);
    }
    assert.throws(() => execFileSync(process.execPath, ["-e", script], {
      env: { ...env, JIO_ENDPOINT: "" }, stdio: "pipe",
    }));
  } finally {
    fs.rmSync(stateDir, { recursive: true });
  }
});

test("constructs a client without contacting Core", () => {
  const stateDir = fs.mkdtempSync(path.join(os.tmpdir(), "jio-node-test-"));
  fs.chmodSync(stateDir, 0o700);
  try {
    const jio = new Jio({
      endpoint: "http://127.0.0.1:8080",
      apiKey: "0123456789abcdef0123456789abcdef",
      stateDir,
    });
    assert.equal(jio.endpoint, "http://127.0.0.1:8080");
    assert.equal(typeof jio.create, "function");
    assert.equal(typeof jio.get, "function");
    assert.equal(typeof jio.attach, "function");
    assert.equal(typeof jio.destroy, "function");
  } finally {
    fs.rmSync(stateDir, { recursive: true });
  }
});

test("rejects a native async request when the session does not exist", async () => {
  const stateDir = fs.mkdtempSync(path.join(os.tmpdir(), "jio-async-test-"));
  fs.chmodSync(stateDir, 0o700);
  const paths: string[] = [];
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch(request) {
      paths.push(new URL(request.url).pathname);
      return new Response("not found", { status: 404 });
    },
  });
  try {
    const jio = new Jio({
      endpoint: server.url.origin,
      apiKey: "0123456789abcdef0123456789abcdef",
      stateDir,
    });
    const id = "a".repeat(32);
    await assert.rejects(jio.inspect(id));
    assert.deepEqual(paths, [`/v0/sessions/${id}`]);
  } finally {
    server.stop(true);
    fs.rmSync(stateDir, { recursive: true });
  }
});
