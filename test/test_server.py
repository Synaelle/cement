"""Protocol tests for the synaftp server, run against the PC shim build.

usage: python test_server.py <server_pc.exe>
"""
import ftplib
import hashlib
import io
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

HOST, PORT = "127.0.0.1", 5000
CHUNK = 256 * 1024
failures = []


def check(cond, what):
    print(("  ok   " if cond else "  FAIL ") + what)
    if not cond:
        failures.append(what)


def connect():
    for _ in range(100):
        try:
            f = ftplib.FTP()
            f.connect(HOST, PORT, timeout=10)
            f.login()
            return f
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("server not reachable")


def upload(f, name, data):
    f.storbinary("STOR " + name, io.BytesIO(data), blocksize=1 << 20)


def download(f, name, rest=None):
    out = io.BytesIO()
    f.retrbinary("RETR " + name, out.write, blocksize=1 << 20, rest=rest)
    return out.getvalue()


def mlsd(f, path):
    return {n: facts for n, facts in f.mlsd(path)}


def main():
    exe = os.path.abspath(sys.argv[1])
    root = tempfile.mkdtemp(prefix="synaftp-sd-")
    flag = os.path.join(root, "..", os.path.basename(root) + ".sleep")
    flag = os.path.abspath(flag)
    outside = os.path.join(os.path.dirname(root), os.path.basename(root) + "-outside.txt")
    with open(outside, "w") as fh:
        fh.write("secret")
    env = dict(os.environ, SYNAFTP_ROOT=root, SYNAFTP_SLEEPFLAG=flag)
    srv = subprocess.Popen([exe], env=env, stderr=subprocess.PIPE, text=True)
    try:
        run(root, flag)
    finally:
        srv.kill()
        print("server log:", srv.stderr.read().strip().replace("\n", " | "))
        shutil.rmtree(root, ignore_errors=True)
        os.remove(outside)
        if os.path.exists(flag):
            os.remove(flag)
    print("\n%d failure(s)" % len(failures))
    sys.exit(1 if failures else 0)


def run(root, flag):
    f = connect()
    print("basic")
    check(f.pwd() == "/", "PWD is /")
    check("MLSD" in f.sendcmd("FEAT"), "FEAT lists MLSD")
    f.mkd("/t")
    f.cwd("/t")
    check(f.pwd() == "/t", "CWD into new folder")

    print("uploads / downloads (sizes around buffer boundaries)")
    for size in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 4 * CHUNK, 4 * CHUNK + 7, 9 * CHUNK + 123, 20 << 20]:
        data = os.urandom(size)
        name = "f%d.bin" % size
        t = time.time()
        upload(f, name, data)
        dt = time.time() - t
        on_disk = open(os.path.join(root, "t", name), "rb").read()
        check(on_disk == data, "STOR %d bytes intact (%.1f MB/s)" % (size, size / 1048576 / max(dt, 1e-6)))
        check(f.size(name) == size, "SIZE %d" % size)
        check(download(f, name) == data, "RETR %d bytes intact" % size)
    check(download(f, "f%d.bin" % (20 << 20), rest=12345) ==
          open(os.path.join(root, "t", "f%d.bin" % (20 << 20)), "rb").read()[12345:], "RETR with REST")

    print("listing")
    entries = mlsd(f, "/t")
    check(entries.get("f1.bin", {}).get("type") == "file" and entries["f1.bin"]["size"] == "1", "MLSD file facts")
    entries = mlsd(f, "/")
    check(entries.get("t", {}).get("type") == "dir", "MLSD dir facts")
    lines = []
    f.retrlines("LIST", lines.append)
    check(any(l.startswith("-rw") and l.endswith(" f1.bin") for l in lines), "LIST unix format")
    check("f1.bin" in f.nlst(), "NLST")

    print("unicode names")
    uname = "été 日本 ünï.bin"
    f.encoding = "utf-8"
    upload(f, uname, b"hello")
    check(os.path.exists(os.path.join(root, "t", uname)), "UTF-8 file name on disk")
    check(uname in mlsd(f, "/t"), "UTF-8 name listed")

    print("resume an interrupted upload")
    data = os.urandom(6 << 20)
    f.voidcmd("TYPE I")
    conn = f.transfercmd("STOR resume.bin")
    conn.sendall(data[: 3 << 20])
    conn.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, b"\x01\x00\x00\x00\x00\x00\x00\x00")
    conn.close()  # RST: simulates the connection dropping
    try:
        f.voidresp()
    except ftplib.Error:
        pass
    have = f.size("resume.bin")
    check(0 < have <= 3 << 20, "partial file kept (%d bytes)" % have)
    f.storbinary("STOR resume.bin", io.BytesIO(data[have:]), rest=have)
    check(open(os.path.join(root, "t", "resume.bin"), "rb").read() == data, "resumed upload intact")

    print("rename / delete")
    f.rename("f1.bin", "renamed.bin")
    check(os.path.exists(os.path.join(root, "t", "renamed.bin")), "RNFR/RNTO file")
    f.mkd("sub")
    upload(f, "sub/x.bin", b"x")
    f.rename("sub", "sub2")
    check(os.path.isdir(os.path.join(root, "t", "sub2")), "RNFR/RNTO folder")
    f.delete("renamed.bin")
    check(not os.path.exists(os.path.join(root, "t", "renamed.bin")), "DELE")
    f.rmd("sub2")
    check(not os.path.exists(os.path.join(root, "t", "sub2")), "RMD removes non-empty folder")
    try:
        f.rmd("/")
        check(False, "RMD / refused")
    except ftplib.error_perm:
        check(True, "RMD / refused")

    print("SD card only (no escaping the root)")
    f.cwd("/../../..")
    check(f.pwd() == "/", "CWD .. stops at /")
    name = os.path.basename(root) + "-outside.txt"
    for path in ["../" + name, "/../" + name, "..\\" + name]:
        try:
            download(f, path)
            check(False, "RETR %s refused" % path)
        except ftplib.error_perm:
            check(True, "RETR %s refused" % path)

    print("parallel sessions")
    results = {}

    def worker(i):
        g = connect()
        d = os.urandom(8 << 20)
        upload(g, "/t/par%d.bin" % i, d)
        results[i] = download(g, "/t/par%d.bin" % i) == d
        g.quit()

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(3)]
    [t.start() for t in threads]
    [t.join() for t in threads]
    check(all(results.get(i) for i in range(3)), "3 parallel upload+download sessions")

    print("abort a download")
    conn = f.transfercmd("RETR /t/f%d.bin" % (20 << 20))
    conn.recv(65536)
    conn.close()
    try:
        f.voidresp()
    except ftplib.Error:
        pass
    check(f.sendcmd("NOOP").startswith("200"), "session usable after aborted RETR")

    print("lid close (sleep) and wake")
    open(flag, "w").close()
    time.sleep(0.5)
    try:
        f.sendcmd("NOOP")
        f.sendcmd("NOOP")
        check(False, "connection dropped on sleep")
    except (OSError, EOFError, ftplib.Error):
        check(True, "connection dropped on sleep")
    os.remove(flag)
    g = connect()
    check(g.pwd() == "/", "reconnect after wake")
    check(f"f{CHUNK}.bin" in g.nlst("/t"), "data still there after wake")
    g.quit()


if __name__ == "__main__":
    main()
