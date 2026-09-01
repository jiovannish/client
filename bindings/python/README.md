# Jio Python SDK

The Python SDK is a native binding to the shared Rust client. It creates a
retained Python microVM, pins its SSH identity, and exposes bounded command and
file operations.

```python
import os
from jio import Jio

jio = Jio(
    endpoint=os.environ["JIO_ENDPOINT"],
    api_key=os.environ["JIO_API_KEY"],
)
vm = jio.create()

try:
    result = vm.exec("python -c 'print(6 * 7)'")
    result.raise_for_status()
    print(result.stdout_text)
finally:
    vm.destroy()
```

`AsyncJio` exposes the same operations without blocking an asyncio event loop.
The current experimental transport supports macOS and Linux clients with
OpenSSH installed. Jio currently provides only the Python 3.12 guest runtime.
