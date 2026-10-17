"""Tests for cement-cli.exe (the command-line tool) against the PC build of the server.

usage: python test_cli.py <server_pc.exe> <cement-cli.exe>
"""
import ftplib
import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

HOST, PORT = "127.0.0.1", 5000
failures = []


def check(cond, what):
    print(("  ok   " if cond else "  FAIL ") + what)
    if not cond:
        failures.append(what)


def cli(*args, timeout=120):
    p = subprocess.run([CLI, "--host", HOST, "--port", str(PORT), "--json", "--quiet", *args],
                       capture_output=True, timeout=timeout)
    out = p.stdout.decode("utf-8", "replace").strip()
    try:
        data = json.loads(out.splitlines()[-1]) if out else {}
    except ValueError:
        data = {"raw": out}
    return p.returncode, data


def sha(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()


def main():
    global CLI
    server, CLI = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    work = tempfile.mkdtemp(prefix="cement-cli-test-")
    sd, local, back = (os.path.join(work, d) for d in ("sd", "local", "back"))
    for d in (sd, local, back):
        os.makedirs(d)
    flag = os.path.join(work, "sleep.flag")
    env = dict(os.environ, CEMENT_ROOT=sd, CEMENT_SLEEPFLAG=flag, CEMENT_KEYS=os.path.join(work, "keys"),
               CEMENT_AMOUT=os.path.join(work, "am.bin"))
    srv = subprocess.Popen([server], env=env, stderr=subprocess.DEVNULL)
    try:
        time.sleep(0.5)
        run(sd, local, back, flag)
    finally:
        srv.kill()
        shutil.rmtree(work, ignore_errors=True)
    print("\n%d failure(s)" % len(failures))
    sys.exit(1 if failures else 0)


def run(sd, local, back, flag):
    print("status / unreachable")
    rc, d = cli("status")
    check(rc == 0 and d.get("ok") and d.get("busy") is False and d.get("status_supported"), "status: idle")
    check(d.get("protocol") == 1 and d.get("app_outdated") is False, "same protocol level: no outdated warning")
    check(0 <= d.get("idle_ms", -1) < 60000 and "warning" not in d, "idle_ms counts from startup, no warning")
    p = subprocess.run([CLI, "--host", HOST, "--port", "5999", "--wait", "1", "--json", "status"], capture_output=True)
    check(p.returncode == 2 and json.loads(p.stdout)["ok"] is False, "unreachable 3DS -> exit code 2")

    print("put / ls / stat / get")
    tree = os.path.join(local, "game")
    os.makedirs(os.path.join(tree, "data", "empty"))
    with open(os.path.join(tree, "game.3dsx"), "wb") as f:
        f.write(os.urandom(3 << 20))
    with open(os.path.join(tree, "data", "café.bin"), "wb") as f:
        f.write(os.urandom(70000))
    single = os.path.join(local, "readme.txt")
    with open(single, "w") as f:
        f.write("hello")
    rc, d = cli("put", tree, single, "/3ds/new")
    check(rc == 0 and d.get("files") == 3 and d.get("folders") == 3 and not d.get("errors"), "put tree + file")
    check(sha(os.path.join(sd, "3ds", "new", "game", "game.3dsx")) == sha(os.path.join(tree, "game.3dsx")), "uploaded file intact")
    check(os.path.isdir(os.path.join(sd, "3ds", "new", "game", "data", "empty")), "empty folder uploaded")
    rc, d = cli("ls", "/3ds/new/game")
    names = {e["name"]: e for e in d.get("entries", [])}
    check(rc == 0 and names.get("data", {}).get("type") == "dir" and names.get("game.3dsx", {}).get("size") == 3 << 20, "ls --json")
    rc, d = cli("stat", "/3ds/new/game/data/café.bin")
    check(rc == 0 and d.get("type") == "file" and d.get("size") == 70000, "stat unicode file")
    rc, d = cli("stat", "/nope")
    check(rc == 3 and d.get("ok") is False, "stat missing -> exit code 3")
    rc, d = cli("get", "/3ds/new/game", "/3ds/new/readme.txt", back)
    check(rc == 0 and d.get("files") == 3, "get folder + file")
    check(sha(os.path.join(back, "game", "data", "café.bin")) == sha(os.path.join(tree, "data", "café.bin")), "downloaded file intact")

    print("sd: paths, --contents, --progress-json")
    rc, d = cli("ls", "sd:/3ds/new")
    check(rc == 0 and {e["name"] for e in d.get("entries", [])} == {"game", "readme.txt"}, "sd:/ prefix accepted")
    rc, d = cli("stat", "sd:3ds/new/readme.txt")
    check(rc == 0 and d.get("size") == 5, "sd: without slash accepted")
    flat = os.path.join(back, "flat")
    rc, d = cli("--contents", "get", "/3ds/new/game", flat)
    check(rc == 0 and os.path.isfile(os.path.join(flat, "game.3dsx")) and os.path.isdir(os.path.join(flat, "data"))
          and not os.path.exists(os.path.join(flat, "game")), "get --contents copies the folder's contents")
    p = subprocess.run([CLI, "--host", HOST, "--port", str(PORT), "--quiet", "--progress-json", "put", tree, "/3ds/again"],
                       capture_output=True)
    lines = [json.loads(l) for l in p.stdout.decode().strip().splitlines()]
    files = [l for l in lines if l.get("event") == "file"]
    check(p.returncode == 0 and len(files) == 2 and files[-1]["files_done"] == 2 and files[-1]["files"] == 2,
          "--progress-json: one line per finished file")
    check(lines[-1].get("event") == "done" and lines[-1].get("files") == 2, "--progress-json: summary is the last line")

    print("mkdir / mv / rm")
    rc, d = cli("mkdir", "/a/b/c")
    check(rc == 0 and os.path.isdir(os.path.join(sd, "a", "b", "c")), "mkdir with parents")
    rc, d = cli("mv", "/3ds/new/readme.txt", "/a/b/readme.txt")
    check(rc == 0 and os.path.exists(os.path.join(sd, "a", "b", "readme.txt")), "mv")
    rc, d = cli("rm", "/3ds/new", "/a")
    check(rc == 0 and not os.path.exists(os.path.join(sd, "3ds", "new")) and not os.path.exists(os.path.join(sd, "a")), "rm folders recursively")
    rc, d = cli("rm", "/")
    check(rc == 1 and os.path.isdir(os.path.join(sd, "3ds")), "rm / refused")

    print("busy: another PC is transferring")
    other = ftplib.FTP()
    other.connect(HOST, PORT, timeout=10)
    other.login()
    other.voidcmd("TYPE I")
    other.voidcmd("SITE BATCH 2 20000000 up")
    other.voidcmd("ALLO 10000000")
    conn = other.transfercmd("STOR /big.bin")
    conn.sendall(os.urandom(2 << 20))
    time.sleep(1.0)
    rc, d = cli("status")
    t = (d.get("transfers") or [{}])[0]
    check(d.get("busy") is True and t.get("direction") == "up" and t.get("file") == "/big.bin", "status shows the other upload")
    check(t.get("size") == 10000000 and t.get("done", 0) >= 2 << 20 and t.get("eta_seconds", -1) >= 0, "size from ALLO, progress and ETA")
    job = t.get("job") or {}
    check(job.get("kind") == "up" and job.get("files") == 2 and job.get("bytes") == 20000000, "whole job reported (SITE BATCH)")
    rc, d = cli("--busy-wait", "3", "mkdir", "/x")
    check(rc == 4 and d.get("busy") is True and not os.path.exists(os.path.join(sd, "x")), "commands wait, then exit code 4")
    conn.close()
    other.voidresp()
    rc, d = cli("--busy-wait", "3", "wait")
    check(rc == 4, "still busy between files of the announced job")

    result = {}

    def waiter():
        result["rc"], result["d"] = cli("--busy-wait", "30", "mkdir", "/after")

    th = threading.Thread(target=waiter)
    th.start()
    time.sleep(2)
    check(not os.path.exists(os.path.join(sd, "after")), "a waiting command does not start early")
    other.voidcmd("SITE BATCH 0 0")
    th.join(60)
    check(result.get("rc") == 0 and os.path.isdir(os.path.join(sd, "after")), "it runs as soon as the job ends")
    other.quit()
    time.sleep(2.2)  # a folder change counts as busy for 2 s
    rc, d = cli("status")
    check(d.get("busy") is False, "idle again")

    print("the 3DS sleeps in the middle of a put, the CLI resumes")
    big = os.path.join(local, "big.bin")
    with open(big, "wb") as f:
        f.write(os.urandom(300 << 20))
    p = subprocess.Popen([CLI, "--host", HOST, "--port", str(PORT), "--json", "--quiet", "--wait", "30", "put", big, "/"],
                         stdout=subprocess.PIPE)
    deadline = time.time() + 20
    while time.time() < deadline and os.path.getsize(os.path.join(sd, "big.bin")) < 50 << 20 if os.path.exists(os.path.join(sd, "big.bin")) else time.time() < deadline:
        time.sleep(0.05)
    open(flag, "w").close()
    time.sleep(3)
    os.remove(flag)
    out, _ = p.communicate(timeout=120)
    d = json.loads(out.decode().strip().splitlines()[-1])
    check(p.returncode == 0 and d.get("files") == 1, "put finished after the 3DS woke up")
    check(sha(os.path.join(sd, "big.bin")) == sha(big), "resumed 300 MB file intact")


if __name__ == "__main__":
    main()
