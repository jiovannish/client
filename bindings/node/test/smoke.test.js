const assert = require("node:assert/strict");
const test = require("node:test");
const os = require("node:os");
const path = require("node:path");
const fs = require("node:fs");

const { Jio } = require("../index.js");

test("defaults to Jio and preserves explicit/environment precedence", () => {
  const { execFileSync } = require("node:child_process");
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
    ]) {
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
