import os

from jio import Jio


jio = Jio(
    api_key=os.environ["JIO_API_KEY"],
)
vm = jio.create()
print(f"created {vm.id}")

try:
    vm.write_file("/home/jio/message.txt", b"hello from Python\n")
    result = vm.exec(
        "tr '[:lower:]' '[:upper:]' < /home/jio/message.txt"
    )
    result.raise_for_status()
    print(result.stdout_text, end="")

    contents = vm.read_file("/home/jio/message.txt")
    assert contents == b"hello from Python\n"
finally:
    vm.destroy()
