// Syna 3DS Tools - FTP engine shared by the GUI client (main.cpp) and the
// command-line tool for agents and scripts (cli.cpp)
#pragma once

#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>

#include <algorithm>
#include <cctype>
#include <cstdint>
#include <cstdio>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

using std::string;
using std::wstring;

#ifndef APP_VERSION
#define APP_VERSION "dev" // set by build.bat: <VERSION file>.<commit count>-<commit>[-dev]
#endif

// build number of "1.2.47-abc1234[-dev]" (the commit count), -1 if unknown
static int buildNumber (string const &v)
{
	int a = 0, b = 0, c = -1;
	return sscanf (v.c_str (), "%d.%d.%d", &a, &b, &c) == 3 ? c : -1;
}

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

struct Entry
{
	wstring name;
	bool dir;
	uint64_t size;
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
				code = cmd ("PASS syna3dstools");
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

	// version of the Syna 3DS Tools app on the 3DS ("" if it does not say:
	// a build older than 1.2, or another FTP server)
	string serverVersion ()
	{
		string text;
		if (cmd ("SITE STATUS", &text) != 211)
			return {};
		size_t pos = text.find (" version=");
		if (pos == string::npos)
			return {};
		pos += 9;
		return text.substr (pos, text.find_first_of (" \r\n", pos) - pos);
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
		// announce the full size first so the 3DS can report progress and time left
		// (before REST: every other command resets the restart offset)
		LARGE_INTEGER fileSize{};
		GetFileSizeEx (h, &fileSize);
		cmd ("ALLO " + std::to_string (fileSize.QuadPart));
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
