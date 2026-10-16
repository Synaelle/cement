// syna3ds - command-line access to the Syna 3DS Tools server, for agents and
// scripts. Uses the same FTP engine (ftp.h) and saved settings as the GUI.
//
// Non-interactive: results on stdout (text or --json), progress on stderr.
// Exit codes: 0 ok, 1 usage, 2 3DS unreachable, 3 operation failed, 4 still busy.
// Commands that change files first wait until no other transfer is running,
// announce their own job to the 3DS (SITE BATCH) and resume after dropped links.

#include "ftp.h"

#include <shlobj.h>

#include <chrono>
#include <thread>

static int const EXIT_OK = 0, EXIT_USAGE = 1, EXIT_UNREACHABLE = 2, EXIT_FAILED = 3, EXIT_BUSY = 4;

using Clock = std::chrono::steady_clock;

struct Unreachable
{
};
struct Busy
{
	string what;
};
struct UsageError
{
	string msg;
};

static bool g_json, g_quiet;
static bool g_tty; // stderr is a console: progress can redraw one line

// ---------------------------------------------------------------------------
// output

static void out (string const &s) { fwrite (s.data (), 1, s.size (), stdout); }

static void note (string const &s)
{
	if (g_quiet)
		return;
	if (g_tty)
		fputs ("\r\x1b[K", stderr);
	fwrite (s.data (), 1, s.size (), stderr);
	fputc ('\n', stderr);
	fflush (stderr);
}

static string jsonStr (string const &s)
{
	string o = "\"";
	for (unsigned char c : s)
	{
		if (c == '"' || c == '\\')
			o += '\\', o += (char)c;
		else if (c < 0x20)
		{
			char buf[8];
			snprintf (buf, sizeof (buf), "\\u%04x", c);
			o += buf;
		}
		else
			o += (char)c;
	}
	return o + "\"";
}

static string sizeText (uint64_t v) { return utf8 (formatSize (v)); }

static string timeText (long long s)
{
	if (s < 0)
		return "unknown time";
	char buf[32];
	snprintf (buf, sizeof (buf), "%lld:%02lld", s / 60, s % 60);
	return buf;
}

static string firstLine (string const &s) { return s.substr (0, s.find_first_of ("\r\n")); }

// ---------------------------------------------------------------------------
// settings (same file as the GUI)

static bool loadAddress (string &host, int &port)
{
	wchar_t path[MAX_PATH];
	wstring file;
	if (GetEnvironmentVariableW (L"SYNA3DS_CONFIG", path, MAX_PATH))
		file = path;
	else
	{
		PWSTR base = nullptr;
		if (SUCCEEDED (SHGetKnownFolderPath (FOLDERID_RoamingAppData, 0, nullptr, &base)))
		{
			file = wstring (base) + L"\\Syna 3DS Tools\\config.ini";
			if (GetFileAttributesW (file.c_str ()) == INVALID_FILE_ATTRIBUTES)
				file = wstring (base) + L"\\synaftp\\config.ini"; // before the rename
		}
		CoTaskMemFree (base);
	}
	wchar_t h[256] = {};
	GetPrivateProfileStringW (L"server", L"host", L"", h, 256, file.c_str ());
	if (!h[0])
		return false;
	host = utf8 (h);
	port = GetPrivateProfileIntW (L"server", L"port", 5000, file.c_str ());
	return true;
}

// ---------------------------------------------------------------------------
// connection with retry

class Link
{
public:
	Link (string host, int port, int waitSec) : m_host (std::move (host)), m_port (port), m_wait (waitSec) {}

	string address () const { return m_host + ":" + std::to_string (m_port); }

	Ftp &ftp () { return m_ftp; }

	// what is left of our job; re-announced after a reconnect
	void batch (uint64_t files, uint64_t bytes, char const *kind)
	{
		m_batchFiles = files;
		m_batchBytes = bytes;
		m_batchKind  = kind;
	}

