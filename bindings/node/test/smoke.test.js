const assert = require("node:assert/strict");
const test = require("node:test");
const os = require("node:os");
const path = require("node:path");
const fs = require("node:fs");

const { Jio } = require("../index.js");

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
