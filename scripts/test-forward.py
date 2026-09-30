#!/usr/bin/env python3
"""Local-only acceptance: built CLI -> fixture API/HTTPS gateway -> SSH -> PostgreSQL.

Requires Python 3, OpenSSH (including sshd), openssl and PostgreSQL tools on PATH.
No installed service or Jio account is used. The SSH wrapper ONLY maps the fixture
guest username/address/port to the current local user and unprivileged test sshd.
Run: python3 scripts/test-forward.py [target/debug/jio]
Set JIO_TEST_PG_BIN if pg_config selects a client-only PostgreSQL installation.
"""
import concurrent.futures
import contextlib
import getpass
import http.server
import json
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time


def run(*args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, capture_output=True,
                          text=True, timeout=30, **kwargs).stdout.strip()


def port():
    with socket.socket() as stream:
        stream.bind(("127.0.0.1", 0))
        return stream.getsockname()[1]


def wait_for(check, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(.02)
    raise AssertionError("timed out waiting for fixture")


def listening(number):
    try:
        with socket.create_connection(("127.0.0.1", number), timeout=.2):
            return True
    except OSError:
        return False


@contextlib.contextmanager
def process(args, **kwargs):
    child = subprocess.Popen([str(a) for a in args], **kwargs)
    try:
        yield child
    finally:
        if child.poll() is None:
            child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=5)


