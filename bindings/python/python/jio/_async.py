"""Asyncio adapters around the native Jio client."""

import asyncio
from typing import Optional

from ._native import CommandResult, Jio, Session, Vm


class AsyncJio:
    """Non-blocking facade for applications already using asyncio."""

    def __init__(
        self,
        endpoint: Optional[str] = None,
        api_key: Optional[str] = None,
        state_dir: Optional[str] = None,
    ) -> None:
        self._sync = Jio(endpoint, api_key, state_dir)

    @property
    def endpoint(self) -> str:
        return self._sync.endpoint

    async def create(self) -> "AsyncVm":
        vm = await asyncio.to_thread(self._sync.create)
        return AsyncVm(vm)

    async def inspect(self, session_id: str) -> Session:
        return await asyncio.to_thread(self._sync.inspect, session_id)

    async def get(self, session_id: str) -> Session:
        return await asyncio.to_thread(self._sync.get, session_id)

    async def attach(self, session_id: str) -> "AsyncVm":
        vm = await asyncio.to_thread(self._sync.attach, session_id)
        return AsyncVm(vm)

    async def stop(self, session_id: str) -> Session:
        return await asyncio.to_thread(self._sync.stop, session_id)

    async def start(self, session_id: str) -> Session:
        return await asyncio.to_thread(self._sync.start, session_id)

    async def destroy(self, session_id: str) -> None:
        await asyncio.to_thread(self._sync.destroy, session_id)


class AsyncVm:
    """An asyncio-compatible handle to a retained Jio VM."""

    def __init__(self, sync_vm: Vm) -> None:
        self._sync = sync_vm

    @property
    def id(self) -> str:
        return self._sync.id

    @property
    def session(self) -> Session:
        return self._sync.session

    async def refresh(self) -> Session:
        return await asyncio.to_thread(self._sync.refresh)

    async def exec(
        self,
        command: str,
        *,
        input: Optional[bytes] = None,
        timeout: int = 120,
    ) -> CommandResult:
        return await asyncio.to_thread(
            self._sync.exec,
            command,
            input=input,
            timeout=timeout,
        )

    async def write_file(
        self,
        remote_path: str,
        contents: bytes,
        *,
        timeout: int = 120,
    ) -> None:
        await asyncio.to_thread(
            self._sync.write_file,
            remote_path,
            contents,
            timeout=timeout,
        )

    async def read_file(self, remote_path: str, *, timeout: int = 120) -> bytes:
        return await asyncio.to_thread(
            self._sync.read_file,
            remote_path,
            timeout=timeout,
        )

    async def stop(self) -> Session:
        return await asyncio.to_thread(self._sync.stop)

    async def start(self) -> Session:
        return await asyncio.to_thread(self._sync.start)

    async def destroy(self) -> None:
        await asyncio.to_thread(self._sync.destroy)
