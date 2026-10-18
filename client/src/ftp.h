// Cement - FTP engine shared by the GUI client (main.cpp) and the
// command-line tool for scripts and automation (cli.cpp). Windows and Linux.
#pragma once

#ifdef _WIN32
#include <winsock2.h>
#include <ws2tcpip.h>
#include <mstcpip.h>
#include <windows.h>
#else
#include <arpa/inet.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

#include <cerrno>
#include <cwchar>
#endif

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

using std::string;
using std::wstring;

#ifndef APP_VERSION
#define APP_VERSION "dev" // set by build.bat / build.sh: <VERSION file>.<commit count>-<commit>[-dev]
#endif

// The 3DS app must report at least this protocol level (SITE STATUS
// "protocol="). Bump it together with PROTOCOL in server/source/main.c, and
// only for changes that really need the new 3DS build: the PC apps warn about
// an outdated 3DS app only when it is below this level.
static int const REQUIRED_PROTOCOL = 1;

// protocol level from a SITE STATUS reply; builds from before the field
// existed already had everything of level 1, a server without SITE STATUS is 0
static int protocolLevel (int code, string const &text)
{
	if (code != 211)
		return 0;
	size_t pos = text.find (" protocol=");
	return pos == string::npos ? 1 : atoi (text.c_str () + pos + 10);
}

// a number field (" name=123") from a SITE STATUS reply's summary line
static bool statusNumber (string const &text, char const *name, uint64_t *out)
{
	string key = string (" ") + name + "=";
	size_t pos = text.find (key);
	if (pos == string::npos)
		return false;
	*out = strtoull (text.c_str () + pos + key.size (), nullptr, 10);
	return true;
}

// ---------------------------------------------------------------------------
// utilities

#ifdef _WIN32
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
#else
// wchar_t is UTF-32 on Linux
static string utf8 (wstring const &w)
{
	string s;
	for (wchar_t wc : w)
	{
		uint32_t c = (uint32_t)wc;
		if (c < 0x80)
			s += (char)c;
		else if (c < 0x800)
			s += (char)(0xC0 | (c >> 6)), s += (char)(0x80 | (c & 0x3F));
		else if (c < 0x10000)
			s += (char)(0xE0 | (c >> 12)), s += (char)(0x80 | ((c >> 6) & 0x3F)), s += (char)(0x80 | (c & 0x3F));
		else
			s += (char)(0xF0 | (c >> 18)), s += (char)(0x80 | ((c >> 12) & 0x3F)), s += (char)(0x80 | ((c >> 6) & 0x3F)),
			    s += (char)(0x80 | (c & 0x3F));
	}
	return s;
}

static wstring wide (string const &s)
{
	wstring w;
	for (size_t i = 0; i < s.size ();)
	{
		unsigned char c = (unsigned char)s[i];
		int extra       = c >= 0xF0 ? 3 : c >= 0xE0 ? 2 : c >= 0xC0 ? 1 : 0;
		uint32_t cp     = extra == 3 ? c & 0x07 : extra == 2 ? c & 0x0F : extra == 1 ? c & 0x1F : c;
		++i;
		for (int k = 0; k < extra && i < s.size (); ++k, ++i)
			cp = (cp << 6) | ((unsigned char)s[i] & 0x3F);
		w += (wchar_t)cp;
	}
	return w;
}
#endif

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

struct Entry
{
	wstring name;
	bool dir;
	uint64_t size;
};

// ---------------------------------------------------------------------------
// platform: sockets and local files

#ifdef _WIN32
static wchar_t const PATH_SEP = L'\\';
#else
static wchar_t const PATH_SEP = L'/';
typedef int SOCKET;
static int const INVALID_SOCKET = -1;
static int const SD_BOTH        = SHUT_RDWR;
static int const SD_SEND        = SHUT_WR;
static int closesocket (SOCKET s) { return ::close (s); }
#endif

// a local file for transfers, opened for reading or writing
class LocalFile
{
public:
	~LocalFile () { close (); }

	bool openRead (wstring const &path)
	{
#ifdef _WIN32
		m_h = CreateFileW (path.c_str (), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, FILE_FLAG_SEQUENTIAL_SCAN, nullptr);
		return m_h != INVALID_HANDLE_VALUE;
#else
		m_fd = ::open (utf8 (path).c_str (), O_RDONLY);
		return m_fd >= 0;
#endif
	}

	// keep: open an existing file without emptying it (to resume)
	bool openWrite (wstring const &path, bool keep)
	{
#ifdef _WIN32
		m_h = CreateFileW (path.c_str (), GENERIC_WRITE, FILE_SHARE_READ, nullptr, keep ? OPEN_ALWAYS : CREATE_ALWAYS, FILE_FLAG_SEQUENTIAL_SCAN, nullptr);
		return m_h != INVALID_HANDLE_VALUE;
#else
		m_fd = ::open (utf8 (path).c_str (), O_WRONLY | O_CREAT | (keep ? 0 : O_TRUNC), 0644);
		return m_fd >= 0;
#endif
	}

