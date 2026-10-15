// synaftp client - minimal Windows FTP explorer for the synaftp 3DS server
//
// - two FTP connections: one for browsing, one for transfers, so the
//   explorer stays responsive during uploads
// - both reconnect forever until the 3DS is reachable again; queued and
//   interrupted transfers resume automatically (SIZE + REST)
// - drag files in from Explorer to upload, drag files out to download

#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <commctrl.h>
#include <dwmapi.h>
#include <shellapi.h>
#include <shlobj.h>
#include <shobjidl.h>
#include <uxtheme.h>
#include <windowsx.h>

#include <algorithm>
#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "resource.h"

using std::string;
using std::wstring;

// ---------------------------------------------------------------------------
// utilities

static string utf8 (wstring const &w)
{
	if (w.empty ())
		return {};
	int n = WideCharToMultiByte (CP_UTF8, 0, w.data (), (int)w.size (), nullptr, 0, nullptr, nullptr);
	string s (n, 0);
	WideCharToMultiByte (CP_UTF8, 0, w.data (), (int)w.size (), s.data (), n, nullptr, nullptr);
	return s;
}

static wstring wide (string const &s)
{
	if (s.empty ())
		return {};
	int n = MultiByteToWideChar (CP_UTF8, 0, s.data (), (int)s.size (), nullptr, 0);
	wstring w (n, 0);
	MultiByteToWideChar (CP_UTF8, 0, s.data (), (int)s.size (), w.data (), n);
	return w;
}

static wstring joinRemote (wstring const &dir, wstring const &name)
{
	return dir == L"/" ? L"/" + name : dir + L"/" + name;
}

static wstring parentRemote (wstring const &dir)
{
	auto pos = dir.find_last_of (L'/');
	return pos == 0 || pos == wstring::npos ? L"/" : dir.substr (0, pos);
}

static wstring formatSize (uint64_t v)
{
	wchar_t buf[64];
	if (v >= 1ull << 30)
		swprintf (buf, 64, L"%.2f GB", v / 1073741824.0);
	else if (v >= 1ull << 20)
		swprintf (buf, 64, L"%.1f MB", v / 1048576.0);
	else if (v >= 1024)
		swprintf (buf, 64, L"%.1f KB", v / 1024.0);
	else
		swprintf (buf, 64, L"%llu B", (unsigned long long)v);
	return buf;
}

// ---------------------------------------------------------------------------
// shared state

enum
{
	WM_APP_STATE = WM_APP + 1, // a worker's connection state changed
	WM_APP_LIST,               // lParam: ListResult*
	WM_APP_MSG,                // lParam: wstring* (status message)
};

struct Entry
{
	wstring name;
	bool dir;
	uint64_t size;
};

struct ListResult
{
	wstring path;
	std::vector<Entry> entries;
	bool ok;
};

static HINSTANCE g_inst;
static HWND g_wnd, g_list, g_path, g_progress;
static HWND g_btnUp, g_btnRefresh, g_btnMkdir, g_btnTheme, g_btnReconnect, g_btnServer, g_btnCancel;
static bool g_dark = true;     // dark theme (saved in config)
static wstring g_statusText[2]; // connection state, transfer/messages
static void setStatus (int part, wstring const &text);
static HFONT g_font;
static UINT g_dpi = 96;

static std::mutex g_cfgLock;
static string g_host;
static int g_port = 5000;

static std::mutex g_cwdLock;
static wstring g_cwd = L"/";

static std::vector<Entry> g_entries; // what the list view shows (UI thread only)
static bool g_dragOut;               // we are the drag source
static std::atomic<bool> g_cancel;
static wstring g_lastMsg; // last status message (UI thread only)

static struct
{
	std::mutex lock;
	bool active = false;
	bool up     = true;
	wstring name;
	uint64_t done = 0, total = 0; // whole job
	int fileIndex = 0, fileCount = 0;
} g_prog;

static wstring currentDir ()
{
	std::lock_guard lk (g_cwdLock);
	return g_cwd;
}

static void postMessageText (wstring const &text)
{
	PostMessageW (g_wnd, WM_APP_MSG, 0, (LPARAM) new wstring (text));
}

// ---------------------------------------------------------------------------
// FTP protocol

struct NetError
{
};
struct CancelError
{
};
struct FtpError
{
	string msg;
};

class Ftp
{
public:
	~Ftp () { close (); }

	bool connected () const { return m_ctrl != INVALID_SOCKET; }

	bool connect (string const &host, int port)
	{
		close ();
		SOCKET s = connectTo (host, port, 3000);
		if (s == INVALID_SOCKET)
			return false;
		BOOL one = TRUE;
		setsockopt (s, IPPROTO_TCP, TCP_NODELAY, (char const *)&one, sizeof (one));
		setTimeout (s, 10000);
		{
			std::lock_guard lk (m_sockLock);
			m_ctrl = s;
		}
		m_host = host;
		m_rbuf.clear ();
		try
		{
			if (reply () != 220)
				throw NetError ();
			int code = cmd ("USER anonymous");
			if (code == 331)
				code = cmd ("PASS synaftp");
			if (code != 230)
				throw NetError ();
			cmd ("TYPE I");
			cmd ("OPTS UTF8 ON");
		}
		catch (NetError &)
		{
			close ();
			return false;
		}
		return true;
	}

	void close ()
	{
		std::lock_guard lk (m_sockLock);
		if (m_data != INVALID_SOCKET)
			closesocket (m_data);
		if (m_ctrl != INVALID_SOCKET)
			closesocket (m_ctrl);
		m_data = m_ctrl = INVALID_SOCKET;
	}

	// called from another thread: makes any blocking call fail right away
	void abort ()
	{
		std::lock_guard lk (m_sockLock);
		if (m_data != INVALID_SOCKET)
			shutdown (m_data, SD_BOTH);
		if (m_ctrl != INVALID_SOCKET)
			shutdown (m_ctrl, SD_BOTH);
	}

	int cmd (string const &line, string *text = nullptr)
	{
		sendAll (m_ctrl, (line + "\r\n").data (), line.size () + 2);
		return reply (text);
	}

	int reply (string *text = nullptr)
	{
		string all;
		for (;;)
		{
			string line = readLine ();
			all += line + "\n";
			if (line.size () >= 4 && isdigit ((unsigned char)line[0]) && isdigit ((unsigned char)line[1]) &&
			    isdigit ((unsigned char)line[2]) && line[3] == ' ')
			{
				if (text)
					*text = all;
				return atoi (line.c_str ());
			}
		}
	}

	std::vector<Entry> list (wstring const &path)
	{
		openData ();
		string text;
		int code = cmd ("MLSD " + utf8 (path), &text);
		if (code != 150 && code != 125)
		{
			closeData ();
			throw FtpError{text};
		}
		string raw;
		char buf[65536];
		for (;;)
		{
			int n = recv (m_data, buf, sizeof (buf), 0);
			if (n == 0)
				break;
			if (n < 0)
				throw NetError ();
			raw.append (buf, n);
		}
		closeData ();
		if (reply (&text) / 100 != 2)
			throw FtpError{text};

		std::vector<Entry> out;
		size_t pos = 0;
		while (pos < raw.size ())
		{
			size_t end = raw.find ('\n', pos);
			if (end == string::npos)
				end = raw.size ();
			string line = raw.substr (pos, end - pos);
			pos         = end + 1;
			if (!line.empty () && line.back () == '\r')
				line.pop_back ();
			size_t sp = line.find ("; ");
			if (sp == string::npos)
				continue;
			string facts = line.substr (0, sp + 1), name = line.substr (sp + 2);
			Entry e{wide (name), false, 0};
			bool skip = false;
			size_t f  = 0;
			while (f < facts.size ())
			{
				size_t semi = facts.find (';', f);
				string fact = facts.substr (f, semi - f);
				f           = semi + 1;
				size_t eq   = fact.find ('=');
				if (eq == string::npos)
					continue;
				string key = fact.substr (0, eq), val = fact.substr (eq + 1);
				for (auto &c : key)
					c = (char)tolower ((unsigned char)c);
				for (auto &c : val)
					c = (char)tolower ((unsigned char)c);
				if (key == "type")
				{
					e.dir = val == "dir";
					skip  = val == "cdir" || val == "pdir";
				}
				else if (key == "size")
					e.size = _strtoui64 (val.c_str (), nullptr, 10);
			}
			if (!skip && !e.name.empty ())
				out.push_back (std::move (e));
		}
		return out;
	}

	// returns false if the remote file does not exist
	bool size (wstring const &path, uint64_t &out)
	{
		string text;
		if (cmd ("SIZE " + utf8 (path), &text) != 213)
			return false;
		out = _strtoui64 (text.c_str () + 4, nullptr, 10);
		return true;
	}

	void simple (string const &line, int expect)
	{
		string text;
		if (cmd (line, &text) / 100 != expect / 100)
			throw FtpError{text};
	}