	// runs f, reconnecting (up to --wait seconds) whenever the connection drops
	template <class F>
	void run (F f)
	{
		for (;;)
		{
			connect ();
			try
			{
				f ();
				return;
			}
			catch (NetError &)
			{
				m_ftp.close ();
				note ("connection lost, reconnecting...");
			}
		}
	}

private:
	void connect ()
	{
		if (m_ftp.connected ())
			return;
		auto deadline = Clock::now () + std::chrono::seconds (m_wait);
		bool told     = false;
		while (!m_ftp.connect (m_host, m_port))
		{
			if (Clock::now () >= deadline)
				throw Unreachable ();
			if (!told)
				note ("waiting for the 3DS at " + address () + "...");
			told = true;
			std::this_thread::sleep_for (std::chrono::seconds (1));
		}
		if (m_batchFiles)
		{
			try
			{
				m_ftp.announceBatch (m_batchFiles, m_batchBytes, m_batchKind);
			}
			catch (NetError &)
			{
				m_ftp.close (); // handled by the next attempt
			}
		}
	}

	string m_host;
	int m_port;
	int m_wait;
	Ftp m_ftp;
	uint64_t m_batchFiles = 0, m_batchBytes = 0;
	char const *m_batchKind = "up";
};

// ---------------------------------------------------------------------------
// what the 3DS is doing (SITE STATUS)

struct Transfer
{
	string peer, dir, file, batch;
	uint64_t done = 0, size = 0, bps = 0;
	long long eta = -1;
	uint64_t batchFilesDone = 0, batchFiles = 0, batchBytesDone = 0, batchBytes = 0;
	long long batchEta = -1;
};

struct Status
{
	bool known = false; // false: server without SITE STATUS
	bool busy = false, installing = false;
	uint64_t idleMs = 0;
	int connections = 0;
	string version;
	std::vector<Transfer> transfers;
	uint64_t installDone = 0, installSize = 0;
	string installFile;
};

// "key=value key=value ... file=<rest of the line>"
static std::vector<std::pair<string, string>> fields (string line)
{
	std::vector<std::pair<string, string>> kv;
	size_t pos = 0;
	while (pos < line.size ())
	{
		while (pos < line.size () && line[pos] == ' ')
			++pos;
		size_t eq = line.find ('=', pos);
		if (eq == string::npos)
			break;
		string key = line.substr (pos, eq - pos);
		size_t end = key == "file" ? line.size () : line.find (' ', eq);
		if (end == string::npos)
			end = line.size ();
		kv.emplace_back (key, line.substr (eq + 1, end - eq - 1));
		pos = end;
	}
	return kv;
}

static Status queryStatus (Link &link)
{
	Status st;
	string text;
	int code = 0;
	link.run ([&] { code = link.ftp ().cmd ("SITE STATUS", &text); });
	if (code != 211)
		return st;
	st.known = true;
	size_t pos = 0;
	while (pos < text.size ())
	{
		size_t nl   = text.find ('\n', pos);
		string line = text.substr (pos, nl == string::npos ? string::npos : nl - pos);
		pos         = nl == string::npos ? text.size () : nl + 1;
		if (line.empty () || line[0] != ' ')
			continue;
		line.erase (0, 1);
		string kind = line.substr (0, line.find (' '));
		auto num    = [] (string const &v) { return _strtoui64 (v.c_str (), nullptr, 10); };
		if (kind.rfind ("busy=", 0) == 0)
		{
			for (auto const &[k, v] : fields (line))
			{
				if (k == "busy")
					st.busy = v == "1";
				else if (k == "idle_ms")
					st.idleMs = num (v);
				else if (k == "installing")
					st.installing = v == "1";
				else if (k == "connections")
					st.connections = (int)num (v);
				else if (k == "version")
					st.version = v;
			}
		}
		else if (kind == "transfer")
		{
			Transfer t;
			for (auto const &[k, v] : fields (line.substr (9)))
			{
				if (k == "peer")
					t.peer = v;
				else if (k == "dir")
					t.dir = v;
				else if (k == "done")
					t.done = num (v);
				else if (k == "size")
					t.size = num (v);
				else if (k == "bps")
					t.bps = num (v);
				else if (k == "eta")
					t.eta = atoll (v.c_str ());
				else if (k == "batch")
					t.batch = v;
				else if (k == "batch_files_done")
					t.batchFilesDone = num (v);
				else if (k == "batch_files")
					t.batchFiles = num (v);
				else if (k == "batch_bytes_done")
					t.batchBytesDone = num (v);
				else if (k == "batch_bytes")
					t.batchBytes = num (v);
				else if (k == "batch_eta")
					t.batchEta = atoll (v.c_str ());
				else if (k == "file")
					t.file = v;
			}
			st.transfers.push_back (t);
		}
		else if (kind == "install")
		{
			for (auto const &[k, v] : fields (line.substr (8)))
			{
				if (k == "done")
					st.installDone = num (v);
				else if (k == "size")
					st.installSize = num (v);
				else if (k == "file")
					st.installFile = v;
			}
		}
	}
	return st;
}