	uint64_t size ()
	{
#ifdef _WIN32
		LARGE_INTEGER s{};
		GetFileSizeEx (m_h, &s);
		return (uint64_t)s.QuadPart;
#else
		struct stat st{};
		fstat (m_fd, &st);
		return (uint64_t)st.st_size;
#endif
	}

	// moves to offset; with cut, the file ends there
	void seek (uint64_t offset, bool cut = false)
	{
#ifdef _WIN32
		LARGE_INTEGER li;
		li.QuadPart = (LONGLONG)offset;
		SetFilePointerEx (m_h, li, nullptr, FILE_BEGIN);
		if (cut)
			SetEndOfFile (m_h);
#else
		lseek (m_fd, (off_t)offset, SEEK_SET);
		if (cut && ftruncate (m_fd, (off_t)offset) != 0)
		{
		}
#endif
	}

	// bytes read, 0 at the end, -1 on error
	long long read (char *buf, size_t n)
	{
#ifdef _WIN32
		DWORD got = 0;
		return ReadFile (m_h, buf, (DWORD)n, &got, nullptr) ? (long long)got : -1;
#else
		return (long long)::read (m_fd, buf, n);
#endif
	}

	bool write (char const *buf, size_t n)
	{
#ifdef _WIN32
		DWORD put = 0;
		return WriteFile (m_h, buf, (DWORD)n, &put, nullptr) && put == (DWORD)n;
#else
		while (n > 0)
		{
			ssize_t w = ::write (m_fd, buf, n);
			if (w <= 0)
				return false;
			buf += w;
			n -= (size_t)w;
		}
		return true;
#endif
	}

	void close ()
	{
#ifdef _WIN32
		if (m_h != INVALID_HANDLE_VALUE)
			CloseHandle (m_h);
		m_h = INVALID_HANDLE_VALUE;
#else
		if (m_fd >= 0)
			::close (m_fd);
		m_fd = -1;
#endif
	}

private:
#ifdef _WIN32
	HANDLE m_h = INVALID_HANDLE_VALUE;
#else
	int m_fd = -1;
#endif
};

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
		int one = 1;
		setsockopt (s, IPPROTO_TCP, TCP_NODELAY, (char const *)&one, sizeof (one));
#ifdef _WIN32
		// acknowledge every reply at once: while a file goes over the data
		// connection nothing else is sent on this one, and a delayed ACK of
		// "150" made the server's stack hold "226" back (~200 ms per file)
		DWORD ackEvery = 1, unused = 0;
		WSAIoctl (s, SIO_TCP_SET_ACK_FREQUENCY, &ackEvery, sizeof (ackEvery), nullptr, 0, &unused, nullptr, nullptr);