	// progress(bytes) is called for every chunk; it throws CancelError to stop
	void upload (wstring const &local, wstring const &remote, uint64_t offset, std::function<void (uint64_t)> const &progress)
	{
		HANDLE h = CreateFileW (local.c_str (),
		    GENERIC_READ,
		    FILE_SHARE_READ | FILE_SHARE_WRITE,
		    nullptr,
		    OPEN_EXISTING,
		    FILE_FLAG_SEQUENTIAL_SCAN,
		    nullptr);
		if (h == INVALID_HANDLE_VALUE)
			throw FtpError{"Can't open local file " + utf8 (local)};
		std::unique_ptr<void, decltype (&CloseHandle)> guard (h, &CloseHandle);
		LARGE_INTEGER li;
		li.QuadPart = (LONGLONG)offset;
		SetFilePointerEx (h, li, nullptr, FILE_BEGIN);

		openData ();
		string text;
		if (offset)
			simple ("REST " + std::to_string (offset), 350);
		int code = cmd ("STOR " + utf8 (remote), &text);
		if (code != 150 && code != 125)
		{
			closeData ();
			throw FtpError{text};
		}

		std::vector<char> buf (1 << 20);
		try
		{
			for (;;)
			{
				DWORD n = 0;
				if (!ReadFile (h, buf.data (), (DWORD)buf.size (), &n, nullptr))
					throw FtpError{"Can't read local file " + utf8 (local)};
				if (n == 0)
					break;
				sendAll (m_data, buf.data (), n);
				progress (n);
			}
		}
		catch (CancelError &)
		{
			resetData ();
			reply ();
			cmd ("DELE " + utf8 (remote)); // don't leave a truncated file behind
			throw;
		}
		shutdown (m_data, SD_SEND);
		code = reply (&text);
		closeData ();
		if (code / 100 != 2)
			throw FtpError{text};
	}

	void download (wstring const &remote, wstring const &local, uint64_t offset, std::function<void (uint64_t)> const &progress)
	{
		HANDLE h = CreateFileW (local.c_str (),
		    GENERIC_WRITE,
		    FILE_SHARE_READ,
		    nullptr,
		    offset ? OPEN_ALWAYS : CREATE_ALWAYS,
		    FILE_FLAG_SEQUENTIAL_SCAN,
		    nullptr);
		if (h == INVALID_HANDLE_VALUE)
			throw FtpError{"Can't create local file " + utf8 (local)};
		std::unique_ptr<void, decltype (&CloseHandle)> guard (h, &CloseHandle);
		LARGE_INTEGER li;
		li.QuadPart = (LONGLONG)offset;
		SetFilePointerEx (h, li, nullptr, FILE_BEGIN);
		SetEndOfFile (h);

		openData ();
		string text;
		if (offset)
			simple ("REST " + std::to_string (offset), 350);
		int code = cmd ("RETR " + utf8 (remote), &text);
		if (code != 150 && code != 125)
		{
			closeData ();
			throw FtpError{text};
		}

		std::vector<char> buf (1 << 20);
		try
		{
			for (;;)
			{
				int n = recv (m_data, buf.data (), (int)buf.size (), 0);
				if (n == 0)
					break;
				if (n < 0)
					throw NetError ();
				DWORD w = 0;
				if (!WriteFile (h, buf.data (), n, &w, nullptr) || w != (DWORD)n)
					throw FtpError{"Can't write local file " + utf8 (local)};
				progress (n);
			}
		}
		catch (CancelError &)
		{
			resetData ();
			reply ();
			throw;
		}
		closeData ();
		if (reply (&text) / 100 != 2)
			throw FtpError{text};
	}

private:
	static void setTimeout (SOCKET s, DWORD ms)
	{
		setsockopt (s, SOL_SOCKET, SO_RCVTIMEO, (char const *)&ms, sizeof (ms));
		setsockopt (s, SOL_SOCKET, SO_SNDTIMEO, (char const *)&ms, sizeof (ms));
	}

	static SOCKET connectTo (string const &host, int port, int timeoutMs)
	{
		addrinfo hints{}, *res = nullptr;
		hints.ai_family   = AF_INET;
		hints.ai_socktype = SOCK_STREAM;
		if (getaddrinfo (host.c_str (), std::to_string (port).c_str (), &hints, &res) != 0 || !res)
			return INVALID_SOCKET;
		SOCKET s = socket (AF_INET, SOCK_STREAM, IPPROTO_TCP);
		if (s == INVALID_SOCKET)
		{
			freeaddrinfo (res);
			return s;
		}
		u_long nb = 1;
		ioctlsocket (s, FIONBIO, &nb);
		int rc = ::connect (s, res->ai_addr, (int)res->ai_addrlen);
		freeaddrinfo (res);
		if (rc != 0 && WSAGetLastError () == WSAEWOULDBLOCK)
		{
			fd_set w, e;
			FD_ZERO (&w);
			FD_ZERO (&e);
			FD_SET (s, &w);
			FD_SET (s, &e);
			timeval tv{timeoutMs / 1000, (timeoutMs % 1000) * 1000};
			rc = select (0, nullptr, &w, &e, &tv) == 1 && FD_ISSET (s, &w) ? 0 : -1;
		}
		if (rc != 0)
		{
			closesocket (s);
			return INVALID_SOCKET;
		}
		nb = 0;
		ioctlsocket (s, FIONBIO, &nb);
		return s;
	}

	void sendAll (SOCKET s, char const *p, size_t n)
	{
		while (n > 0)
		{
			int r = send (s, p, (int)std::min<size_t> (n, 1 << 20), 0);
			if (r <= 0)
				throw NetError ();
			p += r;
			n -= r;
		}
	}

	string readLine ()
	{
		for (;;)
		{
			size_t nl = m_rbuf.find ('\n');
			if (nl != string::npos)
			{
				string line = m_rbuf.substr (0, nl);
				m_rbuf.erase (0, nl + 1);
				if (!line.empty () && line.back () == '\r')
					line.pop_back ();
				return line;
			}
			char buf[4096];
			int n = recv (m_ctrl, buf, sizeof (buf), 0);
			if (n <= 0)
				throw NetError ();
			m_rbuf.append (buf, n);
		}
	}

	void openData ()
	{
		closeData ();
		string text;
		if (cmd ("PASV", &text) != 227)
			throw FtpError{text};
		// "227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)": use the control host
		size_t open = text.find ('(');
		int v[6]    = {};
		if (open == string::npos ||
		    sscanf (text.c_str () + open + 1, "%d,%d,%d,%d,%d,%d", &v[0], &v[1], &v[2], &v[3], &v[4], &v[5]) != 6)
			throw FtpError{text};
		SOCKET s = connectTo (m_host, v[4] * 256 + v[5], 5000);
		if (s == INVALID_SOCKET)
			throw NetError ();
		setTimeout (s, 30000);
		std::lock_guard lk (m_sockLock);
		m_data = s;
	}

	void closeData ()
	{
		std::lock_guard lk (m_sockLock);
		if (m_data != INVALID_SOCKET)
			closesocket (m_data);
		m_data = INVALID_SOCKET;
	}

	// hard close (RST) so the server sees the transfer as broken
	void resetData ()
	{
		linger l{1, 0};
		setsockopt (m_data, SOL_SOCKET, SO_LINGER, (char const *)&l, sizeof (l));
		closeData ();
	}

	std::mutex m_sockLock;
	SOCKET m_ctrl = INVALID_SOCKET, m_data = INVALID_SOCKET;
	string m_host;
	string m_rbuf;
};

// ---------------------------------------------------------------------------
// jobs and workers

struct Job
{
	enum Type
	{
		List,
		Mkdir,
		Rename,
		Delete,
		Upload,
		Download
	} type;

	wstring path;  // List/Mkdir: target. Rename: from. Upload: remote dir. Download/Delete: remote dir
	wstring path2; // Rename: to. Download: local dir
	std::vector<wstring> locals; // Upload sources
	std::vector<Entry> remotes;  // Download/Delete: entries inside path

	// transfer state, kept across reconnects so the job resumes
	struct Item
	{
		wstring local, remote;
		bool dir;
		uint64_t size;
	};
	std::vector<Item> items;
	bool expanded  = false;
	size_t next    = 0;
	bool partial   = false; // items[next] was interrupted mid-file
	uint64_t done  = 0;
	uint64_t total = 0;
	int errors     = 0;

	HANDLE doneEvent = nullptr; // set when finished (Download for drag-out)
	bool ok          = false;
};

using JobPtr = std::shared_ptr<Job>;

class Worker
{
public:
	enum State
	{
		Waiting,
		Connected
	};

	explicit Worker (bool transfer) : m_transfer (transfer) {}

	void start () { m_thread = std::thread ([this] { run (); }); }

	void stop ()
	{
		{
			std::lock_guard lk (m_lock);
			m_quit = true;
		}
		m_cv.notify_all ();
		m_ftp.abort ();
		if (m_thread.joinable ())
			m_thread.join ();
	}

	void push (JobPtr job)
	{
		{
			std::lock_guard lk (m_lock);
			if (job->type == Job::List)
			{
				// only the latest listing matters
				m_jobs.erase (std::remove_if (m_jobs.begin (),
				                  m_jobs.end (),
				                  [&] (JobPtr const &j) { return j->type == Job::List && j != m_running; }),
				    m_jobs.end ());
			}
			m_jobs.push_back (std::move (job));
		}
		m_cv.notify_all ();
	}

	void reconnect ()
	{
		{
			std::lock_guard lk (m_lock);
			m_kick = true;
		}
		m_cv.notify_all ();
		m_ftp.abort ();
	}

	// drop every queued transfer job (the running one stops via g_cancel)
	void clearPending ()
	{
		std::lock_guard lk (m_lock);
		for (auto it = m_jobs.begin (); it != m_jobs.end ();)
		{
			if (*it != m_running)
			{
				finish (*it, false);
				it = m_jobs.erase (it);
			}
			else
				++it;
		}
	}

	bool busy ()
	{
		std::lock_guard lk (m_lock);
		return !m_jobs.empty ();
	}

	std::atomic<int> state{Waiting};

private:
	void setState (State s)
	{
		if (state.exchange (s) != s)
			PostMessageW (g_wnd, WM_APP_STATE, 0, 0);
	}

	static void finish (JobPtr const &job, bool ok)
	{
		job->ok = ok;
		if (job->doneEvent)
			SetEvent (job->doneEvent);
	}

