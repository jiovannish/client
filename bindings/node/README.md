# Jio Node.js SDK

`@jio/sdk` is a native Node.js binding to the shared Rust client. It creates a
retained Python microVM, pins its SSH identity, and exposes Promise-based command
and file operations.

```ts
import { Jio } from "@jio/sdk";

const jio = new Jio({
  endpoint: process.env.JIO_ENDPOINT,
  apiKey: process.env.JIO_API_KEY,
});
const vm = await jio.create();

try {
  const result = await vm.exec("python -c 'print(6 * 7)'");
  if (!result.success) throw new Error(result.stderr.toString("utf8"));
  console.log(result.stdout.toString("utf8"));
} finally {
  await vm.destroy();
}
```

The current experimental transport supports macOS and Linux clients with
OpenSSH installed. Jio currently provides only the Python 3.12 guest runtime;
this JavaScript package controls that VM and does not add a Node.js guest.