#endif
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
				code = cmd ("PASS cement");
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
			int n = (int)recv (m_data, buf, sizeof (buf), 0);
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
					e.size = strtoull (val.c_str (), nullptr, 10);
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
		out = strtoull (text.c_str () + 4, nullptr, 10);
		return true;
	}

	// last modification time "YYYYMMDDhhmmss"; false if missing or unsupported.
	// The 3DS reports its own clock, which is local time.
	bool mdtm (wstring const &path, string &out)
	{
		string text;
		if (cmd ("MDTM " + utf8 (path), &text) != 213 || text.size () < 18)
			return false;
		out = text.substr (4, 14);
		return true;
	}

	void simple (string const &line, int expect)
	{
		string text;
		if (cmd (line, &text) / 100 != expect / 100)
			throw FtpError{text};
	}

	// version ("" if it does not say) and protocol level of the 3DS app
	struct ServerInfo
	{
		string version;
		int protocol = 0;
		bool hasSpace = false; // the 3DS app reports SD space (sdFree, sdTotal)
		uint64_t sdFree = 0, sdTotal = 0;
	};
	ServerInfo serverInfo ()
	{
		ServerInfo info;
		string text;
		int code      = cmd ("SITE STATUS", &text);
		info.protocol = protocolLevel (code, text);
		info.hasSpace = code == 211 && statusNumber (text, "sd_free", &info.sdFree) &&
		                statusNumber (text, "sd_total", &info.sdTotal) && info.sdTotal;
		size_t pos    = code == 211 ? text.find (" version=") : string::npos;
		if (pos != string::npos)
		{
			pos += 9;
			info.version = text.substr (pos, text.find_first_of (" \r\n", pos) - pos);
		}
		return info;
	}

	// free bytes on the SD card (AVBL); false if the server doesn't say
	bool freeBytes (uint64_t *out)
	{
		string text;
		if (cmd ("AVBL", &text) != 213)
			return false;
		*out = strtoull (text.c_str () + 4, nullptr, 10);
		return true;
	}

	// Do these files (remote path, size) fit on the SD card? False only when they
	// surely don't: the bytes to write, minus the files they would overwrite, are
	// more than the free space. True when the server doesn't report free space.
	// The sizes of existing files are only asked for when the total doesn't fit.
	bool fits (std::vector<std::pair<wstring, uint64_t>> const &files, uint64_t *need, uint64_t *avail)
	{
		*need = 0;
		for (auto const &f : files)
			*need += f.second;
		if (!freeBytes (avail) || *need <= *avail)
			return true;
		for (auto const &f : files)
		{
			uint64_t old = 0;
			if (size (f.first, old))
				*need -= std::min (old, *need);
			if (*need <= *avail)
				return true;
		}
		return false;
	}

	// tells the 3DS a multi-file job is running (SITE STATUS shows it as busy with an
	// ETA, even between files); files == 0 ends it. Ignored by other servers.
	void announceBatch (uint64_t files, uint64_t bytes, char const *kind)
	{
		cmd ("SITE BATCH " + std::to_string (files) + " " + std::to_string (bytes) + " " + kind);
	}

	// ends the batch when the job leaves its scope normally. On an exception the
	// caller decides: a dead link needs nothing (the 3DS drops the batch with the
	// connection), a cancel or error on a live link calls endBatch ().
	struct BatchGuard
	{
		Ftp &ftp;
		~BatchGuard ()
		{
			if (std::uncaught_exceptions () == 0)
				ftp.endBatch ();
		}
	};

	void endBatch ()
	{
		try
		{
			if (connected ())
				announceBatch (0, 0, "up");
		}
		catch (...)
		{
		}
	}

	// progress(bytes) is called for every chunk; it throws CancelError to stop
	void upload (wstring const &local, wstring const &remote, uint64_t offset, std::function<void (uint64_t)> const &progress)
	{
		LocalFile file;
		if (!file.openRead (local))
			throw FtpError{"Can't open local file " + utf8 (local)};
		file.seek (offset);

		openData ();
		string text;
		// announce the full size first so the 3DS can report progress and time left
		// (before REST: every other command resets the restart offset)
		cmd ("ALLO " + std::to_string (file.size ()));
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
				long long n = file.read (buf.data (), buf.size ());
				if (n < 0)
					throw FtpError{"Can't read local file " + utf8 (local)};
				if (n == 0)
					break;
				sendAll (m_data, buf.data (), (size_t)n);
				progress ((uint64_t)n);
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
		LocalFile file;
		if (!file.openWrite (local, offset != 0))
			throw FtpError{"Can't create local file " + utf8 (local)};
		file.seek (offset, true);

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
				int n = (int)recv (m_data, buf.data (), (int)buf.size (), 0);
				if (n == 0)
					break;
				if (n < 0)
					throw NetError ();
				if (!file.write (buf.data (), (size_t)n))
					throw FtpError{"Can't write local file " + utf8 (local)};
				progress ((uint64_t)n);
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
	static void setTimeout (SOCKET s, int ms)
	{
#ifdef _WIN32
		DWORD t = (DWORD)ms;
#else
		timeval t{ms / 1000, (ms % 1000) * 1000};
#endif
		setsockopt (s, SOL_SOCKET, SO_RCVTIMEO, (char const *)&t, sizeof (t));
		setsockopt (s, SOL_SOCKET, SO_SNDTIMEO, (char const *)&t, sizeof (t));
	}

	static void setBlocking (SOCKET s, bool blocking)
	{
#ifdef _WIN32
		u_long nb = blocking ? 0 : 1;
		ioctlsocket (s, FIONBIO, &nb);
#else
		int flags = fcntl (s, F_GETFL, 0);
		fcntl (s, F_SETFL, blocking ? flags & ~O_NONBLOCK : flags | O_NONBLOCK);
#endif
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
		setBlocking (s, false);
		int rc = ::connect (s, res->ai_addr, (int)res->ai_addrlen);
		freeaddrinfo (res);
#ifdef _WIN32
		bool pending = rc != 0 && WSAGetLastError () == WSAEWOULDBLOCK;
#else
		bool pending = rc != 0 && errno == EINPROGRESS;
#endif
		if (pending)
		{
			fd_set w, e;
			FD_ZERO (&w);
			FD_ZERO (&e);
			FD_SET (s, &w);
			FD_SET (s, &e);
			timeval tv{timeoutMs / 1000, (timeoutMs % 1000) * 1000};
			rc = select ((int)s + 1, nullptr, &w, &e, &tv) == 1 && FD_ISSET (s, &w) ? 0 : -1;
#ifndef _WIN32
			int err       = 0;
			socklen_t len = sizeof (err);
			if (rc == 0 && (getsockopt (s, SOL_SOCKET, SO_ERROR, &err, &len) != 0 || err != 0))
				rc = -1; // Linux reports a refused connection as writable
#endif
		}
		if (rc != 0)
		{
			closesocket (s);
			return INVALID_SOCKET;
		}
		setBlocking (s, true);
		return s;
	}

	void sendAll (SOCKET s, char const *p, size_t n)
	{
#ifdef _WIN32
		int const flags = 0;
#else
		int const flags = MSG_NOSIGNAL; // a closed connection is an error, not a signal
#endif
		while (n > 0)
		{
			int r = (int)send (s, p, (int)std::min<size_t> (n, 1 << 20), flags);
			if (r <= 0)
				throw NetError ();
			p += r;
			n -= (size_t)r;
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
			int n = (int)recv (m_ctrl, buf, sizeof (buf), 0);
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