def main():
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/jio").resolve()
    ssh = shutil.which("ssh")
    sshd = shutil.which("sshd") or "/usr/sbin/sshd"
    pg_bin = Path(os.environ.get("JIO_TEST_PG_BIN") or run("pg_config", "--bindir"))
    with tempfile.TemporaryDirectory(prefix="jio-forward-", dir="/tmp") as temporary:
        root = Path(temporary)
        sid, key = "ab" * 16, "01" * 32  # disposable fixture authority
        for name in ["host", "client", "wrong"]:
            run("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "", "-f", root / name)
        host_key = (root / "host.pub").read_text().strip()
        state = root / "state" / "sessions" / sid
        state.mkdir(parents=True, mode=0o700)
        state.parent.chmod(0o700)
        state.parent.parent.chmod(0o700)
        shutil.copy(root / "client", state / "id_ed25519")
        (state / "known_hosts").write_text(f"jio-{sid} {host_key}\n")
        (state / "known_hosts").chmod(0o600)
        (state.parent.parent / "current-session").write_text(sid)
        (state.parent.parent / "current-session").chmod(0o600)
        ssh_port, pg_port = port(), port()
        config = root / "sshd.conf"
        base = (f"Port {ssh_port}\nListenAddress 127.0.0.1\nHostKey {root}/host\n"
                f"AuthorizedKeysFile {root}/client.pub\nPidFile {root}/sshd.pid\n"
                "StrictModes no\nUsePAM no\nAuthenticationMethods publickey\n"
                "PasswordAuthentication no\nKbdInteractiveAuthentication no\n"
                "PermitOpen 127.0.0.1:*\nAllowStreamLocalForwarding no\nGatewayPorts no\n")
        config.write_text(base + "AllowTcpForwarding local\n")
        # Repeated negative-auth cases intentionally share localhost. Disable only
        # this fixture's OpenSSH 9.8+ source penalty, not any installed daemon.
        if subprocess.run([sshd, "-T", "-f", str(config), "-o", "PerSourcePenalties=no"],
                          capture_output=True, timeout=5).returncode == 0:
            base += "PerSourcePenalties no\n"
            config.write_text(base + "AllowTcpForwarding local\n")
        wrappers = root / "bin"
        wrappers.mkdir()
        (wrappers / "ssh").write_text(
            f"#!{sys.executable}\nimport os,sys\nargs=sys.argv[1:]\n"
            f"if any(a.endswith('@fixture-jump') for a in args) and '-F' not in args: args=['-F',{str(root / 'jump.conf')!r}]+args\n"
            f"args=[a.replace('jio@127.0.0.1', {getpass.getuser() + '@127.0.0.1'!r})"
            f".replace('jio@172.31.10.2', {getpass.getuser() + '@127.0.0.1'!r}) for a in args]\n"
            f"args=[str({ssh_port}) if a=='22' and i and args[i-1]=='-p' else a for i,a in enumerate(args)]\n"
            f"args=[{f'127.0.0.1:{ssh_port}'!r} if a=='172.31.10.2:22' else a for a in args]\n"
            f"os.execv({ssh!r}, [{ssh!r}]+args)\n")
        (wrappers / "ssh").chmod(0o700)
        (root / "jump.hosts").write_text(f"fixture-jump {host_key}\n")
        (root / "jump.conf").write_text(
            f"Host fixture-jump\n HostName 127.0.0.1\n Port {ssh_port}\n"
            f" IdentityFile {root}/client\n IdentitiesOnly yes\n"
            f" HostKeyAlias fixture-jump\n UserKnownHostsFile {root}/jump.hosts\n"
            " StrictHostKeyChecking yes\n GlobalKnownHostsFile /dev/null\n")
        env = {k: v for k, v in os.environ.items() if not k.startswith(("JIO_", "PG"))}
        env.update(JIO_STATE_DIR=str(state.parent.parent), JIO_API_KEY=key,
                   PATH=str(wrappers) + os.pathsep + os.environ["PATH"])
        # Test the real HTTPS transport with a temporary CA; never disable TLS verification.
        run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            "-subj", "/CN=Jio local fixture CA", "-keyout", root / "ca.key", "-out", root / "ca.pem")
        run("openssl", "req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost",
            "-keyout", root / "tls.key", "-out", root / "tls.csr")
        (root / "extensions").write_text("subjectAltName=IP:127.0.0.1\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n")
        run("openssl", "x509", "-req", "-in", root / "tls.csr", "-CA", root / "ca.pem",
            "-CAkey", root / "ca.key", "-CAcreateserial", "-days", "1",
            "-extfile", root / "extensions", "-out", root / "tls.pem")
        env["JIO_CA_CERT"] = str(root / "ca.pem")
        session = dict(session_id=sid, state="ready", generation=1, runtime="python3.12-source-v0",
                       vcpu_count=2, memory_mib=4096, template_id="c" * 64, core_sha256="d" * 64,
                       volume_id="ef" * 16, workspace_path="/workspace", guest_ipv4="172.31.10.2",
                       ssh_port=22, ssh_username="jio", ssh_host_public_key=host_key,
                       guest_ready_ns=10, storage_ready_ns=20, network_ready_ns=30, ssh_ready_ns=40)
        relays = []
        requests = []
        exposure = dict(session_id=sid, port=pg_port, generation=1, status="published",
                        url=f"https://{sid}-{pg_port}.apps.example.test", credential=None)

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def do_GET(self):
                requests.append(self.path)
                if self.headers.get("Authorization") != "Bearer " + key:
                    return self.reply(401, {"error": "unauthorized fixture key"})
                if self.path == f"/v0/sessions/{sid}":
                    return self.reply(200, session)
                if self.path == f"/v1/sessions/{sid}/ports":
                    return self.reply(200, [{**exposure, "status": "published"}])
                if self.path != f"/v0/sessions/{sid}/ssh":
                    return self.reply(404, {"error": "fixture session not found"})
                assert self.headers["Upgrade"] == "jio-ssh"
                assert self.headers["X-Jio-Generation"] == "1"
                with socket.create_connection(("127.0.0.1", ssh_port), timeout=5) as upstream:
                    self.send_response(101)
                    self.send_header("Connection", "upgrade")
                    self.send_header("Upgrade", "jio-ssh")
                    self.end_headers()
                    streams = [self.connection, upstream]
                    relays.extend(streams)
                    try:
                        while True:
                            ready, _, _ = select.select(streams, [], [], 1)
                            for source in ready:
                                data = source.recv(65536)
                                if not data:
                                    return
                                streams[1 - streams.index(source)].sendall(data)
                    except OSError:
                        return
                    finally:
                        for stream in streams:
                            relays.remove(stream)
                        self.close_connection = True

            def do_POST(self):
                requests.append(self.path)
                if self.headers.get("Authorization") != "Bearer " + key:
                    return self.reply(401, {"error": "unauthorized fixture key"})
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                if self.path == f"/v1/sessions/{sid}/ports/{pg_port}":
                    return self.reply(200, exposure)
                if self.path == "/v1/domains":
                    assert json.loads(body) == dict(hostname="app.example.test", session_id=sid, port=pg_port)
                    return self.reply(200, dict(url="https://app.example.test", hostname="app.example.test",
                                               status="pending_dns", records=[], subdomain_cname_alternative="gateway.example.test"))
                return self.reply(404, {"error": "fixture route not found"})

            def reply(self, status, value):
                data = json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        with contextlib.ExitStack() as stack:
            for tls in [False, True]:
                server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
                if tls:
                    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                    ctx.load_cert_chain(root / "tls.pem", root / "tls.key")
                    server.socket = ctx.wrap_socket(server.socket, server_side=True)
                threading.Thread(target=server.serve_forever, daemon=True).start()
                stack.callback(server.server_close)
                stack.callback(server.shutdown)
                if tls:
                    hosted = f"https://127.0.0.1:{server.server_port}"
                else:
                    direct = f"http://127.0.0.1:{server.server_port}"
                    env["JIO_ENDPOINT_PORT"] = str(server.server_port)
            log = stack.enter_context((root / "sshd.log").open("w"))
            daemon = stack.enter_context(process([sshd, "-D", "-e", "-f", config], stderr=log))
            wait_for(lambda: listening(ssh_port))
            run(pg_bin / "initdb", "-D", root / "pg", "-A", "trust", "-U", "fixture", "--no-locale", env=env)
            run(pg_bin / "pg_ctl", "-D", root / "pg", "-l", root / "pg.log", "-w", "start",
                "-o", f"-h 127.0.0.1 -p {pg_port} -k {root}", env=env)
            stack.callback(run, pg_bin / "pg_ctl", "-D", root / "pg", "-m", "immediate", "-w", "stop")

            def command(endpoint, local, guest=pg_port, target=sid):
                return [str(binary), "forward", str(guest), target, "--local-port", str(local), "--host", endpoint]

            def query(local, sql="select 42"):
                return run(pg_bin / "psql", "-h", "127.0.0.1", "-p", local, "-U", "fixture",
                           "-d", "postgres", "-At", "-c", sql, env={**env, "PGCONNECT_TIMEOUT": "3"})

            def fails(args, contains=None, overrides=None):
                result = subprocess.run(args, env={**env, **(overrides or {})}, capture_output=True,
                                        text=True, timeout=20)
                assert result.returncode != 0, result.stdout
                if contains:
                    assert contains in result.stderr, result.stderr + (root / "sshd.log").read_text()

            ssh_args = [ssh, "-F", str(root / "jump.conf"), "-o", "BatchMode=yes",
                        "-o", "ExitOnForwardFailure=yes"]
            jump = getpass.getuser() + "@fixture-jump"
            # PermitOpen compares literal names: even localhost must not bypass
            # the fixed 127.0.0.1 destination policy. No external address is dialed.
            fails(ssh_args + ["-W", f"localhost:{pg_port}", jump], "administratively prohibited")
            remote_port = port()
            fails(ssh_args + ["-N", "-R", f"127.0.0.1:{remote_port}:127.0.0.1:{pg_port}", jump],
                  "remote port forwarding failed")
            assert not listening(remote_port)
            print("PASS PermitOpen rejects nonliteral loopback; remote TCP forwarding denied")

            before = len(requests)
            for bad in ["0", "65536", "-1", "abc"]:
                fails(command(hosted, bad))
                fails(command(hosted, port(), guest=bad))
            for options in [["1234", "--local-port", "12345"], ["--local-port=12345", "1234"]]:
                fails([str(binary), "expose", *options, "--host", hosted],
                      "--local-port is only supported by jio forward")
            fails([str(binary), "expose", "1234", "--typo"], "unexpected option '--typo'")
            assert len(requests) == before
            assert "PostgreSQL" in run(binary, "forward", "--help", env=env)
            assert "HTTP" in run(binary, "expose", "--help", env=env)
            fails(command(hosted, port()), overrides={"JIO_API_KEY": "02" * 32})
            fails(command(hosted, port(), target="cd" * 16))
            session["state"] = "stopped"
            fails(command(hosted, port()), "Stopped")
            session["state"] = "ready"
            session["ssh_host_public_key"] = (root / "wrong.pub").read_text().strip()
            fails(command(hosted, port()), "pinned key")
            session["ssh_host_public_key"] = host_key
            print("PASS help, invalid ports before network, API auth, session and pinned-key rejection")

            for endpoint in [direct, hosted, getpass.getuser() + "@fixture-jump"]:
                local = port()
                with socket.socket() as occupied:
                    occupied.bind(("127.0.0.1", local))
                    occupied.listen()
                    fails(command(endpoint, local))
                shutil.copy(root / "wrong", state / "id_ed25519")
                fails(command(endpoint, port()), "Permission denied")
                shutil.copy(root / "client", state / "id_ed25519")
                # Same pinned key as API, but the SSH server presents another key.
                original = (state / "known_hosts").read_text()
                wrong_key = (root / "wrong.pub").read_text().strip()
                (state / "known_hosts").write_text(f"jio-{sid} {wrong_key}\n")
                session["ssh_host_public_key"] = wrong_key
                fails(command(endpoint, port()), "HOST IDENTIFICATION HAS CHANGED")
                (state / "known_hosts").write_text(original)
                session["ssh_host_public_key"] = host_key
                for stop in [signal.SIGINT, signal.SIGTERM, signal.SIGHUP]:
                    local = port()
                    with (root / "forward.log").open("w+") as errors:
                        with process(command(endpoint, local), env=env, stderr=errors, stdout=subprocess.DEVNULL) as child:
                            wait_for(lambda: child.poll() is not None or listening(local))
                            assert child.poll() is None, (root / "forward.log").read_text()
                            assert query(local) == "42"
                            if endpoint == direct and stop == signal.SIGINT:
                                expose = [str(binary), "expose", str(pg_port), sid, "--host", endpoint]
                                before_expose = len(requests)
                                fails(expose, f"already exposed publicly over HTTP: {exposure['url']}")
                                assert "/v1/domains" not in requests[before_expose:]
                                assert "pending_dns" in run(*expose, "--domain", "app.example.test", env=env)
                                # An existing helper reconnecting must not be rejected as a duplicate.
                                exposure["status"] = "disconnected"
                                assert run(*expose, env=env) == exposure["url"]
                                exposure["status"] = "published"
                                listing = subprocess.run([str(binary), "ports", sid, "--host", endpoint],
                                                         env=env, capture_output=True, text=True, timeout=20, check=True)
                                assert f"{pg_port}\tpublic-http\tpublished\t{exposure['url']}" in listing.stdout
                                assert "Local SSH forwards (jio forward) are not listed here" in listing.stderr
                                assert query(local) == "42"  # Exposure commands leave the active forward alone.
                                print("PASS expose flag hints, duplicate error, domain alias, reconnect and HTTP labels while forwarding")
                            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                                assert list(pool.map(lambda _: query(local), range(4))) == ["42"] * 4
                            assert query(local, "select repeat('x', 65536)") == "x" * 65536
                            child.send_signal(stop)
                            assert child.wait(timeout=5) == 0
                            assert not list(Path("/tmp").glob(f".create-{child.pid}-*"))
                        wait_for(lambda: not listening(local))
                        wait_for(lambda: not relays)
                print("PASS", "HTTPS master" if endpoint == hosted else "direct SSH" if endpoint == direct else "SSH config alias/jump",
                      "PostgreSQL + 4 clients + 64KiB, bind/auth/key failures, INT/TERM/HUP cleanup")

            # Guest refusal is a channel error; gateway loss ends the whole forward.
            local, unused_guest_port = port(), port()
            with (root / "forward.log").open("w+") as errors:
                with process(command(hosted, local, guest=unused_guest_port), env=env,
                             stderr=errors, stdout=subprocess.DEVNULL) as child:
                    wait_for(lambda: listening(local))
                    wait_for(lambda: "Connection refused" in (root / "forward.log").read_text())
                    assert child.poll() is None
                    for stream in relays[:]:
                        try:
                            stream.shutdown(socket.SHUT_RDWR)
                        except OSError:
                            pass
                    assert child.wait(timeout=5) != 0
            wait_for(lambda: not listening(local))
            wait_for(lambda: not relays)
            print("PASS guest connection refusal is visible; gateway loss fails and closes listener")

            # Old-template behavior must remain visible, not be mistaken for success.
            config.write_text(base + "AllowTcpForwarding no\n")
            daemon.send_signal(signal.SIGHUP)
            time.sleep(.2)
            local = port()
            with (root / "forward.log").open("w+") as errors:
                with process(command(hosted, local), env=env, stderr=errors, stdout=subprocess.DEVNULL) as child:
                    wait_for(lambda: listening(local))
                    try:
                        query(local)
                        raise AssertionError("disabled forwarding unexpectedly reached PostgreSQL")
                    except subprocess.CalledProcessError:
                        pass
                    wait_for(lambda: "administratively prohibited" in (root / "forward.log").read_text())
                    assert child.poll() is None  # per-channel failure, documented native SSH behavior
            wait_for(lambda: not listening(local))
            wait_for(lambda: not relays)
            print("PASS old-template policy rejection is visible and connections fail")
    print("PASS all disposable processes, clusters, keys and fixture files cleaned up")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        print(error.stderr, file=sys.stderr)
        raise
