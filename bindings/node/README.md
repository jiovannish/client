# Jio Node.js SDK

`@jio/sdk` is a native Node.js binding to the shared Rust client. It creates a
retained microVM, pins its SSH identity, and exposes Promise-based command and
file operations.

```ts
import { Jio } from "@jio/sdk";

const jio = new Jio({
  apiKey: process.env.JIO_API_KEY,
});
const vm = await jio.create();

try {
  const result = await vm.exec("printf '42\\n'");
  if (!result.success) throw new Error(result.stderr.toString("utf8"));
  console.log(result.stdout.toString("utf8"));
  await vm.writeFile("/workspace/result.txt", result.stdout);
  await vm.stop();
  const restarted = await vm.start();
  if (restarted.generation !== 2n) throw new Error("restart failed");
} finally {
  await vm.destroy();
}
```

The Jio connection and public TLS certificate are built in; only an API key is
needed. Hosted ephemeral sessions do not support stop/start.

The current experimental transport supports macOS and Linux clients with
OpenSSH installed. Standalone Core currently admits one operator-selected
template; this package controls that VM and does not add or assume a guest
language runtime.