// one line per activity, for humans and for "waiting because ..." notes
static std::vector<string> describe (Status const &st)
{
	std::vector<string> lines;
	for (auto const &t : st.transfers)
	{
		if (t.dir == "up" || t.dir == "down")
		{
			string l = string (t.dir == "up" ? "upload " : "download ") + t.file + "  " + sizeText (t.done);
			if (t.size)
				l += " / " + sizeText (t.size);
			if (t.bps)
				l += "  " + sizeText (t.bps) + "/s";
			if (t.eta >= 0)
				l += "  " + timeText (t.eta) + " left";
			lines.push_back (l + "  (from " + t.peer + ")");
		}
		if (!t.batch.empty ())
		{
			string l = (t.batch == "delete" ? "delete job: " : t.batch == "down" ? "download job: " : "upload job: ") +
			           std::to_string (t.batchFilesDone) + "/" + std::to_string (t.batchFiles) + (t.batch == "delete" ? " items" : " files");
			if (t.batchBytes)
				l += ", " + sizeText (t.batchBytesDone) + " / " + sizeText (t.batchBytes);
			l += ", " + timeText (t.batchEta) + " left  (from " + t.peer + ")";
			lines.push_back (l);
		}
	}
	if (st.installing)
		lines.push_back ("installing CIA " + st.installFile + " " +
		                 std::to_string (st.installSize ? st.installDone * 100 / st.installSize : 0) + "%");
	if (st.busy && lines.empty ())
		lines.push_back ("files were just changed (deletion or rename in progress)");
	return lines;
}

static string statusJson (Status const &st, string const &address)
{
	string transfers;
	for (auto const &t : st.transfers)
	{
		char buf[512];
		snprintf (buf,
		    sizeof (buf),
		    "{\"peer\":%s,\"direction\":%s,\"file\":%s,\"done\":%llu,\"size\":%llu,\"bytes_per_second\":%llu,\"eta_seconds\":%lld,"
		    "\"job\":%s}",
		    jsonStr (t.peer).c_str (),
		    jsonStr (t.dir).c_str (),
		    jsonStr (t.file).c_str (),
		    (unsigned long long)t.done,
		    (unsigned long long)t.size,
		    (unsigned long long)t.bps,
		    t.eta,
		    t.batch.empty () ? "null" : [&] {
			    static char job[256];
			    snprintf (job,
			        sizeof (job),
			        "{\"kind\":%s,\"files_done\":%llu,\"files\":%llu,\"bytes_done\":%llu,\"bytes\":%llu,\"eta_seconds\":%lld}",
			        jsonStr (t.batch).c_str (),
			        (unsigned long long)t.batchFilesDone,
			        (unsigned long long)t.batchFiles,
			        (unsigned long long)t.batchBytesDone,
			        (unsigned long long)t.batchBytes,
			        t.batchEta);
			    return (char const *)job;
		    }());
		transfers += (transfers.empty () ? "" : ",") + string (buf);
	}
	string install = "null";
	if (st.installing)
		install = "{\"file\":" + jsonStr (st.installFile) + ",\"done\":" + std::to_string (st.installDone) +
		          ",\"size\":" + std::to_string (st.installSize) + "}";
	return "{\"ok\":true,\"address\":" + jsonStr (address) + ",\"version\":" + jsonStr (st.version) +
	       ",\"status_supported\":" + (st.known ? "true" : "false") + ",\"busy\":" + (st.busy ? "true" : "false") +
	       ",\"idle_ms\":" + std::to_string (st.idleMs) + ",\"connections\":" + std::to_string (st.connections) +
	       ",\"transfers\":[" + transfers + "],\"install\":" + install + "}\n";
}

// blocks until nothing else is transferring, up to budget seconds
static void waitIdle (Link &link, int budget)
{
	auto start    = Clock::now ();
	auto lastNote = Clock::time_point{};
	for (;;)
	{
		Status st = queryStatus (link);
		if (!st.known || !st.busy)
			return;
		auto lines = describe (st);
		string what;
		for (auto const &l : lines)
			what += (what.empty () ? "" : "; ") + l;
		if (Clock::now () - start >= std::chrono::seconds (budget))
			throw Busy{what};
		if (Clock::now () - lastNote >= std::chrono::seconds (15))
		{
			note ("3DS busy, waiting: " + what);
			lastNote = Clock::now ();
		}
		std::this_thread::sleep_for (std::chrono::seconds (2));
	}
}

