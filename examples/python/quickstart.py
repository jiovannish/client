import os

from jio import Jio


jio = Jio(
    endpoint=os.environ["JIO_ENDPOINT"],
    api_key=os.environ["JIO_API_KEY"],
)
vm = jio.create()
print(f"created {vm.id}")

try:
    vm.write_file("/home/jio/message.txt", b"hello from Python\n")
    result = vm.exec(
        "python -c \"from pathlib import Path; "
        "print(Path('/home/jio/message.txt').read_text().upper(), end='')\""
    )
    result.raise_for_status()
    print(result.stdout_text, end="")

    contents = vm.read_file("/home/jio/message.txt")
    assert contents == b"hello from Python\n"
finally:
    vm.destroy()
