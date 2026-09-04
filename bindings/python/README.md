# Jio Python SDK

The Python SDK is a native binding to the shared Rust client. It creates a
retained microVM, pins its SSH identity, and exposes bounded command and file
operations.

```python
import os
from jio import Jio

jio = Jio(
    endpoint=os.environ["JIO_ENDPOINT"],
    api_key=os.environ["JIO_API_KEY"],
)
vm = jio.create()

try:
    result = vm.exec("printf '42\\n'")
    result.raise_for_status()
    print(result.stdout_text)
    vm.write_file("/workspace/result.txt", result.stdout)
    vm.stop()
    restarted = vm.start()
    assert restarted.generation == 2
    assert vm.read_file("/workspace/result.txt") == result.stdout
finally:
    vm.destroy()
```

`AsyncJio` exposes the same operations without blocking an asyncio event loop.
The current experimental transport supports macOS and Linux clients with
OpenSSH installed. Standalone Core currently admits one operator-selected
template; this client does not assume a guest language runtime.