	void run ()
	{
		for (;;)
		{
			if (!m_ftp.connected ())
			{
				string host;
				int port;
				{
					std::lock_guard lk (g_cfgLock);
					host = g_host;
					port = g_port;
				}
				if (m_ftp.connect (host, port))
				{
					setState (Connected);
					if (!m_transfer)
					{
						// always show fresh content after (re)connecting
						auto job  = std::make_shared<Job> ();
						job->type = Job::List;
						job->path = currentDir ();
						push (job);
					}
				}
				else
				{
					setState (Waiting);
					std::unique_lock lk (m_lock);
					m_cv.wait_for (lk, std::chrono::seconds (2), [&] { return m_quit || m_kick; });
					m_kick = false;
					if (m_quit)
						return;
					continue;
				}
			}

			JobPtr job;
			{
				std::unique_lock lk (m_lock);
				m_cv.wait_for (lk, std::chrono::seconds (10), [&] { return m_quit || m_kick || !m_jobs.empty (); });
				if (m_quit)
					return;
				if (m_kick)
				{
					m_kick = false;
					lk.unlock ();
					m_ftp.close ();
					setState (Waiting);
					continue;
				}
				if (!m_jobs.empty ())
					m_running = job = m_jobs.front ();
			}

			if (!job)
			{
				// keep-alive, and notice quickly when the 3DS went away
				try
				{
					if (m_ftp.cmd ("NOOP") / 100 != 2)
						throw NetError ();
				}
				catch (NetError &)
				{
					m_ftp.close ();
					setState (Waiting);
				}
				continue;
			}

			bool ok   = false;
			bool done = true;
			try
			{
				execute (*job);
				ok = job->errors == 0;
			}
			catch (NetError &)
			{
				// connection lost: keep the job, it resumes after reconnecting
				m_ftp.close ();
				setState (Waiting);
				done = g_cancel;
			}
			catch (CancelError &)
			{
				postMessageText (L"Transfer cancelled");
			}
			catch (FtpError &e)
			{
				postMessageText (L"Error: " + wide (e.msg.substr (0, e.msg.find_last_not_of ("\r\n") + 1)));
			}

			{
				std::lock_guard lk (m_lock);
				m_running.reset ();
				if (done)
				{
					auto it = std::find (m_jobs.begin (), m_jobs.end (), job);
					if (it != m_jobs.end ())
						m_jobs.erase (it);
				}
			}
			if (done)
			{
				finish (job, ok);
				if (m_transfer)
				{
					std::lock_guard lk (g_prog.lock);
					if (!busy ())
						g_prog.active = false;
				}
				if (job->type != Job::List)
					refreshAfter ();
			}
		}
	}

	void refreshAfter ();

	void execute (Job &job)
	{
		switch (job.type)
		{
		case Job::List:
		{
			auto *r = new ListResult{job.path, {}, false};
			try
			{
				r->entries = m_ftp.list (job.path);
				r->ok      = true;
			}
			catch (FtpError &)
			{
			}
			catch (...)
			{
				delete r;
				throw;
			}
			PostMessageW (g_wnd, WM_APP_LIST, 0, (LPARAM)r);
			break;
		}
		case Job::Mkdir:
			m_ftp.simple ("MKD " + utf8 (job.path), 257);
			break;
		case Job::Rename:
			m_ftp.simple ("RNFR " + utf8 (job.path), 350);
			m_ftp.simple ("RNTO " + utf8 (job.path2), 250);
			break;
		case Job::Delete:
			for (auto const &e : job.remotes)
				m_ftp.simple ((e.dir ? "RMD " : "DELE ") + utf8 (joinRemote (job.path, e.name)), 250);
			break;
		case Job::Upload:
		case Job::Download:
			transfer (job);
			break;
		}
	}

	void expandLocal (Job &job, wstring const &local, wstring const &remote)
	{
		WIN32_FILE_ATTRIBUTE_DATA fa;
		if (!GetFileAttributesExW (local.c_str (), GetFileExInfoStandard, &fa))
			return;
		if (fa.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)
		{
			job.items.push_back ({local, remote, true, 0});
			WIN32_FIND_DATAW fd;
			HANDLE h = FindFirstFileW ((local + L"\\*").c_str (), &fd);
			if (h == INVALID_HANDLE_VALUE)
				return;
			do
			{
				if (wcscmp (fd.cFileName, L".") && wcscmp (fd.cFileName, L".."))
					expandLocal (job, local + L"\\" + fd.cFileName, joinRemote (remote, fd.cFileName));
			} while (FindNextFileW (h, &fd));
			FindClose (h);
		}
		else
		{
			uint64_t size = ((uint64_t)fa.nFileSizeHigh << 32) | fa.nFileSizeLow;
			job.items.push_back ({local, remote, false, size});
			job.total += size;
		}
	}

	void expandRemote (Job &job, wstring const &remote, wstring const &local, Entry const &e)
	{
		if (e.dir)
		{
			job.items.push_back ({local, remote, true, 0});
			for (auto const &child : m_ftp.list (remote))
				expandRemote (job, joinRemote (remote, child.name), local + L"\\" + child.name, child);
		}
		else
		{
			job.items.push_back ({local, remote, false, e.size});
			job.total += e.size;
		}
	}

	void transfer (Job &job)
	{
		bool up = job.type == Job::Upload;
		if (!job.expanded)
		{
			if (up)
				for (auto const &l : job.locals)
					expandLocal (job, l, joinRemote (job.path, l.substr (l.find_last_of (L"\\/") + 1)));
			else
				for (auto const &e : job.remotes)
					expandRemote (job, joinRemote (job.path, e.name), job.path2 + L"\\" + e.name, e);
			job.expanded = true;
		}

		int fileCount = 0;
		for (auto const &it : job.items)
			fileCount += !it.dir;

		for (; job.next < job.items.size (); ++job.next)
		{
			if (g_cancel)
				throw CancelError ();
			auto const &it = job.items[job.next];
			if (it.dir)
			{
				if (up)
				{
					string text;
					m_ftp.cmd ("MKD " + utf8 (it.remote), &text); // fails harmlessly if it exists
				}
				else
					CreateDirectoryW (it.local.c_str (), nullptr);
				continue;
			}

			int index = 0;
			for (size_t i = 0; i <= job.next; ++i)
				index += !job.items[i].dir;

			// resume an interrupted file where it stopped
			uint64_t offset = 0;
			if (job.partial)
			{
				if (up)
				{
					uint64_t remote = 0;
					if (m_ftp.size (it.remote, remote) && remote <= it.size)
						offset = remote;
				}
				else
				{
					WIN32_FILE_ATTRIBUTE_DATA fa;
					if (GetFileAttributesExW (it.local.c_str (), GetFileExInfoStandard, &fa))
						offset = std::min (it.size, ((uint64_t)fa.nFileSizeHigh << 32) | fa.nFileSizeLow);
				}
			}

			uint64_t fileDone = offset;
			uint64_t base     = job.done;
			{
				std::lock_guard lk (g_prog.lock);
				g_prog.active    = true;
				g_prog.up        = up;
				g_prog.name      = it.remote.substr (it.remote.find_last_of (L'/') + 1);
				g_prog.fileIndex = index;
				g_prog.fileCount = fileCount;
				g_prog.total     = job.total;
				g_prog.done      = base + offset;
			}
			auto progress = [&] (uint64_t n) {
				fileDone += n;
				std::lock_guard lk (g_prog.lock);
				g_prog.done = base + fileDone;
				if (g_cancel)
					throw CancelError ();
			};

			job.partial = true;
			try
			{
				if (up)
					m_ftp.upload (it.local, it.remote, offset, progress);
				else
					m_ftp.download (it.remote, it.local, offset, progress);
			}
			catch (FtpError &e)
			{
				++job.errors;
				postMessageText (L"Error: " + wide (e.msg.substr (0, e.msg.find_last_not_of ("\r\n") + 1)));
			}
			catch (CancelError &)
			{
				if (!up)
					DeleteFileW (it.local.c_str ());
				throw;
			}
			catch (NetError &)
			{
				// remember how far this job got, the file resumes on retry
				throw;
			}
			job.partial = false;
			job.done    = base + it.size;
		}

		if (!job.errors)
			postMessageText (wstring (up ? L"Upload" : L"Download") + L" complete: " + std::to_wstring (fileCount) +
			                 L" file(s), " + formatSize (job.total));
	}

	bool m_transfer;
	Ftp m_ftp;
	std::thread m_thread;
	std::mutex m_lock;
	std::condition_variable m_cv;
	std::deque<JobPtr> m_jobs;
	JobPtr m_running;
	bool m_quit = false;
	bool m_kick = false;
};

static Worker g_browse (false);
static Worker g_xfer (true);

void Worker::refreshAfter ()
{
	auto job  = std::make_shared<Job> ();
	job->type = Job::List;
	job->path = currentDir ();
	g_browse.push (job);
}

// ---------------------------------------------------------------------------
// config

static wstring configPath ()
{
	PWSTR base = nullptr;
	wstring path;
	if (SUCCEEDED (SHGetKnownFolderPath (FOLDERID_RoamingAppData, 0, nullptr, &base)))
	{
		path = wstring (base) + L"\\synaftp";
		CreateDirectoryW (path.c_str (), nullptr);
		path += L"\\config.ini";
	}
	CoTaskMemFree (base);
	return path;
}

static bool loadConfig ()
{
	wchar_t host[256] = {};
	auto path         = configPath ();
	g_dark            = GetPrivateProfileIntW (L"ui", L"dark", 1, path.c_str ()) != 0;
	GetPrivateProfileStringW (L"server", L"host", L"", host, 256, path.c_str ());
	int port = GetPrivateProfileIntW (L"server", L"port", 5000, path.c_str ());
	if (!host[0])
		return false;
	std::lock_guard lk (g_cfgLock);
	g_host = utf8 (host);
	g_port = port;
	return true;
}