// ---------------------------------------------------------------------------
// helpers

// "/a/b": forward slashes, leading slash, no trailing slash
static wstring remotePath (string const &arg)
{
	wstring p = wide (arg);
	for (auto &c : p)
		if (c == L'\\')
			c = L'/';
	wstring out;
	for (wchar_t c : L"/" + p)
		if (!(c == L'/' && !out.empty () && out.back () == L'/'))
			out += c;
	if (out.size () > 1 && out.back () == L'/')
		out.pop_back ();
	return out;
}

static wstring baseName (wstring const &p) { return p.substr (p.find_last_of (L"/\\") + 1); }

// looks the item up in its parent folder; false if it does not exist
static bool statRemote (Link &link, wstring const &path, Entry &out)
{
	if (path == L"/")
	{
		out = {L"/", true, 0};
		return true;
	}
	bool found = false;
	link.run ([&] {
		found = false;
		std::vector<Entry> siblings;
		try
		{
			siblings = link.ftp ().list (parentRemote (path));
		}
		catch (FtpError &)
		{
			return; // parent missing
		}
		wstring name = baseName (path);
		for (auto const &e : siblings)
			if (e.name == name)
				out = e, found = true;
	});
	return found;
}

static void mkdir1 (Link &link, wstring const &path)
{
	link.run ([&] {
		string text;
		link.ftp ().cmd ("MKD " + utf8 (path), &text); // fails harmlessly when it exists
	});
}

static void mkdirs (Link &link, wstring const &path)
{
	for (size_t pos = 1; pos <= path.size ();)
	{
		size_t next = path.find (L'/', pos);
		if (next == wstring::npos)
			next = path.size ();
		mkdir1 (link, path.substr (0, next));
		pos = next + 1;
	}
	Entry e;
	if (path != L"/" && (!statRemote (link, path, e) || !e.dir))
		throw FtpError{"could not create folder " + utf8 (path)};
}

struct Item
{
	wstring local, remote;
	bool dir;
	uint64_t size;
};

struct Totals
{
	int files = 0, folders = 0;
	uint64_t bytes = 0;
	std::vector<string> errors;
	Clock::time_point start = Clock::now ();

	double seconds () const { return std::chrono::duration<double> (Clock::now () - start).count (); }
};

// progress callback: one redrawn line on a console, nothing when captured
static std::function<void (uint64_t)> progressLine (wstring const &name, uint64_t size, uint64_t start)
{
	auto done = std::make_shared<uint64_t> (start);
	auto last = std::make_shared<Clock::time_point> ();
	return [=] (uint64_t n) {
		*done += n;
		auto now = Clock::now ();
		if (!g_tty || g_quiet || now - *last < std::chrono::milliseconds (200))
			return;
		*last = now;
		fprintf (stderr, "\r\x1b[K  %s  %s / %s", utf8 (name).c_str (), sizeText (*done).c_str (), sizeText (size).c_str ());
		fflush (stderr);
	};
}

static void collectLocal (wstring const &local, wstring const &remoteDir, std::vector<Item> &items, Totals &t)
{
	WIN32_FILE_ATTRIBUTE_DATA fa;
	if (!GetFileAttributesExW (local.c_str (), GetFileExInfoStandard, &fa))
	{
		t.errors.push_back ("local path not found: " + utf8 (local));
		return;
	}
	wstring remote = joinRemote (remoteDir, baseName (local));
	if (!(fa.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY))
	{
		items.push_back ({local, remote, false, ((uint64_t)fa.nFileSizeHigh << 32) | fa.nFileSizeLow});
		return;
	}
	items.push_back ({local, remote, true, 0});
	WIN32_FIND_DATAW fd;
	HANDLE h = FindFirstFileW ((local + L"\\*").c_str (), &fd);
	if (h == INVALID_HANDLE_VALUE)
		return;
	do
		if (wcscmp (fd.cFileName, L".") && wcscmp (fd.cFileName, L".."))
			collectLocal (local + L"\\" + fd.cFileName, remote, items, t);
	while (FindNextFileW (h, &fd));
	FindClose (h);
}

