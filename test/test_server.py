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


def fake_cia(title_id, size):
    """Minimal CIA layout: header, cert chain, ticket, then a TMD whose
    title ID sits where FBI's tmd_get_title_id reads it."""
    header, cert, ticket = 0x2020, 0xA00, 0x350
    align = lambda v: (v + 0x3F) & ~0x3F
    tmd = align(header) + align(cert) + align(ticket)
    data = bytearray(os.urandom(max(size, tmd + 0x200)))
    data[0:4] = header.to_bytes(4, "little")
    data[8:12] = cert.to_bytes(4, "little")
    data[12:16] = ticket.to_bytes(4, "little")
    data[tmd:tmd + 4] = (0x00010004).to_bytes(4, "big")  # RSA-2048 SHA-256 -> 0x140 bytes
    data[tmd + 0x140 + 0x4C: tmd + 0x140 + 0x54] = title_id.to_bytes(8, "big")
    return bytes(data)


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
    os.environ["SYNAFTP_AMOUT"] = os.path.abspath(root + "-am.bin")
    os.environ["SYNAFTP_KEYS"] = os.path.abspath(root + "-keys.txt")
    env = dict(os.environ, SYNAFTP_ROOT=root, SYNAFTP_SLEEPFLAG=flag)
    srv = subprocess.Popen([exe], env=env, stderr=subprocess.PIPE, text=True)
    try:
        run(root, flag)
    finally:
        srv.kill()
        log = srv.stderr.read()
        print("server log:", log.strip().replace("\n", " | "))
        check(log.count("AM_StartCiaInstall media=1") == 1, "exactly one install started, to SD")
        check("AM_DeleteTitle media=1 id=0004000000123400" in log, "old copy of the title removed first (as FBI)")
        check("sleep allowed=0" in log and "sleep allowed=1" in log, "sleep blocked during install, then allowed")
        if os.path.exists(os.environ["SYNAFTP_AMOUT"]):
            os.remove(os.environ["SYNAFTP_AMOUT"])
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

    print("CIA install from the bottom screen")
    am_out = os.environ["SYNAFTP_AMOUT"]
    keys = os.environ["SYNAFTP_KEYS"]

    def press(key):
        with open(keys, "w") as fh:
            fh.write(key)
        for _ in range(100):
            if not os.path.exists(keys):
                break
            time.sleep(0.02)
        time.sleep(0.5)

    good = fake_cia(0x0004000000123400, 3 * CHUNK + 99)
    f.mkd("/cias")
    upload(f, "/cias/good.cia", good)
    upload(f, "/cias/bad.cia", os.urandom(5000))
    upload(f, "/cias/n3ds only.CIA", fake_cia(0x0004000020123400, 4096))
    upload(f, "/cias/not-a-cia.bin", b"x")
    # installs wait while a transfer runs or right after a deletion
    g = connect()
    g.voidcmd("TYPE I")
    slow = g.transfercmd("STOR /cias/slow.bin")
    slow.sendall(b"x" * 100000)
    press("A")
    check(not os.path.exists(am_out), "install refused while an upload is running")
    slow.close()
    g.voidresp()
    g.quit()
    f.delete("/cias/not-a-cia.bin")
    press("A")
    check(not os.path.exists(am_out), "install refused right after a deletion")
    time.sleep(2.2)

    press("A")  # first entry: good.cia
    check(os.path.exists(am_out) and open(am_out, "rb").read() == good, "CIA streamed to AM intact")
    check(not os.path.exists(os.path.join(root, "cias", "good.cia")), "installed CIA deleted from SD")
    os.remove(am_out)
    press("A")  # list is now bad.cia, n3ds only.CIA
    check(os.path.exists(os.path.join(root, "cias", "bad.cia")), "invalid CIA kept after failed install")
    check(not os.path.exists(am_out), "invalid CIA never reached AM")
    press("DOWN")
    press("A")
    check(os.path.exists(os.path.join(root, "cias", "n3ds only.CIA")), "New 3DS title refused on Old 3DS, file kept")
    check(not os.path.exists(am_out), "New 3DS title never reached AM")

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