static void saveConfig ()
{
	auto path = configPath ();
	std::lock_guard lk (g_cfgLock);
	WritePrivateProfileStringW (L"server", L"host", wide (g_host).c_str (), path.c_str ());
	WritePrivateProfileStringW (L"server", L"port", std::to_wstring (g_port).c_str (), path.c_str ());
	WritePrivateProfileStringW (L"ui", L"dark", g_dark ? L"1" : L"0", path.c_str ());
}

// ---------------------------------------------------------------------------
// theme (dark by default)

static HBRUSH g_bgBrush, g_editBrush;

struct Theme
{
	COLORREF bg, panel, text, dim, list, edit, border, accent;
};

static Theme theme ()
{
	if (g_dark)
		return {RGB (32, 32, 32), RGB (43, 43, 43), RGB (230, 230, 230), RGB (160, 160, 160), RGB (25, 25, 25),
		    RGB (45, 45, 45), RGB (64, 64, 64), RGB (76, 160, 255)};
	return {GetSysColor (COLOR_BTNFACE), GetSysColor (COLOR_BTNFACE), GetSysColor (COLOR_WINDOWTEXT),
	    GetSysColor (COLOR_GRAYTEXT), GetSysColor (COLOR_WINDOW), GetSysColor (COLOR_WINDOW),
	    GetSysColor (COLOR_3DSHADOW), GetSysColor (COLOR_HIGHLIGHT)};
}

static void makeBrushes ()
{
	if (g_bgBrush)
		DeleteObject (g_bgBrush);
	if (g_editBrush)
		DeleteObject (g_editBrush);
	g_bgBrush   = CreateSolidBrush (theme ().bg);
	g_editBrush = CreateSolidBrush (theme ().edit);
}

// dark title bar (documented DWM attribute, Windows 10 20H1+ / 11)
static void darkTitleBar (HWND wnd)
{
	BOOL dark = g_dark;
	DwmSetWindowAttribute (wnd, 20 /* DWMWA_USE_IMMERSIVE_DARK_MODE */, &dark, sizeof (dark));
}