static void collectRemote (Link &link, wstring const &remote, Entry const &e, wstring const &local, std::vector<Item> &items)
{
	items.push_back ({local, remote, e.dir, e.size});
	if (!e.dir)
		return;
	std::vector<Entry> children;
	link.run ([&] { children = link.ftp ().list (remote); });
	for (auto const &c : children)
		collectRemote (link, joinRemote (remote, c.name), c, local + L"\\" + c.name, items);
}

static void collectDelete (Link &link, wstring const &path, bool dir, std::vector<Item> &items)
{
	if (dir)
	{
		std::vector<Entry> children;
		link.run ([&] { children = link.ftp ().list (path); });
		for (auto const &c : children)
			collectDelete (link, joinRemote (path, c.name), c.dir, items);
	}
	items.push_back ({L"", path, dir, 0}); // contents first, the folder last
}

static void uploadFile (Link &link, Item const &it)
{
	bool retry = false;
	link.run ([&] {
		uint64_t offset = 0;
		if (retry) // resume after a dropped connection
		{
			uint64_t have = 0;
			if (link.ftp ().size (it.remote, have) && have <= it.size)
				offset = have;
		}
		retry = true;
		link.ftp ().upload (it.local, it.remote, offset, progressLine (baseName (it.remote), it.size, offset));
	});
	uint64_t have = 0;
	link.run ([&] { link.ftp ().size (it.remote, have); });
	if (have != it.size)
		throw FtpError{"size check failed for " + utf8 (it.remote) + " (" + std::to_string (have) + " of " +
		               std::to_string (it.size) + " bytes on the 3DS)"};
}

static void downloadFile (Link &link, Item const &it)
{
	bool retry = false;
	link.run ([&] {
		uint64_t offset = 0;
		WIN32_FILE_ATTRIBUTE_DATA fa;
		if (retry && GetFileAttributesExW (it.local.c_str (), GetFileExInfoStandard, &fa))
			offset = std::min (it.size, ((uint64_t)fa.nFileSizeHigh << 32) | fa.nFileSizeLow);
		retry = true;
		link.ftp ().download (it.remote, it.local, offset, progressLine (baseName (it.remote), it.size, offset));
	});
}

static void printSummary (char const *verb, Totals const &t)
{
	double s    = t.seconds ();
	double rate = s > 0 ? t.bytes / s : 0;
	if (g_json)
	{
		string errs;
		for (auto const &e : t.errors)
			errs += (errs.empty () ? "" : ",") + jsonStr (e);
		char buf[256];
		snprintf (buf,
		    sizeof (buf),
		    "{\"ok\":%s,\"files\":%d,\"folders\":%d,\"bytes\":%llu,\"seconds\":%.2f,\"bytes_per_second\":%.0f,\"errors\":[",
		    t.errors.empty () ? "true" : "false",
		    t.files,
		    t.folders,
		    (unsigned long long)t.bytes,
		    s,
		    rate);
		out (buf + errs + "]}\n");
		return;
	}
	char buf[256];
	snprintf (buf, sizeof (buf), "%s %d file(s), %d folder(s), %s in %.1f s (%s/s)\n", verb, t.files, t.folders,
	    sizeText (t.bytes).c_str (), s, sizeText ((uint64_t)rate).c_str ());
	out (buf);
	for (auto const &e : t.errors)
		out ("error: " + e + "\n");
}

// ---------------------------------------------------------------------------
// commands

struct Options
{
	int busyWait = 600;
	bool noWait  = false;
};

