import { Jio } from "@jio/sdk";

const jio = new Jio({
  endpoint: process.env.JIO_ENDPOINT,
  apiKey: process.env.JIO_API_KEY,
});
const vm = await jio.create();
console.log(`created ${vm.id}`);

try {
  await vm.writeFile(
    "/home/jio/message.txt",
    Buffer.from("hello from Node.js\n"),
  );
  const result = await vm.exec(
    "python -c \"from pathlib import Path; " +
      "print(Path('/home/jio/message.txt').read_text().upper(), end='')\"",
  );
  if (!result.success) {
    throw new Error(result.stderr.toString("utf8"));
  }
  process.stdout.write(result.stdout);

  const contents = await vm.readFile("/home/jio/message.txt");
  if (contents.toString("utf8") !== "hello from Node.js\n") {
    throw new Error("Jio returned different file contents");
  }
} finally {
  await vm.destroy();
}