// dark popup menus: uxtheme's SetPreferredAppMode (ordinal 135, Windows 10
// 1903+). Undocumented but stable; skipped on older builds.
static void setMenuMode ()
{
	using SetMode = int (WINAPI *) (int);
	using Flush   = void (WINAPI *) ();
	static SetMode setMode;
	static Flush flush;
	static bool init;
	if (!init)
	{
		init = true;
		using RtlGetVersionFn = LONG (WINAPI *) (OSVERSIONINFOW *);
		auto rtlGetVersion    = (RtlGetVersionFn)(void *)GetProcAddress (GetModuleHandleW (L"ntdll.dll"), "RtlGetVersion");
		OSVERSIONINFOW vi{sizeof (vi)};
		HMODULE ux = LoadLibraryExW (L"uxtheme.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32);
		if (rtlGetVersion && rtlGetVersion (&vi) == 0 && vi.dwBuildNumber >= 18362 && ux)
		{
			setMode = (SetMode)(void *)GetProcAddress (ux, MAKEINTRESOURCEA (135));
			flush   = (Flush)(void *)GetProcAddress (ux, MAKEINTRESOURCEA (136));
		}
	}
	if (setMode)
		setMode (g_dark ? 2 /* ForceDark */ : 3 /* ForceLight */);
	if (flush)
		flush ();
}

// WM_CTLCOLOR* for dialogs, read-only edits and buttons
static LRESULT ctlColor (UINT msg, HDC dc)
{
	auto t = theme ();
	SetTextColor (dc, t.text);
	if (msg == WM_CTLCOLOREDIT)
	{
		SetBkColor (dc, t.edit);
		return (LRESULT)g_editBrush;
	}
	SetBkColor (dc, t.bg);
	return (LRESULT)g_bgBrush;
}

static void themeControl (HWND h, bool edit = false)
{
	SetWindowTheme (h, g_dark ? (edit ? L"DarkMode_CFD" : L"DarkMode_Explorer") : nullptr, nullptr);
}

static INT_PTR CALLBACK serverDlgProc (HWND dlg, UINT msg, WPARAM wp, LPARAM)
{
	switch (msg)
	{
	case WM_CTLCOLORDLG:
	case WM_CTLCOLORSTATIC:
	case WM_CTLCOLOREDIT:
	case WM_CTLCOLORBTN:
		return (INT_PTR)ctlColor (msg, (HDC)wp);
	case WM_INITDIALOG:
	{
		darkTitleBar (dlg);
		for (int id : {IDOK, IDCANCEL})
			themeControl (GetDlgItem (dlg, id));
		for (int id : {IDC_HOST, IDC_PORT})
			themeControl (GetDlgItem (dlg, id), true);
		std::lock_guard lk (g_cfgLock);
		SetDlgItemTextW (dlg, IDC_HOST, wide (g_host).c_str ());
		SetDlgItemInt (dlg, IDC_PORT, g_port, FALSE);
		return TRUE;
	}
	case WM_COMMAND:
		if (LOWORD (wp) == IDOK)
		{
			wchar_t host[256];
			GetDlgItemTextW (dlg, IDC_HOST, host, 256);
			BOOL ok  = FALSE;
			UINT port = GetDlgItemInt (dlg, IDC_PORT, &ok, FALSE);
			wstring h = host;
			h.erase (0, h.find_first_not_of (L' '));
			h.erase (h.find_last_not_of (L' ') + 1);
			if (h.empty () || !ok || port == 0 || port > 65535)
			{
				MessageBoxW (dlg, L"Enter the IP address and port shown on the 3DS.", L"synaftp", MB_ICONWARNING);
				return TRUE;
			}
			{
				std::lock_guard lk (g_cfgLock);
				g_host = utf8 (h);
				g_port = (int)port;
			}
			saveConfig ();
			EndDialog (dlg, IDOK);
			return TRUE;
		}
		if (LOWORD (wp) == IDCANCEL)
		{
			EndDialog (dlg, IDCANCEL);
			return TRUE;
		}
		break;
	}
	return FALSE;
}

// ---------------------------------------------------------------------------
// list view

static int iconFor (wstring const &name, bool dir)
{
	static std::map<wstring, int> cache;
	wstring ext = L"<dir>";
	if (!dir)
	{
		auto dot = name.find_last_of (L'.');
		ext      = dot == wstring::npos ? L"" : name.substr (dot);
		for (auto &c : ext)
			c = towlower (c);
	}
	auto it = cache.find (ext);
	if (it != cache.end ())
		return it->second;
	SHFILEINFOW fi{};
	SHGetFileInfoW (dir ? L"folder" : (L"file" + ext).c_str (),
	    dir ? FILE_ATTRIBUTE_DIRECTORY : FILE_ATTRIBUTE_NORMAL,
	    &fi,
	    sizeof (fi),
	    SHGFI_USEFILEATTRIBUTES | SHGFI_SYSICONINDEX | SHGFI_SMALLICON);
	return cache[ext] = fi.iIcon;
}

static void navigate (wstring const &path)
{
	{
		std::lock_guard lk (g_cwdLock);
		g_cwd = path;
	}
	SetWindowTextW (g_path, path.c_str ());
	auto job  = std::make_shared<Job> ();
	job->type = Job::List;
	job->path = path;
	g_browse.push (job);
}

static void refresh () { navigate (currentDir ()); }

static void showList (ListResult *r)
{
	wstring cwd = currentDir ();
	if (r->path != cwd)
		return;
	if (!r->ok)
	{
		setStatus (1, (L"Can't open " + r->path).c_str ());
		if (cwd != L"/")
			navigate (parentRemote (cwd));
		return;
	}

	std::sort (r->entries.begin (), r->entries.end (), [] (Entry const &a, Entry const &b) {
		if (a.dir != b.dir)
			return a.dir;
		return CompareStringEx (LOCALE_NAME_USER_DEFAULT,
		           NORM_IGNORECASE | SORT_DIGITSASNUMBERS,
		           a.name.c_str (),
		           -1,
		           b.name.c_str (),
		           -1,
		           nullptr,
		           nullptr,
		           0) == CSTR_LESS_THAN;
	});

	// keep the selection when refreshing the same folder
	std::vector<wstring> selected;
	for (int i = -1; (i = ListView_GetNextItem (g_list, i, LVNI_SELECTED)) >= 0;)
	{
		LVITEMW it{LVIF_PARAM, i};
		ListView_GetItem (g_list, &it);
		if (it.lParam >= 0 && it.lParam < (LPARAM)g_entries.size ())
			selected.push_back (g_entries[it.lParam].name);
	}

	g_entries = std::move (r->entries);
	SendMessageW (g_list, WM_SETREDRAW, FALSE, 0);
	ListView_DeleteAllItems (g_list);
	int row = 0;
	if (cwd != L"/")
	{
		LVITEMW it{LVIF_TEXT | LVIF_IMAGE | LVIF_PARAM, row++};
		it.pszText = (LPWSTR)L"..";
		it.iImage  = iconFor (L"", true);
		it.lParam  = -1;
		ListView_InsertItem (g_list, &it);
	}
	for (size_t i = 0; i < g_entries.size (); ++i)
	{
		auto const &e = g_entries[i];
		LVITEMW it{LVIF_TEXT | LVIF_IMAGE | LVIF_PARAM, row};
		it.pszText = (LPWSTR)e.name.c_str ();
		it.iImage  = iconFor (e.name, e.dir);
		it.lParam  = (LPARAM)i;
		if (std::find (selected.begin (), selected.end (), e.name) != selected.end ())
		{
			it.mask |= LVIF_STATE;
			it.state = it.stateMask = LVIS_SELECTED;
		}
		ListView_InsertItem (g_list, &it);
		if (!e.dir)
		{
			wstring sz = formatSize (e.size);
			ListView_SetItemText (g_list, row, 1, (LPWSTR)sz.c_str ());
		}
		++row;
	}
	if (ListView_GetNextItem (g_list, -1, LVNI_FOCUSED) < 0 && row > 0)
		ListView_SetItemState (g_list, 0, LVIS_FOCUSED, LVIS_FOCUSED);
	SendMessageW (g_list, WM_SETREDRAW, TRUE, 0);
	InvalidateRect (g_list, nullptr, TRUE);
}

static Entry const *entryAt (int row)
{
	LVITEMW it{LVIF_PARAM, row};
	if (row < 0 || !ListView_GetItem (g_list, &it) || it.lParam < 0 || it.lParam >= (LPARAM)g_entries.size ())
		return nullptr;
	return &g_entries[it.lParam];
}

static bool isParentRow (int row)
{
	LVITEMW it{LVIF_PARAM, row};
	return row >= 0 && ListView_GetItem (g_list, &it) && it.lParam == -1;
}

static std::vector<Entry> selectedEntries ()
{
	std::vector<Entry> out;
	for (int i = -1; (i = ListView_GetNextItem (g_list, i, LVNI_SELECTED)) >= 0;)
		if (auto e = entryAt (i))
			out.push_back (*e);
	return out;
}

static void openRow (int row)
{
	if (isParentRow (row))
		navigate (parentRemote (currentDir ()));
	else if (auto e = entryAt (row); e && e->dir)
		navigate (joinRemote (currentDir (), e->name));
}

static void deleteSelected ()
{
	auto sel = selectedEntries ();
	if (sel.empty ())
		return;
	wstring q = sel.size () == 1 ? L"Delete \"" + sel[0].name + L"\"" + (sel[0].dir ? L" and everything in it" : L"") + L"?"
	                             : L"Delete " + std::to_wstring (sel.size ()) + L" items?";
	if (MessageBoxW (g_wnd, q.c_str (), L"synaftp", MB_YESNO | MB_ICONWARNING) != IDYES)
		return;
	auto job     = std::make_shared<Job> ();
	job->type    = Job::Delete;
	job->path    = currentDir ();
	job->remotes = sel;
	g_browse.push (job);
}

static void newFolder ()
{
	wstring dir = currentDir ();
	wstring name = L"New folder";
	for (int n = 2;; ++n)
	{
		bool taken = std::any_of (g_entries.begin (), g_entries.end (), [&] (Entry const &e) { return e.name == name; });
		if (!taken)
			break;
		name = L"New folder (" + std::to_wstring (n) + L")";
	}
	auto job  = std::make_shared<Job> ();
	job->type = Job::Mkdir;
	job->path = joinRemote (dir, name);
	g_browse.push (job);
}

static void startUpload (std::vector<wstring> const &files, wstring const &remoteDir)
{
	if (files.empty ())
		return;
	g_cancel    = false;
	auto job    = std::make_shared<Job> ();
	job->type   = Job::Upload;
	job->path   = remoteDir;
	job->locals = files;
	{
		std::lock_guard lk (g_prog.lock);
		g_prog.active = true;
	}
	g_xfer.push (job);
}

static void startDownload (std::vector<Entry> const &entries, wstring const &localDir, HANDLE doneEvent)
{
	g_cancel       = false;
	auto job       = std::make_shared<Job> ();
	job->type      = Job::Download;
	job->path      = currentDir ();
	job->path2     = localDir;
	job->remotes   = entries;
	job->doneEvent = doneEvent;
	{
		std::lock_guard lk (g_prog.lock);
		g_prog.active = true;
	}
	g_xfer.push (job);
}

// ---------------------------------------------------------------------------
// drag and drop

// files dropped from Explorer onto the list: upload them
class DropTarget : public IDropTarget
{
public:
	HRESULT STDMETHODCALLTYPE QueryInterface (REFIID riid, void **ppv) override
	{
		if (riid == IID_IUnknown || riid == IID_IDropTarget)
		{
			*ppv = this;
			AddRef ();
			return S_OK;
		}
		*ppv = nullptr;
		return E_NOINTERFACE;
	}
	ULONG STDMETHODCALLTYPE AddRef () override { return InterlockedIncrement (&m_ref); }
	ULONG STDMETHODCALLTYPE Release () override
	{
		LONG r = InterlockedDecrement (&m_ref);
		if (!r)
			delete this;
		return r;
	}

	HRESULT STDMETHODCALLTYPE DragEnter (IDataObject *data, DWORD key, POINTL pt, DWORD *effect) override
	{
		FORMATETC fe{CF_HDROP, nullptr, DVASPECT_CONTENT, -1, TYMED_HGLOBAL};
		m_ok = !g_dragOut && data->QueryGetData (&fe) == S_OK;
		return DragOver (key, pt, effect);
	}

	HRESULT STDMETHODCALLTYPE DragOver (DWORD, POINTL pt, DWORD *effect) override
	{
		if (!m_ok)
		{
			*effect = DROPEFFECT_NONE;
			return S_OK;
		}
		highlight (folderRowAt (pt));
		*effect = DROPEFFECT_COPY;
		return S_OK;
	}

	HRESULT STDMETHODCALLTYPE DragLeave () override
	{
		highlight (-1);
		return S_OK;
	}

	HRESULT STDMETHODCALLTYPE Drop (IDataObject *data, DWORD, POINTL pt, DWORD *effect) override
	{
		int row = folderRowAt (pt);
		highlight (-1);
		*effect = DROPEFFECT_NONE;
		if (!m_ok)
			return S_OK;

		FORMATETC fe{CF_HDROP, nullptr, DVASPECT_CONTENT, -1, TYMED_HGLOBAL};
		STGMEDIUM m{};
		if (data->GetData (&fe, &m) != S_OK)
			return S_OK;
		std::vector<wstring> files;
		HDROP drop = (HDROP)m.hGlobal;
		UINT count = DragQueryFileW (drop, 0xFFFFFFFF, nullptr, 0);
		for (UINT i = 0; i < count; ++i)
		{
			UINT len = DragQueryFileW (drop, i, nullptr, 0);
			wstring f (len, 0);
			DragQueryFileW (drop, i, f.data (), len + 1);
			files.push_back (f);
		}
		ReleaseStgMedium (&m);

		wstring dir = currentDir ();
		if (isParentRow (row))
			dir = parentRemote (dir);
		else if (auto e = entryAt (row))
			dir = joinRemote (dir, e->name);
		startUpload (files, dir);
		*effect = DROPEFFECT_COPY;
		return S_OK;
	}

private:
	static int folderRowAt (POINTL pt)
	{
		LVHITTESTINFO hit{{pt.x, pt.y}};
		ScreenToClient (g_list, &hit.pt);
		int row = ListView_HitTest (g_list, &hit);
		if (row < 0)
			return -1;
		if (isParentRow (row))
			return row;
		auto e = entryAt (row);
		return e && e->dir ? row : -1;
	}

	void highlight (int row)
	{
		if (row == m_hot)
			return;
		if (m_hot >= 0)
			ListView_SetItemState (g_list, m_hot, 0, LVIS_DROPHILITED);
		if (row >= 0)
			ListView_SetItemState (g_list, row, LVIS_DROPHILITED, LVIS_DROPHILITED);
		m_hot = row;
	}

	LONG m_ref = 1;
	bool m_ok  = false;
	int m_hot  = -1;
};

// files dragged out to Explorer: offered as CF_HDROP of temp files that are
// downloaded right when the mouse button is released over a drop target
class DataObject : public IDataObject
{
public:
	explicit DataObject (std::vector<wstring> files) : m_files (std::move (files)) {}

	HRESULT STDMETHODCALLTYPE QueryInterface (REFIID riid, void **ppv) override
	{
		if (riid == IID_IUnknown || riid == IID_IDataObject)
		{
			*ppv = this;
			AddRef ();
			return S_OK;
		}
		*ppv = nullptr;
		return E_NOINTERFACE;
	}
	ULONG STDMETHODCALLTYPE AddRef () override { return InterlockedIncrement (&m_ref); }
	ULONG STDMETHODCALLTYPE Release () override
	{
		LONG r = InterlockedDecrement (&m_ref);
		if (!r)
			delete this;
		return r;
	}

	HRESULT STDMETHODCALLTYPE GetData (FORMATETC *fe, STGMEDIUM *m) override
	{
		if (QueryGetData (fe) != S_OK)
			return DV_E_FORMATETC;
		size_t chars = 1;
		for (auto const &f : m_files)
			chars += f.size () + 1;
		HGLOBAL h = GlobalAlloc (GHND, sizeof (DROPFILES) + chars * sizeof (wchar_t));
		if (!h)
			return E_OUTOFMEMORY;
		auto *df   = (DROPFILES *)GlobalLock (h);
		df->pFiles = sizeof (DROPFILES);
		df->fWide  = TRUE;
		auto *p    = (wchar_t *)(df + 1);
		for (auto const &f : m_files)
		{
			memcpy (p, f.c_str (), (f.size () + 1) * sizeof (wchar_t));
			p += f.size () + 1;
		}
		GlobalUnlock (h);
		m->tymed          = TYMED_HGLOBAL;
		m->hGlobal        = h;
		m->pUnkForRelease = nullptr;
		return S_OK;
	}
	HRESULT STDMETHODCALLTYPE GetDataHere (FORMATETC *, STGMEDIUM *) override { return E_NOTIMPL; }
	HRESULT STDMETHODCALLTYPE QueryGetData (FORMATETC *fe) override
	{
		return fe->cfFormat == CF_HDROP && (fe->tymed & TYMED_HGLOBAL) ? S_OK : DV_E_FORMATETC;
	}
	HRESULT STDMETHODCALLTYPE GetCanonicalFormatEtc (FORMATETC *, FORMATETC *out) override
	{
		out->ptd = nullptr;
		return E_NOTIMPL;
	}
	HRESULT STDMETHODCALLTYPE SetData (FORMATETC *, STGMEDIUM *, BOOL) override { return E_NOTIMPL; }
	HRESULT STDMETHODCALLTYPE EnumFormatEtc (DWORD dir, IEnumFORMATETC **out) override
	{
		if (dir != DATADIR_GET)
			return E_NOTIMPL;
		FORMATETC fe{CF_HDROP, nullptr, DVASPECT_CONTENT, -1, TYMED_HGLOBAL};
		return SHCreateStdEnumFmtEtc (1, &fe, out);
	}
	HRESULT STDMETHODCALLTYPE DAdvise (FORMATETC *, DWORD, IAdviseSink *, DWORD *) override { return OLE_E_ADVISENOTSUPPORTED; }
	HRESULT STDMETHODCALLTYPE DUnadvise (DWORD) override { return OLE_E_ADVISENOTSUPPORTED; }
	HRESULT STDMETHODCALLTYPE EnumDAdvise (IEnumSTATDATA **) override { return OLE_E_ADVISENOTSUPPORTED; }

private:
	LONG m_ref = 1;
	std::vector<wstring> m_files;
};

class DropSource : public IDropSource
{
public:
	std::function<bool ()> onDrop;

	HRESULT STDMETHODCALLTYPE QueryInterface (REFIID riid, void **ppv) override
	{
		if (riid == IID_IUnknown || riid == IID_IDropSource)
		{
			*ppv = this;
			AddRef ();
			return S_OK;
		}
		*ppv = nullptr;
		return E_NOINTERFACE;
	}
	ULONG STDMETHODCALLTYPE AddRef () override { return InterlockedIncrement (&m_ref); }
	ULONG STDMETHODCALLTYPE Release () override
	{
		LONG r = InterlockedDecrement (&m_ref);
		if (!r)
			delete this;
		return r;
	}

	HRESULT STDMETHODCALLTYPE QueryContinueDrag (BOOL escape, DWORD keys) override
	{
		if (escape)
			return DRAGDROP_S_CANCEL;
		if (keys & MK_LBUTTON)
			return S_OK;

		// button released: only download if a real target outside this app accepts it
		POINT pt;
		GetCursorPos (&pt);
		DWORD pid = 0;
		GetWindowThreadProcessId (WindowFromPoint (pt), &pid);
		if (m_effect == DROPEFFECT_NONE || pid == GetCurrentProcessId ())
			return DRAGDROP_S_CANCEL;
		return onDrop () ? DRAGDROP_S_DROP : DRAGDROP_S_CANCEL;
	}

	HRESULT STDMETHODCALLTYPE GiveFeedback (DWORD effect) override
	{
		m_effect = effect;
		return DRAGDROP_S_USEDEFAULTCURSORS;
	}

private:
	LONG m_ref      = 1;
	DWORD m_effect  = DROPEFFECT_NONE;
};

static wstring tempRoot ()
{
	wchar_t tmp[MAX_PATH];
	GetTempPathW (MAX_PATH, tmp);
	return wstring (tmp) + L"synaftp";
}

static void deleteTree (wstring const &path)
{
	// SHFileOperation wants a double-null-terminated string
	wstring from = path + L'\0';
	SHFILEOPSTRUCTW op{};
	op.wFunc  = FO_DELETE;
	op.pFrom  = from.c_str ();
	op.fFlags = FOF_NO_UI;
	SHFileOperationW (&op);
}

// download into dir while keeping the window painted; Esc cancels
static bool downloadModal (std::vector<Entry> const &sel, wstring const &dir)
{
	HANDLE ev = CreateEventW (nullptr, TRUE, FALSE, nullptr);
	startDownload (sel, dir, ev);
	setStatus (1, L"Downloading... (Esc to cancel)");
	HCURSOR old = SetCursor (LoadCursor (nullptr, IDC_WAIT));
	while (MsgWaitForMultipleObjects (1, &ev, FALSE, INFINITE, QS_ALLINPUT) == WAIT_OBJECT_0 + 1)
	{
		MSG msg;
		while (PeekMessageW (&msg, nullptr, 0, 0, PM_REMOVE))
		{
			if (msg.message == WM_KEYDOWN && msg.wParam == VK_ESCAPE)
			{
				g_cancel = true;
				g_xfer.clearPending ();
			}
			bool input = (msg.message >= WM_KEYFIRST && msg.message <= WM_KEYLAST) ||
			             (msg.message >= WM_MOUSEFIRST && msg.message <= WM_MOUSELAST);
			if (!input)
				DispatchMessageW (&msg);
		}
	}
	SetCursor (old);
	// the job stays queued while disconnected; it is finished here
	// (successfully or not) so ev is always signaled
	auto ok = WaitForSingleObject (ev, 0) == WAIT_OBJECT_0 && !g_cancel;
	CloseHandle (ev);
	return ok;
}

static void beginDragOut ()
{
	auto sel = selectedEntries ();
	if (sel.empty ())
		return;

	wstring dir = tempRoot ();
	CreateDirectoryW (dir.c_str (), nullptr);
	dir += L"\\" + std::to_wstring (GetTickCount64 ());
	CreateDirectoryW (dir.c_str (), nullptr);

	std::vector<wstring> files;
	for (auto const &e : sel)
		files.push_back (dir + L"\\" + e.name);

	auto *data   = new DataObject (files);
	auto *source = new DropSource ();
	bool fetched = false;
	source->onDrop = [&] {
		fetched = true;
		return downloadModal (sel, dir);
	};
	g_dragOut  = true;
	DWORD eff  = 0;
	DoDragDrop (data, source, DROPEFFECT_COPY, &eff);
	g_dragOut = false;
	data->Release ();
	source->Release ();
	if (!fetched)
		RemoveDirectoryW (dir.c_str ());
}

static void downloadTo ()
{
	auto sel = selectedEntries ();
	if (sel.empty ())
		return;
	IFileOpenDialog *dlg = nullptr;
	if (FAILED (CoCreateInstance (CLSID_FileOpenDialog, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS (&dlg))))
		return;
	DWORD opts = 0;
	dlg->GetOptions (&opts);
	dlg->SetOptions (opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
	dlg->SetTitle (L"Download to");
	IShellItem *item = nullptr;
	if (SUCCEEDED (dlg->Show (g_wnd)) && SUCCEEDED (dlg->GetResult (&item)))
	{
		PWSTR path = nullptr;
		if (SUCCEEDED (item->GetDisplayName (SIGDN_FILESYSPATH, &path)))
		{
			startDownload (sel, path, nullptr);
			CoTaskMemFree (path);
		}
		item->Release ();
	}
	dlg->Release ();
}

// ---------------------------------------------------------------------------
// main window

enum
{
	ID_LIST = 1,
	ID_UP,
	ID_REFRESH,
	ID_MKDIR,
	ID_RECONNECT,
	ID_SERVER,
	ID_CANCEL,
	ID_THEME,
	ID_OPEN,
	ID_DOWNLOAD,
	ID_RENAME,
	ID_DELETE,
	ID_TIMER = 100,
};

static int S (int v) { return MulDiv (v, (int)g_dpi, 96); }

static void updateFont ()
{
	NONCLIENTMETRICSW ncm{sizeof (ncm)};
	SystemParametersInfoForDpi (SPI_GETNONCLIENTMETRICS, sizeof (ncm), &ncm, 0, g_dpi);
	HFONT old = g_font;
	g_font    = CreateFontIndirectW (&ncm.lfMessageFont);
	for (HWND h : {g_list, g_path, g_btnUp, g_btnRefresh, g_btnMkdir, g_btnTheme, g_btnReconnect, g_btnServer, g_btnCancel})
		SendMessageW (h, WM_SETFONT, (WPARAM)g_font, TRUE);
	if (old)
		DeleteObject (old);
}

static bool progressVisible ()
{
	std::lock_guard lk (g_prog.lock);
	return g_prog.active;
}

static void setStatus (int part, wstring const &text)
{
	if (g_statusText[part] == text)
		return;
	g_statusText[part] = text;
	RECT rc;
	GetClientRect (g_wnd, &rc);
	rc.top = rc.bottom - S (24);
	InvalidateRect (g_wnd, &rc, FALSE);
}

static void applyTheme ()
{
	auto t = theme ();
	makeBrushes ();
	darkTitleBar (g_wnd);
	setMenuMode ();
	for (HWND h : {g_btnUp, g_btnRefresh, g_btnMkdir, g_btnTheme, g_btnReconnect, g_btnServer, g_btnCancel})
		themeControl (h);
	SetWindowTheme (g_list, g_dark ? L"DarkMode_Explorer" : L"Explorer", nullptr);
	SetWindowTheme (ListView_GetHeader (g_list), g_dark ? L"DarkMode_ItemsView" : nullptr, nullptr);
	ListView_SetBkColor (g_list, t.list);
	ListView_SetTextBkColor (g_list, t.list);
	ListView_SetTextColor (g_list, t.text);
	// progress bar colors only apply without visual styles
	SetWindowTheme (g_progress, g_dark ? L"" : nullptr, g_dark ? L"" : nullptr);
	SendMessageW (g_progress, PBM_SETBKCOLOR, 0, g_dark ? (LPARAM)t.panel : (LPARAM)CLR_DEFAULT);
	SendMessageW (g_progress, PBM_SETBARCOLOR, 0, g_dark ? (LPARAM)t.accent : (LPARAM)CLR_DEFAULT);
	SetWindowTextW (g_btnTheme, g_dark ? L"Light mode" : L"Dark mode");
	RedrawWindow (g_wnd, nullptr, nullptr, RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
}

static void paint (HDC dc)
{
	auto t = theme ();
	RECT rc;
	GetClientRect (g_wnd, &rc);
	FillRect (dc, &rc, g_bgBrush);

	// status strip: connection state | transfer progress and messages
	RECT sr{0, rc.bottom - S (24), rc.right, rc.bottom};
	HBRUSH panel = CreateSolidBrush (t.panel), border = CreateSolidBrush (t.border);
	FillRect (dc, &sr, panel);
	RECT line{0, sr.top, rc.right, sr.top + 1};
	FillRect (dc, &line, border);
	RECT sep{S (260), sr.top + S (4), S (260) + 1, sr.bottom - S (4)};
	FillRect (dc, &sep, border);
	DeleteObject (panel);
	DeleteObject (border);

	HFONT old = (HFONT)SelectObject (dc, g_font);
	SetBkMode (dc, TRANSPARENT);
	SetTextColor (dc, t.text);
	UINT fmt   = DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX;
	RECT part0 = {S (8), sr.top + 1, S (252), sr.bottom};
	DrawTextW (dc, g_statusText[0].c_str (), -1, &part0, fmt);
	SetTextColor (dc, t.dim);
	RECT part1 = {S (268), sr.top + 1, rc.right - S (8), sr.bottom};
	DrawTextW (dc, g_statusText[1].c_str (), -1, &part1, fmt);
	SelectObject (dc, old);
}

// list view subclass: dark header text and dark rename box
static LRESULT CALLBACK listProc (HWND wnd, UINT msg, WPARAM wp, LPARAM lp, UINT_PTR, DWORD_PTR)
{
	if (msg == WM_NOTIFY && g_dark)
	{
		auto *nm = (NMHDR *)lp;
		if (nm->hwndFrom == ListView_GetHeader (wnd) && nm->code == NM_CUSTOMDRAW)
		{
			auto *cd = (NMCUSTOMDRAW *)lp;
			if (cd->dwDrawStage == CDDS_PREPAINT)
				return CDRF_NOTIFYITEMDRAW;
			if (cd->dwDrawStage == CDDS_ITEMPREPAINT)
			{
				SetTextColor (cd->hdc, theme ().text);
				return CDRF_DODEFAULT;
			}
		}
	}
	if (msg == WM_CTLCOLOREDIT)
		return ctlColor (msg, (HDC)wp);
	return DefSubclassProc (wnd, msg, wp, lp);
}

static void layout ()
{
	RECT rc;
	GetClientRect (g_wnd, &rc);
	int statusH = S (24);
	InvalidateRect (g_wnd, nullptr, FALSE);

	int pad = S (6), h = S (26), x = pad, y = pad;
	auto place = [&] (HWND w, int width) {
		MoveWindow (w, x, y, width, h, TRUE);
		x += width + S (4);
	};
	place (g_btnUp, S (48));
	place (g_btnRefresh, S (70));
	place (g_btnMkdir, S (86));
	int right = rc.right - pad - S (90) - S (4) - S (110) - S (4) - S (80);
	MoveWindow (g_path, x, y + S (2), std::max (S (40), right - x - S (4)), h - S (4), TRUE);
	x = right;
	place (g_btnTheme, S (90));
	place (g_btnReconnect, S (110));
	place (g_btnServer, S (80));

	int top    = y + h + pad;
	int bottom = rc.bottom - statusH;
	bool prog  = progressVisible ();
	if (prog)
	{
		int ph = S (24);
		MoveWindow (g_progress, pad, bottom - ph - S (3), rc.right - 3 * pad - S (80), ph - S (6), TRUE);
		MoveWindow (g_btnCancel, rc.right - pad - S (80), bottom - ph, S (80), ph - S (2), TRUE);
		bottom -= ph + S (2);
	}
	ShowWindow (g_progress, prog ? SW_SHOW : SW_HIDE);
	ShowWindow (g_btnCancel, prog ? SW_SHOW : SW_HIDE);
	MoveWindow (g_list, 0, top, rc.right, std::max (0, bottom - top), TRUE);
	ListView_SetColumnWidth (g_list, 0, std::max (S (100), (int)rc.right - S (110) - GetSystemMetricsForDpi (SM_CXVSCROLL, g_dpi)));
	ListView_SetColumnWidth (g_list, 1, S (100));
}

static void updateConnState ()
{
	string host;
	int port;
	{
		std::lock_guard lk (g_cfgLock);
		host = g_host;
		port = g_port;
	}
	wstring addr = wide (host) + L":" + std::to_wstring (port);
	wstring text = g_browse.state == Worker::Connected ? L"Connected to " + addr : L"Waiting for 3DS at " + addr + L"...";
	setStatus (0, text.c_str ());
}

static void updateProgress ()
{
	static bool wasActive;
	static uint64_t lastDone;
	static ULONGLONG lastTick;
	static double rate;

	bool active;
	wstring name;
	uint64_t done, total;
	int index, count;
	bool up;
	{
		std::lock_guard lk (g_prog.lock);
		active = g_prog.active;
		name   = g_prog.name;
		done   = g_prog.done;
		total  = g_prog.total;
		index  = g_prog.fileIndex;
		count  = g_prog.fileCount;
		up     = g_prog.up;
	}
	if (active != wasActive)
	{
		wasActive = active;
		rate      = 0;
		lastDone  = done;
		lastTick  = GetTickCount64 ();
		layout ();
		if (!active)
			setStatus (1, g_lastMsg.c_str ());
	}
	if (!active)
		return;

	ULONGLONG now = GetTickCount64 ();
	if (now - lastTick >= 500)
	{
		double inst = done >= lastDone ? (done - lastDone) * 1000.0 / (now - lastTick) : 0;
		rate        = rate == 0 ? inst : rate * 0.6 + inst * 0.4;
		lastDone    = done;
		lastTick    = now;
	}
	SendMessageW (g_progress, PBM_SETPOS, total ? (WPARAM)(done * 1000 / total) : 0, 0);

	wstring text;
	if (g_browse.state != Worker::Connected && g_xfer.state != Worker::Connected)
		text = L"Paused, waiting for 3DS...";
	else if (count)
	{
		text = wstring (up ? L"Uploading " : L"Downloading ") + std::to_wstring (index) + L"/" + std::to_wstring (count) +
		       L": " + name + L"   " + formatSize (done) + L" / " + formatSize (total) + L"   " +
		       formatSize ((uint64_t)rate) + L"/s";
		if (rate > 1 && total > done)
		{
			uint64_t eta = (uint64_t)((total - done) / rate);
			wchar_t buf[32];
			swprintf (buf, 32, L"   %llu:%02llu left", eta / 60, eta % 60);
			text += buf;
		}
	}
	else
		text = L"Preparing...";
	setStatus (1, text.c_str ());
}

static void showContextMenu (int x, int y)
{
	auto sel   = selectedEntries ();
	HMENU menu = CreatePopupMenu ();
	if (sel.size () == 1 && sel[0].dir)
		AppendMenuW (menu, MF_STRING, ID_OPEN, L"Open");
	if (!sel.empty ())
	{
		AppendMenuW (menu, MF_STRING, ID_DOWNLOAD, L"Download to...");
		AppendMenuW (menu, MF_STRING | (sel.size () == 1 ? 0 : MF_GRAYED), ID_RENAME, L"Rename\tF2");
		AppendMenuW (menu, MF_STRING, ID_DELETE, L"Delete\tDel");
		AppendMenuW (menu, MF_SEPARATOR, 0, nullptr);
	}
	AppendMenuW (menu, MF_STRING, ID_MKDIR, L"New folder");
	AppendMenuW (menu, MF_STRING, ID_REFRESH, L"Refresh\tF5");
	if (x == -1 && y == -1) // keyboard: open at the focused item
	{
		RECT r{};
		int row = ListView_GetNextItem (g_list, -1, LVNI_FOCUSED);
		if (row >= 0)
			ListView_GetItemRect (g_list, row, &r, LVIR_LABEL);
		POINT pt{r.left, r.bottom};
		ClientToScreen (g_list, &pt);
		x = pt.x;
		y = pt.y;
	}
	TrackPopupMenu (menu, TPM_RIGHTBUTTON, x, y, 0, g_wnd, nullptr);
	DestroyMenu (menu);
}

static void onCommand (int id)
{
	switch (id)
	{
	case ID_UP:
		if (currentDir () != L"/")
			navigate (parentRemote (currentDir ()));
		break;
	case ID_REFRESH:
		refresh ();
		break;
	case ID_MKDIR:
		newFolder ();
		break;
	case ID_RECONNECT:
		g_browse.reconnect ();
		g_xfer.reconnect ();
		break;
	case ID_SERVER:
		if (DialogBoxW (g_inst, MAKEINTRESOURCEW (IDD_SERVER), g_wnd, serverDlgProc) == IDOK)
		{
			updateConnState ();
			navigate (L"/");
			g_browse.reconnect ();
			g_xfer.reconnect ();
		}
		break;
	case ID_THEME:
		g_dark = !g_dark;
		saveConfig ();
		applyTheme ();
		break;
	case ID_CANCEL:
		g_cancel = true;
		g_xfer.clearPending ();
		break;
	case ID_OPEN:
		openRow (ListView_GetNextItem (g_list, -1, LVNI_SELECTED));
		break;
	case ID_DOWNLOAD:
		downloadTo ();
		break;
	case ID_RENAME:
	{
		int row = ListView_GetNextItem (g_list, -1, LVNI_SELECTED);
		if (row >= 0 && !isParentRow (row))
		{
			SetFocus (g_list);
			ListView_EditLabel (g_list, row);
		}
		break;
	}
	case ID_DELETE:
		deleteSelected ();
		break;
	}
}

static LRESULT onNotify (NMHDR *nm)
{
	if (nm->hwndFrom != g_list)
		return 0;
	switch (nm->code)
	{
	case NM_DBLCLK:
		openRow (((NMITEMACTIVATE *)nm)->iItem);
		break;
	case NM_RETURN:
		openRow (ListView_GetNextItem (g_list, -1, LVNI_FOCUSED));
		break;
	case LVN_KEYDOWN:
		switch (((NMLVKEYDOWN *)nm)->wVKey)
		{
		case VK_BACK:
			onCommand (ID_UP);
			break;
		case VK_DELETE:
			deleteSelected ();
			break;
		case VK_F2:
			onCommand (ID_RENAME);
			break;
		case VK_F5:
			refresh ();
			break;
		case 'A':
			if (GetKeyState (VK_CONTROL) < 0)
				ListView_SetItemState (g_list, -1, LVIS_SELECTED, LVIS_SELECTED);
			break;
		}
		break;
	case LVN_BEGINDRAG:
		beginDragOut ();
		break;
	case LVN_BEGINLABELEDITW:
		return isParentRow (((NMLVDISPINFOW *)nm)->item.iItem); // TRUE blocks editing ".."
	case LVN_ENDLABELEDITW:
	{
		auto *di = (NMLVDISPINFOW *)nm;
		auto e   = entryAt (di->item.iItem);
		if (!di->item.pszText || !e || !di->item.pszText[0] || e->name == di->item.pszText)
			return FALSE;
		wstring name = di->item.pszText;
		if (name.find_first_of (L"/\\") != wstring::npos)
			return FALSE;
		auto job   = std::make_shared<Job> ();
		job->type  = Job::Rename;
		job->path  = joinRemote (currentDir (), e->name);
		job->path2 = joinRemote (currentDir (), name);
		g_browse.push (job);
		return TRUE;
	}
	}
	return 0;
}

static HWND makeButton (wchar_t const *text, int id)
{
	return CreateWindowW (L"BUTTON", text, WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON, 0, 0, 0, 0, g_wnd, (HMENU)(INT_PTR)id, g_inst, nullptr);
}

static LRESULT CALLBACK wndProc (HWND wnd, UINT msg, WPARAM wp, LPARAM lp)
{
	switch (msg)
	{
	case WM_CREATE:
	{
		g_wnd = wnd;
		g_dpi = GetDpiForWindow (wnd);
		g_btnUp        = makeButton (L"Up", ID_UP);
		g_btnRefresh   = makeButton (L"Refresh", ID_REFRESH);
		g_btnMkdir     = makeButton (L"New folder", ID_MKDIR);
		g_btnTheme     = makeButton (L"Light mode", ID_THEME);
		g_btnReconnect = makeButton (L"Force reconnect", ID_RECONNECT);
		g_btnServer    = makeButton (L"Server...", ID_SERVER);
		g_btnCancel    = makeButton (L"Cancel", ID_CANCEL);
		g_path         = CreateWindowExW (0, L"EDIT", L"/", WS_CHILD | WS_VISIBLE | ES_READONLY | ES_AUTOHSCROLL, 0, 0, 0, 0, wnd, nullptr, g_inst, nullptr);
		g_progress     = CreateWindowExW (0, PROGRESS_CLASSW, nullptr, WS_CHILD | PBS_SMOOTH, 0, 0, 0, 0, wnd, nullptr, g_inst, nullptr);
		SendMessageW (g_progress, PBM_SETRANGE32, 0, 1000);
		g_list   = CreateWindowExW (0,
		      WC_LISTVIEWW,
		      nullptr,
		      WS_CHILD | WS_VISIBLE | WS_TABSTOP | LVS_REPORT | LVS_EDITLABELS | LVS_SHAREIMAGELISTS | LVS_SHOWSELALWAYS,
		      0,
		      0,
		      0,
		      0,
		      wnd,
		      (HMENU)ID_LIST,
		      g_inst,
		      nullptr);
		ListView_SetExtendedListViewStyle (g_list, LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER);
		SetWindowSubclass (g_list, listProc, 0, 0);
		SHFILEINFOW fi{};
		auto images = (HIMAGELIST)SHGetFileInfoW (L"folder", FILE_ATTRIBUTE_DIRECTORY, &fi, sizeof (fi), SHGFI_USEFILEATTRIBUTES | SHGFI_SYSICONINDEX | SHGFI_SMALLICON);
		ListView_SetImageList (g_list, images, LVSIL_SMALL);
		LVCOLUMNW col{LVCF_TEXT | LVCF_WIDTH, 0, 300, (LPWSTR)L"Name"};
		ListView_InsertColumn (g_list, 0, &col);
		col.mask |= LVCF_FMT;
		col.fmt     = LVCFMT_RIGHT;
		col.pszText = (LPWSTR)L"Size";
		ListView_InsertColumn (g_list, 1, &col);
		RegisterDragDrop (g_list, new DropTarget ());
		updateFont ();
		applyTheme ();
		SetTimer (wnd, ID_TIMER, 250, nullptr);
		return 0;
	}
	case WM_SIZE:
		layout ();
		return 0;
	case WM_ERASEBKGND:
		return 1;
	case WM_PAINT:
	{
		PAINTSTRUCT ps;
		HDC dc = BeginPaint (wnd, &ps);
		paint (dc);
		EndPaint (wnd, &ps);
		return 0;
	}
	case WM_CTLCOLORSTATIC:
	case WM_CTLCOLOREDIT:
	case WM_CTLCOLORBTN:
		return ctlColor (msg, (HDC)wp);
	case WM_SETFOCUS:
		SetFocus (g_list);
		return 0;
	case WM_DPICHANGED:
	{
		g_dpi    = HIWORD (wp);
		auto *r  = (RECT *)lp;
		updateFont ();
		SetWindowPos (wnd, nullptr, r->left, r->top, r->right - r->left, r->bottom - r->top, SWP_NOZORDER | SWP_NOACTIVATE);
		layout ();
		return 0;
	}
	case WM_GETMINMAXINFO:
		((MINMAXINFO *)lp)->ptMinTrackSize = {S (660), S (300)};
		return 0;
	case WM_COMMAND:
		onCommand (LOWORD (wp));
		return 0;
	case WM_NOTIFY:
		return onNotify ((NMHDR *)lp);
	case WM_CONTEXTMENU:
		if ((HWND)wp == g_list)
		{
			showContextMenu (GET_X_LPARAM (lp), GET_Y_LPARAM (lp));
			return 0;
		}
		break;
	case WM_TIMER:
		updateProgress ();
		return 0;
	case WM_APP_STATE:
		updateConnState ();
		return 0;
	case WM_APP_LIST:
	{
		std::unique_ptr<ListResult> r ((ListResult *)lp);
		showList (r.get ());
		return 0;
	}
	case WM_APP_MSG:
	{
		std::unique_ptr<wstring> text ((wstring *)lp);
		g_lastMsg = *text;
		if (!progressVisible ())
			setStatus (1, text->c_str ());
		return 0;
	}
	case WM_DESTROY:
		RevokeDragDrop (g_list);
		KillTimer (wnd, ID_TIMER);
		PostQuitMessage (0);
		return 0;
	}
	return DefWindowProcW (wnd, msg, wp, lp);
}

int WINAPI wWinMain (HINSTANCE inst, HINSTANCE, PWSTR, int show)
{
	g_inst = inst;
	SetProcessDpiAwarenessContext (DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
	OleInitialize (nullptr);
	INITCOMMONCONTROLSEX icc{sizeof (icc), ICC_LISTVIEW_CLASSES | ICC_BAR_CLASSES | ICC_PROGRESS_CLASS};
	InitCommonControlsEx (&icc);
	WSADATA wsa;
	WSAStartup (MAKEWORD (2, 2), &wsa);

	// leftovers from previous drag-out downloads
	deleteTree (tempRoot ());

	// ask for the server only once
	bool configured = loadConfig ();
	makeBrushes ();
	setMenuMode ();
	if (!configured && DialogBoxW (inst, MAKEINTRESOURCEW (IDD_SERVER), nullptr, serverDlgProc) != IDOK)
		return 0;

	WNDCLASSEXW wc{sizeof (wc)};
	wc.lpfnWndProc   = wndProc;
	wc.hInstance     = inst;
	wc.hCursor       = LoadCursor (nullptr, IDC_ARROW);
	wc.hbrBackground = nullptr; // painted in WM_PAINT
	wc.lpszClassName = L"synaftp";
	wc.hIcon         = LoadIconW (inst, MAKEINTRESOURCEW (IDI_APP));
	RegisterClassExW (&wc);

	HWND wnd = CreateWindowExW (0, L"synaftp", L"synaftp", WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT, 0, 0, nullptr, nullptr, inst, nullptr);
	RECT r{0, 0, S (760), S (520)};
	AdjustWindowRectExForDpi (&r, WS_OVERLAPPEDWINDOW, FALSE, 0, g_dpi);
	SetWindowPos (wnd, nullptr, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER);
	ShowWindow (wnd, show);
	updateConnState ();

	g_browse.start ();
	g_xfer.start ();

	MSG msg;
	while (GetMessageW (&msg, nullptr, 0, 0))
	{
		TranslateMessage (&msg);
		DispatchMessageW (&msg);
	}

	g_cancel = true;
	g_browse.stop ();
	g_xfer.stop ();
	WSACleanup ();
	OleUninitialize ();
	return 0;
}