static int run (Link &link, string const &cmd, std::vector<string> const &args, Options const &opt)
{
	auto need = [&] (size_t n) {
		if (args.size () < n)
			throw UsageError{"'" + cmd + "' needs more arguments"};
	};
	// commands that change files: never start on top of another transfer
	auto idle = [&] {
		if (!opt.noWait)
			waitIdle (link, opt.busyWait);
	};

	if (cmd == "status")
	{
		Status st = queryStatus (link);
		if (g_json)
			out (statusJson (st, link.address ()));
		else
		{
			out ("connected to " + link.address () + (st.version.empty () ? "" : " (Syna 3DS Tools " + st.version + ")") + "\n");
			if (!st.known)
				out ("this server does not report its activity (SITE STATUS)\n");
			else if (!st.busy)
				out ("idle for " + std::to_string (st.idleMs / 1000) + " s\n");
			else
			{
				out ("busy:\n");
				for (auto const &l : describe (st))
					out ("  " + l + "\n");
			}
		}
		return EXIT_OK;
	}

	if (cmd == "wait")
	{
		waitIdle (link, opt.busyWait);
		out (g_json ? "{\"ok\":true,\"busy\":false}\n" : "idle\n");
		return EXIT_OK;
	}

	if (cmd == "ls")
	{
		wstring path = remotePath (args.empty () ? "/" : args[0]);
		std::vector<Entry> entries;
		string error;
		link.run ([&] {
			try
			{
				entries = link.ftp ().list (path);
			}
			catch (FtpError &e)
			{
				error = firstLine (e.msg);
			}
		});
		std::sort (entries.begin (), entries.end (), [] (Entry const &a, Entry const &b) {
			return a.dir != b.dir ? a.dir : _wcsicmp (a.name.c_str (), b.name.c_str ()) < 0;
		});
		if (g_json)
		{
			string list;
			for (auto const &e : entries)
				list += (list.empty () ? "" : ",") + string ("{\"name\":") + jsonStr (utf8 (e.name)) + ",\"type\":\"" +
				        (e.dir ? "dir" : "file") + "\",\"size\":" + std::to_string (e.size) + "}";
			out ("{\"ok\":" + string (error.empty () ? "true" : "false") + ",\"path\":" + jsonStr (utf8 (path)) +
			     (error.empty () ? "" : ",\"error\":" + jsonStr (error)) + ",\"entries\":[" + list + "]}\n");
		}
		else if (error.empty ())
			for (auto const &e : entries)
			{
				char size[24] = "";
				if (!e.dir)
					snprintf (size, sizeof (size), "%llu", (unsigned long long)e.size);
				char line[64];
				snprintf (line, sizeof (line), "%-5s %12s  ", e.dir ? "dir" : "file", size);
				out (line + utf8 (e.name) + "\n");
			}
		else
			out ("error: " + error + "\n");
		return error.empty () ? EXIT_OK : EXIT_FAILED;
	}

	if (cmd == "stat")
	{
		need (1);
		wstring path = remotePath (args[0]);
		Entry e;
		bool found = statRemote (link, path, e);
		if (g_json)
			out (found ? "{\"ok\":true,\"path\":" + jsonStr (utf8 (path)) + ",\"type\":\"" + (e.dir ? "dir" : "file") +
			                 "\",\"size\":" + std::to_string (e.size) + "}\n"
			           : "{\"ok\":false,\"path\":" + jsonStr (utf8 (path)) + ",\"error\":\"not found\"}\n");
		else
			out (found ? string (e.dir ? "dir " : "file ") + std::to_string (e.size) + " " + utf8 (path) + "\n"
			           : "not found: " + utf8 (path) + "\n");
		return found ? EXIT_OK : EXIT_FAILED;
	}

	if (cmd == "put")
	{
		need (2);
		wstring dir = remotePath (args.back ());
		Totals t;
		std::vector<Item> items;
		for (size_t i = 0; i + 1 < args.size (); ++i)
		{
			wstring local = wide (args[i]);
			while (local.size () > 3 && (local.back () == L'\\' || local.back () == L'/'))
				local.pop_back ();
			collectLocal (local, dir, items, t);
		}
		uint64_t files = 0, bytes = 0;
		for (auto const &it : items)
			files += !it.dir, bytes += it.size;
		idle ();
		mkdirs (link, dir);
		link.batch (files, bytes, "up");
		link.run ([&] { link.ftp ().announceBatch (files, bytes, "up"); });
		for (auto const &it : items)
		{
			if (it.dir)
			{
				mkdir1 (link, it.remote);
				t.folders++;
				continue;
			}
			try
			{
				uploadFile (link, it);
				note ("put " + utf8 (it.remote) + " (" + sizeText (it.size) + ")");
				t.files++;
				t.bytes += it.size;
			}
			catch (FtpError &e)
			{
				t.errors.push_back (firstLine (e.msg));
			}
			link.batch (--files, bytes -= it.size, "up");
		}
		link.batch (0, 0, "up");
		link.run ([&] { link.ftp ().endBatch (); });
		printSummary ("uploaded", t);
		return t.errors.empty () ? EXIT_OK : EXIT_FAILED;
	}

	if (cmd == "get")
	{
		need (2);
		wstring dir = wide (args.back ());
		Totals t;
		std::vector<Item> items;
		for (size_t i = 0; i + 1 < args.size (); ++i)
		{
			wstring remote = remotePath (args[i]);
			Entry e;
			if (remote == L"/" || !statRemote (link, remote, e))
				t.errors.push_back ("not found on the 3DS: " + utf8 (remote));
			else
				collectRemote (link, remote, e, dir + L"\\" + e.name, items);
		}
		uint64_t files = 0, bytes = 0;
		for (auto const &it : items)
			files += !it.dir, bytes += it.size;
		idle ();
		SHCreateDirectoryExW (nullptr, dir.c_str (), nullptr);
		link.batch (files, bytes, "down");
		link.run ([&] { link.ftp ().announceBatch (files, bytes, "down"); });
		for (auto const &it : items)
		{
			if (it.dir)
			{
				CreateDirectoryW (it.local.c_str (), nullptr);
				t.folders++;
				continue;
			}
			try
			{
				downloadFile (link, it);
				note ("got " + utf8 (it.remote) + " (" + sizeText (it.size) + ")");
				t.files++;
				t.bytes += it.size;
			}
			catch (FtpError &e)
			{
				t.errors.push_back (firstLine (e.msg));
			}
			link.batch (--files, bytes -= it.size, "down");
		}
		link.batch (0, 0, "down");
		link.run ([&] { link.ftp ().endBatch (); });
		printSummary ("downloaded", t);
		return t.errors.empty () ? EXIT_OK : EXIT_FAILED;
	}

	if (cmd == "rm")
	{
		need (1);
		Totals t;
		std::vector<Item> items;
		for (auto const &arg : args)
		{
			wstring path = remotePath (arg);
			Entry e;
			if (path == L"/")
				throw UsageError{"refusing to delete the whole SD card"};
			if (!statRemote (link, path, e))
				t.errors.push_back ("not found on the 3DS: " + utf8 (path));
			else
				collectDelete (link, path, e.dir, items);
		}
		idle ();
		uint64_t left = items.size ();
		link.batch (left, 0, "delete");
		link.run ([&] { link.ftp ().announceBatch (left, 0, "delete"); });
		for (auto const &it : items)
		{
			int code = 0;
			string text;
			link.run ([&] { code = link.ftp ().cmd ((it.dir ? "RMD " : "DELE ") + utf8 (it.remote), &text); });
			// after a reconnect the item may already be gone: only report what still exists
			Entry still;
			if (code / 100 != 2 && statRemote (link, it.remote, still))
				t.errors.push_back (utf8 (it.remote) + ": " + firstLine (text));
			else
				(it.dir ? t.folders : t.files)++;
			link.batch (--left, 0, "delete");
		}
		link.batch (0, 0, "delete");
		link.run ([&] { link.ftp ().endBatch (); });
		printSummary ("deleted", t);
		return t.errors.empty () ? EXIT_OK : EXIT_FAILED;
	}

	if (cmd == "mkdir")
	{
		need (1);
		idle ();
		for (auto const &arg : args)
			mkdirs (link, remotePath (arg));
		out (g_json ? "{\"ok\":true}\n" : "ok\n");
		return EXIT_OK;
	}

	if (cmd == "mv")
	{
		need (2);
		wstring from = remotePath (args[0]), to = remotePath (args[1]);
		idle ();
		link.run ([&] {
			link.ftp ().simple ("RNFR " + utf8 (from), 350);
			link.ftp ().simple ("RNTO " + utf8 (to), 250);
		});
		out (g_json ? "{\"ok\":true}\n" : "ok\n");
		return EXIT_OK;
	}

	throw UsageError{"unknown command '" + cmd + "'"};
}

static char const *const USAGE =
    "syna3ds - talk to the Syna 3DS Tools server on the 3DS (SD card only)\n"
    "\n"
    "usage: syna3ds [options] <command> [arguments]\n"
    "\n"
    "commands:\n"
    "  status                         is the 3DS reachable, busy (transfers, ETA, CIA install) or idle\n"
    "  wait                           block until no transfer/deletion is running\n"
    "  ls [remote-dir]                list a folder (default /)\n"
    "  stat <remote>                  type and size of a file/folder\n"
    "  put <local>... <remote-dir>    upload files/folders into remote-dir (created if needed)\n"
    "  get <remote>... <local-dir>    download files/folders into local-dir\n"
    "  rm <remote>...                 delete files/folders (folders recursively)\n"
    "  mkdir <remote>...              create folders (with parents)\n"
    "  mv <from> <to>                 rename or move\n"
    "\n"
    "options:\n"
    "  --host <ip> --port <n>         3DS address (default: the one saved by the GUI)\n"
    "  --wait <seconds>               keep retrying an unreachable 3DS / dropped link (default 20)\n"
    "  --busy-wait <seconds>          how long put/get/rm/mkdir/mv and wait wait for other\n"
    "                                 transfers to finish (default 600)\n"
    "  --no-wait                      start even if another transfer is running\n"
    "  --json                         print one JSON object on stdout\n"
    "  --quiet                        no progress messages on stderr\n"
    "\n"
    "exit codes: 0 ok, 1 usage, 2 3DS unreachable, 3 operation failed, 4 still busy after --busy-wait\n"
    "remote paths are absolute on the SD card, e.g. /3ds/app/app.3dsx\n";

int wmain (int argc, wchar_t **argv)
{
	SetConsoleOutputCP (CP_UTF8);
	DWORD mode;
	g_tty = GetConsoleMode (GetStdHandle (STD_ERROR_HANDLE), &mode) != 0;
	if (g_tty)
		SetConsoleMode (GetStdHandle (STD_ERROR_HANDLE), mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);

	string host;
	int port = 0, wait = 20;
	Options opt;
	string cmd;
	std::vector<string> args;
	try
	{
		for (int i = 1; i < argc; ++i)
		{
			string a = utf8 (argv[i]);
			auto value = [&] () -> string {
				if (i + 1 >= argc)
					throw UsageError{a + " needs a value"};
				return utf8 (argv[++i]);
			};
			if (a == "--json")
				g_json = true;
			else if (a == "--quiet")
				g_quiet = true;
			else if (a == "--host")
				host = value ();
			else if (a == "--port")
				port = atoi (value ().c_str ());
			else if (a == "--wait")
				wait = std::max (0, atoi (value ().c_str ()));
			else if (a == "--busy-wait")
				opt.busyWait = std::max (0, atoi (value ().c_str ()));
			else if (a == "--no-wait")
				opt.noWait = true;
			else if (a == "-h" || a == "--help" || a == "help")
			{
				out (USAGE);
				return EXIT_OK;
			}
			else if (cmd.empty ())
				cmd = a;
			else
				args.push_back (a);
		}
		if (cmd.empty ())
			throw UsageError{"no command"};

		string savedHost;
		int savedPort = 5000;
		bool saved    = loadAddress (savedHost, savedPort);
		if (host.empty ())
		{
			if (!saved)
				throw UsageError{"no 3DS address saved yet: open Syna3DSTools.exe once or pass --host"};
			host = savedHost;
		}
		if (port == 0)
			port = saved ? savedPort : 5000;

		WSADATA wsa;
		WSAStartup (MAKEWORD (2, 2), &wsa);
		Link link (host, port, wait);
		auto fail = [&] (int code, string const &msg, char const *extra = "") {
			out (g_json ? "{\"ok\":false,\"error\":" + jsonStr (msg) + extra + "}\n" : "error: " + msg + "\n");
			return code;
		};
		try
		{
			int rc = run (link, cmd, args, opt);
			link.ftp ().close ();
			return rc;
		}
		catch (Unreachable &)
		{
			return fail (EXIT_UNREACHABLE,
			    "3DS not reachable at " + link.address () + " (is Syna 3DS Tools open on the 3DS, Wi-Fi on and the lid open?)");
		}
		catch (Busy &b)
		{
			return fail (EXIT_BUSY, "3DS still busy after " + std::to_string (opt.busyWait) + " s: " + b.what, ",\"busy\":true");
		}
		catch (FtpError &e)
		{
			return fail (EXIT_FAILED, firstLine (e.msg));
		}
	}
	catch (UsageError &e)
	{
		if (g_json)
			out ("{\"ok\":false,\"error\":" + jsonStr (e.msg) + "}\n");
		else
			fprintf (stderr, "syna3ds: %s\n\n%s", e.msg.c_str (), USAGE);
		return EXIT_USAGE;
	}
}
