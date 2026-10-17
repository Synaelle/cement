// Cement client - minimal Windows FTP explorer for the Cement server
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

#include "ftp.h"
#include "resource.h"

using std::string;
using std::wstring;

// ---------------------------------------------------------------------------
// utilities

// "Replace PC folder" must never empty a drive or a personal/system folder
static bool unsafeReplaceTarget (wstring const &dir)
{
	wchar_t full[MAX_PATH];
	if (dir.empty () || !GetFullPathNameW (dir.c_str (), MAX_PATH, full, nullptr))
		return true;
	wstring f = full;
	while (f.size () > 3 && (f.back () == L'\\' || f.back () == L'/'))
		f.pop_back ();
	if (f.size () <= 3) // "C:\" or shorter
		return true;
	for (auto const &id : {FOLDERID_Profile, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads,
	         FOLDERID_Pictures, FOLDERID_Music, FOLDERID_Videos, FOLDERID_Windows, FOLDERID_ProgramFiles,
	         FOLDERID_ProgramFilesX86, FOLDERID_RoamingAppData, FOLDERID_LocalAppData, FOLDERID_Public})
	{
		PWSTR known = nullptr;
		bool same   = SUCCEEDED (SHGetKnownFolderPath (id, 0, nullptr, &known)) && _wcsicmp (known, f.c_str ()) == 0;
		CoTaskMemFree (known);
		if (same)
			return true;
	}
	return false;
}

// delete everything inside dir (not dir itself); to the Recycle Bin when recycle
static bool emptyFolder (wstring const &dir, bool recycle)
{
	wstring list; // double-null-terminated list for SHFileOperation
	WIN32_FIND_DATAW fd;
	HANDLE h = FindFirstFileW ((dir + L"\\*").c_str (), &fd);
	if (h == INVALID_HANDLE_VALUE)
		return true;
	do
	{
		if (wcscmp (fd.cFileName, L".") && wcscmp (fd.cFileName, L".."))
			list += dir + L"\\" + fd.cFileName + L'\0';
	} while (FindNextFileW (h, &fd));
	FindClose (h);
	if (list.empty ())
		return true;
	list += L'\0';
	SHFILEOPSTRUCTW op{};
	op.wFunc  = FO_DELETE;
	op.pFrom  = list.c_str ();
	op.fFlags = FOF_NO_UI | (recycle ? FOF_ALLOWUNDO : 0);
	return SHFileOperationW (&op) == 0 && !op.fAnyOperationsAborted;
}

// ---------------------------------------------------------------------------
// shared state

enum
{
	WM_APP_STATE = WM_APP + 1, // a worker's connection state changed
	WM_APP_LIST,               // lParam: ListResult*
	WM_APP_MSG,                // lParam: wstring* (status message)
};

struct ListResult
{
	wstring path;
	std::vector<Entry> entries;
	bool ok;
};

static HINSTANCE g_inst;
static HWND g_wnd, g_list, g_path, g_progress;
static HWND g_btnUp, g_btnRefresh, g_btnMkdir, g_btnRename, g_btnSend, g_btnReconnect, g_btnSettings, g_btnCancel;
static wstring g_sendDir;                // "Send to PC" destination folder (settings)
static std::vector<wstring> g_sendItems; // "Send to PC" 3DS paths (settings)
static bool g_sendReplace;                // "Send to PC" empties the PC folder first
static bool g_dark = true;     // dark theme (saved in config)
static wstring g_statusText[2]; // connection state, transfer/messages
static void setStatus (int part, wstring const &text);
static HFONT g_font;
static UINT g_dpi = 96;

static std::mutex g_cfgLock;
static string g_host;
static string g_serverVersion; // Cement version on the 3DS ("" = too old to say)
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
	bool del    = false; // deletion: done/total count items, not bytes
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
		Download,
		SendToPc
	} type;

	wstring path;  // List/Mkdir: target. Rename: from. Upload: remote dir. Download/Delete: remote dir
	wstring path2; // Rename: to. Download: local dir
	std::vector<wstring> locals; // Upload sources
	std::vector<Entry> remotes;  // Download/Delete: entries inside path
	std::vector<wstring> sendPaths; // SendToPc: absolute 3DS paths, copied under path2
	HWND notify = nullptr;          // List: window receiving WM_APP_LIST (main window if null)
	int missing = 0;                // SendToPc: paths not found on the 3DS
	bool replace = false;           // SendToPc: empty path2 first (once, when connected)
	bool recycle = true;            // ...into the Recycle Bin

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
				                  [&] (JobPtr const &j) { return j->type == Job::List && j->notify == job->notify && j != m_running; }),
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
		CoInitializeEx (nullptr, COINIT_APARTMENTTHREADED);
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
					if (!m_transfer)
					{
						// shown next to "Connected", with a hint when the 3DS app is older
						string version;
						try
						{
							version = m_ftp.serverVersion ();
						}
						catch (NetError &)
						{
						}
						std::lock_guard lk (g_cfgLock);
						g_serverVersion = version;
					}
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
				m_ftp.endBatch ();
				postMessageText (L"Transfer cancelled");
			}
			catch (FtpError &e)
			{
				m_ftp.endBatch ();
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
			if (!PostMessageW (job.notify ? job.notify : g_wnd, WM_APP_LIST, 0, (LPARAM)r))
				delete r; // the picker closed meanwhile
			break;
		}
		case Job::Mkdir:
			m_ftp.simple ("MKD " + utf8 (job.path), 257);
			break;
		case Job::Rename:
			m_ftp.simple ("RNFR " + utf8 (job.path), 350);
			m_ftp.simple ("RNTO " + utf8 (job.path2), 250);
			postMessageText (L"Renamed " + job.path.substr (job.path.find_last_of (L'/') + 1) + L" to " +
			                 job.path2.substr (job.path2.find_last_of (L'/') + 1));
			break;
		case Job::Delete:
			deleteItems (job);
			break;
		case Job::Upload:
		case Job::Download:
		case Job::SendToPc:
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

	// folder contents first, the folder itself last
	void expandDelete (Job &job, wstring const &remote, bool dir)
	{
		if (dir)
			for (auto const &child : m_ftp.list (remote))
				expandDelete (job, joinRemote (remote, child.name), child.dir);
		job.items.push_back ({L"", remote, dir, 0});
	}

	// file by file, so no single command runs long on the 3DS; resumable like transfers
	void deleteItems (Job &job)
	{
		if (!job.expanded)
		{
			for (auto const &e : job.remotes)
				expandDelete (job, joinRemote (job.path, e.name), e.dir);
			job.expanded = true;
		}
		string firstError;
		bool resuming = job.partial; // the last attempt was cut off mid-item
		m_ftp.announceBatch (job.items.size () - job.next, 0, "delete");
		Ftp::BatchGuard batch{m_ftp};
		for (; job.next < job.items.size (); ++job.next)
		{
			if (g_cancel)
				throw CancelError ();
			auto const &it = job.items[job.next];
			{
				std::lock_guard lk (g_prog.lock);
				g_prog.active    = true;
				g_prog.del       = true;
				g_prog.name      = it.remote.substr (it.remote.find_last_of (L'/') + 1);
				g_prog.fileIndex = (int)job.next + 1;
				g_prog.fileCount = (int)job.items.size ();
				g_prog.done      = job.next;
				g_prog.total     = job.items.size ();
			}
			job.partial = true;
			string text;
			int code = m_ftp.cmd ((it.dir ? "RMD " : "DELE ") + utf8 (it.remote), &text);
			// after a reconnect the interrupted item may already be gone: that is fine
			if (code / 100 != 2 && !resuming)
			{
				++job.errors;
				if (firstError.empty ())
					firstError = utf8 (it.remote) + ": " + text.substr (0, text.find_last_not_of ("\r\n") + 1);
			}
			job.partial = false;
			resuming    = false;
		}
		if (job.errors)
			postMessageText (L"Deleted with " + std::to_wstring (job.errors) + L" error(s), first: " + wide (firstError));
		else
			postMessageText (L"Deleted " + std::to_wstring (job.items.size ()) + L" item(s)");
	}

	void transfer (Job &job)
	{
		bool up = job.type == Job::Upload;
		if (!job.expanded)
		{
			if (up)
				for (auto const &l : job.locals)
					expandLocal (job, l, joinRemote (job.path, l.substr (l.find_last_of (L"\\/") + 1)));
			else if (job.type == Job::SendToPc)
			{
				if (job.replace)
				{
					if (unsafeReplaceTarget (job.path2))
						throw FtpError{"Refusing to empty " + utf8 (job.path2) + " (drive or personal/system folder)"};
					if (!emptyFolder (job.path2, job.recycle))
						throw FtpError{"Could not empty " + utf8 (job.path2) + " (a file may be in use)"};
					job.replace = false; // only once, even if the job resumes after a reconnect
				}
				for (auto const &rp : job.sendPaths)
				{
					if (rp == L"/") // the whole SD card: its contents go straight into the PC folder
					{
						expandRemote (job, rp, job.path2, Entry{L"", true, 0});
						continue;
					}
					// each item lands directly in the PC folder: /openmw/logs -> <PC folder>\logs
					wstring name  = rp.substr (rp.find_last_of (L'/') + 1);
					wstring local = job.path2 + L"\\" + name;
					// find the item in its parent folder to know if it is a folder or a file
					std::vector<Entry> siblings;
					try
					{
						siblings = m_ftp.list (parentRemote (rp));
					}
					catch (FtpError &)
					{
					}
					auto it = std::find_if (siblings.begin (), siblings.end (), [&] (Entry const &e) { return e.name == name; });
					if (it == siblings.end ())
					{
						++job.missing;
						continue;
					}
					expandRemote (job, rp, local, *it);
				}
			}
			else
				for (auto const &e : job.remotes)
					expandRemote (job, joinRemote (job.path, e.name), job.path2 + L"\\" + e.name, e);
			job.expanded = true;
		}

		int fileCount = 0, filesLeft = 0;
		for (size_t i = 0; i < job.items.size (); ++i)
		{
			fileCount += !job.items[i].dir;
			filesLeft += !job.items[i].dir && i >= job.next;
		}
		// let the 3DS (and other clients asking it) know what is left of this job
		m_ftp.announceBatch (filesLeft, job.total - job.done, up ? "up" : "down");
		Ftp::BatchGuard batch{m_ftp};

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
				g_prog.del       = false;
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

		wstring missing = job.missing ? L"  (" + std::to_wstring (job.missing) + L" item(s) not found on the 3DS)" : L"";
		if (!job.errors)
			postMessageText (wstring (up ? L"Upload" : job.type == Job::SendToPc ? L"Send to PC" : L"Download") +
			                 L" complete: " + std::to_wstring (fileCount) + L" file(s), " + formatSize (job.total) + missing);
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
	// CEMENT_CONFIG points to another config file (tests, portable use)
	wchar_t custom[MAX_PATH];
	if (GetEnvironmentVariableW (L"CEMENT_CONFIG", custom, MAX_PATH))
		return custom;
	PWSTR base = nullptr;
	wstring path;
	if (SUCCEEDED (SHGetKnownFolderPath (FOLDERID_RoamingAppData, 0, nullptr, &base)))
	{
		path = wstring (base) + L"\\Cement";
		CreateDirectoryW (path.c_str (), nullptr);
		path += L"\\config.ini";
		// settings saved under the project's earlier names
		for (wchar_t const *old : {L"\\Syna 3DS Tools\\config.ini", L"\\synaftp\\config.ini"})
			if (GetFileAttributesW (path.c_str ()) == INVALID_FILE_ATTRIBUTES)
				CopyFileW ((wstring (base) + old).c_str (), path.c_str (), TRUE);
	}
	CoTaskMemFree (base);
	return path;
}

// "%XX"-escape non-ASCII so the list survives an ANSI ini file
static wstring escapeIni (wstring const &w)
{
	wstring out;
	for (unsigned char c : utf8 (w))
	{
		if (c >= 0x80 || c < 0x20 || c == '%')
		{
			wchar_t buf[4];
			swprintf (buf, 4, L"%%%02X", c);
			out += buf;
		}
		else
			out += (wchar_t)c;
	}
	return out;
}

static wstring unescapeIni (wstring const &w)
{
	string s;
	for (size_t i = 0; i < w.size (); ++i)
	{
		if (w[i] == L'%' && i + 2 < w.size ())
		{
			s += (char)wcstoul (w.substr (i + 1, 2).c_str (), nullptr, 16);
			i += 2;
		}
		else
			s += (char)w[i];
	}
	return wide (s);
}

static bool loadConfig ()
{
	wchar_t host[256] = {};
	auto path         = configPath ();
	g_dark            = GetPrivateProfileIntW (L"ui", L"dark", 1, path.c_str ()) != 0;
	GetPrivateProfileStringW (L"server", L"host", L"", host, 256, path.c_str ());
	int port = GetPrivateProfileIntW (L"server", L"port", 5000, path.c_str ());
	wchar_t dir[MAX_PATH] = {};
	GetPrivateProfileStringW (L"transfer", L"sendto", L"", dir, MAX_PATH, path.c_str ());
	g_sendDir     = dir;
	g_sendReplace = GetPrivateProfileIntW (L"transfer", L"replace", 0, path.c_str ()) != 0;
	g_sendItems.clear ();
	int count = GetPrivateProfileIntW (L"sendtopc", L"count", 0, path.c_str ());
	for (int i = 0; i < count; ++i)
	{
		wchar_t item[4096] = {};
		GetPrivateProfileStringW (L"sendtopc", (L"item" + std::to_wstring (i)).c_str (), L"", item, 4096, path.c_str ());
		if (item[0])
			g_sendItems.push_back (unescapeIni (item));
	}
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
	WritePrivateProfileStringW (L"transfer", L"sendto", g_sendDir.c_str (), path.c_str ());
	WritePrivateProfileStringW (L"transfer", L"replace", g_sendReplace ? L"1" : L"0", path.c_str ());
	WritePrivateProfileStringW (L"sendtopc", nullptr, nullptr, path.c_str ()); // drop the old list
	WritePrivateProfileStringW (L"sendtopc", L"count", std::to_wstring (g_sendItems.size ()).c_str (), path.c_str ());
	for (size_t i = 0; i < g_sendItems.size (); ++i)
		WritePrivateProfileStringW (L"sendtopc", (L"item" + std::to_wstring (i)).c_str (), escapeIni (g_sendItems[i]).c_str (), path.c_str ());
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
	if (msg == WM_CTLCOLOREDIT || msg == WM_CTLCOLORLISTBOX)
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

static bool pickFolder (HWND owner, wchar_t const *title, wstring &out)
{
	IFileOpenDialog *dlg = nullptr;
	if (FAILED (CoCreateInstance (CLSID_FileOpenDialog, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS (&dlg))))
		return false;
	DWORD opts = 0;
	dlg->GetOptions (&opts);
	dlg->SetOptions (opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
	dlg->SetTitle (title);
	bool ok          = false;
	IShellItem *item = nullptr;
	if (SUCCEEDED (dlg->Show (owner)) && SUCCEEDED (dlg->GetResult (&item)))
	{
		PWSTR path = nullptr;
		if (SUCCEEDED (item->GetDisplayName (SIGDN_FILESYSPATH, &path)))
		{
			out = path;
			ok  = true;
			CoTaskMemFree (path);
		}
		item->Release ();
	}
	dlg->Release ();
	return ok;
}

static void applyTheme ();
static bool pickRemote (HWND owner, std::vector<wstring> &out);
static bool browseConnected ();
static HWND s_settingsDlg;

// "/a/b" form: forward slashes, leading slash, no trailing slash
static wstring normalizeRemote (wstring p)
{
	for (auto &c : p)
		if (c == L'\\')
			c = L'/';
	p.erase (0, p.find_first_not_of (L' '));
	p.erase (p.find_last_not_of (L' ') + 1);
	wstring out;
	for (wchar_t c : L"/" + p)
		if (!(c == L'/' && !out.empty () && out.back () == L'/'))
			out += c;
	if (out.size () > 1 && out.back () == L'/')
		out.pop_back ();
	return out;
}

static void addSendItem (HWND dlg, wstring const &path)
{
	HWND list = GetDlgItem (dlg, IDC_ITEMS);
	if (SendMessageW (list, LB_FINDSTRINGEXACT, (WPARAM)-1, (LPARAM)path.c_str ()) == LB_ERR)
		SendMessageW (list, LB_ADDSTRING, 0, (LPARAM)path.c_str ());
}

static void updateSendSection (HWND dlg)
{
	bool on = browseConnected ();
	for (int id : {IDC_ITEMS, IDC_ADDPICK, IDC_REMOVE, IDC_NEWPATH, IDC_ADDPATH})
		EnableWindow (GetDlgItem (dlg, id), on);
	SetDlgItemTextW (dlg, IDC_SENDNOTE, on ? L"" : L"Connect to the 3DS to change this list.");
}

static INT_PTR CALLBACK settingsDlgProc (HWND dlg, UINT msg, WPARAM wp, LPARAM)
{
	switch (msg)
	{
	case WM_CTLCOLORDLG:
	case WM_CTLCOLORSTATIC:
	case WM_CTLCOLOREDIT:
	case WM_CTLCOLORBTN:
	case WM_CTLCOLORLISTBOX:
		return (INT_PTR)ctlColor (msg, (HDC)wp);
	case WM_APP_STATE:
		updateSendSection (dlg);
		return TRUE;
	case WM_DESTROY:
		s_settingsDlg = nullptr;
		break;
	case WM_INITDIALOG:
	{
		s_settingsDlg = dlg;
		darkTitleBar (dlg);
		for (int id : {IDOK, IDCANCEL, IDC_BROWSE, IDC_DARK, IDC_ADDPICK, IDC_REMOVE, IDC_ADDPATH, IDC_ITEMS, IDC_REPLACE})
			themeControl (GetDlgItem (dlg, id));
		for (int id : {IDC_HOST, IDC_PORT, IDC_SENDDIR, IDC_NEWPATH})
			themeControl (GetDlgItem (dlg, id), true);
		SendDlgItemMessageW (dlg, IDC_NEWPATH, EM_SETCUEBANNER, TRUE, (LPARAM)L"/path/to/folder or file");
		for (auto const &item : g_sendItems)
			addSendItem (dlg, item);
		updateSendSection (dlg);
		SetDlgItemTextW (dlg, IDC_SENDDIR, g_sendDir.c_str ());
		CheckDlgButton (dlg, IDC_DARK, g_dark ? BST_CHECKED : BST_UNCHECKED);
		CheckDlgButton (dlg, IDC_REPLACE, g_sendReplace ? BST_CHECKED : BST_UNCHECKED);
		std::lock_guard lk (g_cfgLock);
		SetDlgItemTextW (dlg, IDC_HOST, wide (g_host).c_str ());
		SetDlgItemInt (dlg, IDC_PORT, g_port, FALSE);
		return TRUE;
	}
	case WM_COMMAND:
		if (LOWORD (wp) == IDC_ADDPICK)
		{
			std::vector<wstring> picked;
			if (pickRemote (dlg, picked))
				for (auto const &p : picked)
					addSendItem (dlg, p);
			return TRUE;
		}
		if (LOWORD (wp) == IDC_ADDPATH)
		{
			wchar_t text[1024];
			GetDlgItemTextW (dlg, IDC_NEWPATH, text, 1024);
			wstring p = normalizeRemote (text);
			if (text[0] && p.size () > 1)
			{
				addSendItem (dlg, p);
				SetDlgItemTextW (dlg, IDC_NEWPATH, L"");
			}
			return TRUE;
		}
		if (LOWORD (wp) == IDC_REPLACE && IsDlgButtonChecked (dlg, IDC_REPLACE) == BST_CHECKED &&
		    MessageBoxW (dlg,
		        L"Each Send to PC will first empty the PC folder (its current files and folders go to the "
		        L"Recycle Bin), then copy the 3DS items into it.\n\nOnly use a folder dedicated to this. Turn it on?",
		        L"Replace PC folder contents",
		        MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) != IDYES)
		{
			CheckDlgButton (dlg, IDC_REPLACE, BST_UNCHECKED);
			return TRUE;
		}
		if (LOWORD (wp) == IDC_REMOVE)
		{
			HWND list = GetDlgItem (dlg, IDC_ITEMS);
			for (int i = (int)SendMessageW (list, LB_GETCOUNT, 0, 0) - 1; i >= 0; --i)
				if (SendMessageW (list, LB_GETSEL, i, 0) > 0)
					SendMessageW (list, LB_DELETESTRING, i, 0);
			return TRUE;
		}
		if (LOWORD (wp) == IDC_BROWSE)
		{
			wstring dir;
			if (pickFolder (dlg, L"Send to PC folder", dir))
				SetDlgItemTextW (dlg, IDC_SENDDIR, dir.c_str ());
			return TRUE;
		}
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
				MessageBoxW (dlg, L"Enter the IP address and port shown on the 3DS.", L"Cement", MB_ICONWARNING);
				return TRUE;
			}
			wchar_t dir[MAX_PATH];
			GetDlgItemTextW (dlg, IDC_SENDDIR, dir, MAX_PATH);
			bool dark    = IsDlgButtonChecked (dlg, IDC_DARK) == BST_CHECKED;
			bool replace = IsDlgButtonChecked (dlg, IDC_REPLACE) == BST_CHECKED;
			if (replace && dir[0] && unsafeReplaceTarget (dir))
			{
				MessageBoxW (dlg,
				    L"\"Replace PC folder contents\" can't be used with a drive or with your Desktop, Documents, "
				    L"Downloads, user, Windows or Program Files folder. Choose a dedicated folder.",
				    L"Cement",
				    MB_ICONWARNING);
				return TRUE;
			}
			g_sendReplace = replace;
			bool moved;
			{
				std::lock_guard lk (g_cfgLock);
				moved  = g_host != utf8 (h) || g_port != (int)port;
				g_host = utf8 (h);
				g_port = (int)port;
			}
			g_sendDir = dir;
			g_sendItems.clear ();
			HWND list = GetDlgItem (dlg, IDC_ITEMS);
			for (int i = 0, n = (int)SendMessageW (list, LB_GETCOUNT, 0, 0); i < n; ++i)
			{
				wstring item ((size_t)SendMessageW (list, LB_GETTEXTLEN, i, 0), 0);
				SendMessageW (list, LB_GETTEXT, i, (LPARAM)item.data ());
				g_sendItems.push_back (item);
			}
			if (dark != g_dark)
			{
				g_dark = dark;
				if (g_wnd)
					applyTheme ();
			}
			saveConfig ();
			EndDialog (dlg, moved ? IDOK : IDRETRY); // IDRETRY: saved, same server
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

static wstring g_shown;      // folder whose listing the list view shows (UI thread)
static wstring g_selectNext; // item to select in the next listing (a renamed file)

static void navigate (wstring const &path)
{
	{
		std::lock_guard lk (g_cwdLock);
		g_cwd = path;
	}
	SetWindowTextW (g_path, path.c_str ());
	if (path != g_shown)
	{
		// never leave the previous folder clickable under the new path: while the
		// 3DS is unreachable that turned /luma into /luma/luma/luma...
		ListView_DeleteAllItems (g_list);
		g_entries.clear ();
		g_shown.clear ();
	}
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
	if (!g_selectNext.empty ())
	{
		selected = {g_selectNext};
		g_selectNext.clear ();
	}

	g_entries = std::move (r->entries);
	g_shown   = cwd;
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
	if (MessageBoxW (g_wnd, q.c_str (), L"Cement", MB_YESNO | MB_ICONWARNING) != IDYES)
		return;
	// on the transfer connection: tracked with the progress bar, cancellable, resumable
	g_cancel     = false;
	auto job     = std::make_shared<Job> ();
	job->type    = Job::Delete;
	job->path    = currentDir ();
	job->remotes = sel;
	{
		std::lock_guard lk (g_prog.lock);
		g_prog.active = true;
	}
	g_xfer.push (job);
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
	return wstring (tmp) + L"Cement";
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
	wstring dir;
	if (!sel.empty () && pickFolder (g_wnd, L"Download to", dir))
		startDownload (sel, dir, nullptr);
}

// one click: copy every 3DS item listed in Settings directly into the PC
// folder (/openmw/logs -> <folder>\logs)
static void sendToPc ()
{
	if (g_sendItems.empty ())
	{
		setStatus (1, L"Add the 3DS folders/files to send in Settings first");
		return;
	}
	if (g_sendDir.empty ())
	{
		wstring dir;
		if (!pickFolder (g_wnd, L"Choose the Send to PC folder (saved in Settings)", dir))
			return;
		g_sendDir = dir;
		saveConfig ();
	}
	SHCreateDirectoryExW (nullptr, g_sendDir.c_str (), nullptr);
	if (!(GetFileAttributesW (g_sendDir.c_str ()) & FILE_ATTRIBUTE_DIRECTORY) ||
	    GetFileAttributesW (g_sendDir.c_str ()) == INVALID_FILE_ATTRIBUTES)
	{
		setStatus (1, L"Can't use the Send to PC folder: " + g_sendDir);
		return;
	}
	g_cancel        = false;
	auto job        = std::make_shared<Job> ();
	job->type       = Job::SendToPc;
	job->path2      = g_sendDir;
	job->sendPaths  = g_sendItems;
	job->replace    = g_sendReplace;
	{
		std::lock_guard lk (g_prog.lock);
		g_prog.active = true;
	}
	g_xfer.push (job);
}

// ---------------------------------------------------------------------------
// 3DS picker (Settings > Add from 3DS...)

static LRESULT CALLBACK listProc (HWND, UINT, WPARAM, LPARAM, UINT_PTR, DWORD_PTR);

static struct
{
	wstring cwd;
	std::vector<Entry> entries;
	std::vector<wstring> *out;
} s_pick;

static void pickNavigate (HWND dlg, wstring const &path)
{
	s_pick.cwd = path;
	SetDlgItemTextW (dlg, IDC_PICKPATH, path.c_str ());
	ListView_DeleteAllItems (GetDlgItem (dlg, IDC_PICKLIST));
	auto job    = std::make_shared<Job> ();
	job->type   = Job::List;
	job->path   = path;
	job->notify = dlg;
	g_browse.push (job);
}

static INT_PTR CALLBACK pickDlgProc (HWND dlg, UINT msg, WPARAM wp, LPARAM lp)
{
	HWND list = GetDlgItem (dlg, IDC_PICKLIST);
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
		for (int id : {IDOK, IDCANCEL, IDC_PICKUP})
			themeControl (GetDlgItem (dlg, id));
		themeControl (GetDlgItem (dlg, IDC_PICKPATH), true);
		auto t = theme ();
		ListView_SetExtendedListViewStyle (list, LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER);
		SetWindowTheme (list, g_dark ? L"DarkMode_Explorer" : L"Explorer", nullptr);
		SetWindowTheme (ListView_GetHeader (list), g_dark ? L"DarkMode_ItemsView" : nullptr, nullptr);
		SetWindowSubclass (list, listProc, 0, 0);
		ListView_SetBkColor (list, t.list);
		ListView_SetTextBkColor (list, t.list);
		ListView_SetTextColor (list, t.text);
		SHFILEINFOW fi{};
		auto images = (HIMAGELIST)SHGetFileInfoW (L"folder", FILE_ATTRIBUTE_DIRECTORY, &fi, sizeof (fi), SHGFI_USEFILEATTRIBUTES | SHGFI_SYSICONINDEX | SHGFI_SMALLICON);
		ListView_SetImageList (list, images, LVSIL_SMALL);
		RECT rc;
		GetClientRect (list, &rc);
		LVCOLUMNW col{LVCF_TEXT | LVCF_WIDTH, 0, rc.right - GetSystemMetrics (SM_CXVSCROLL) - 4, (LPWSTR)L"Name"};
		ListView_InsertColumn (list, 0, &col);
		pickNavigate (dlg, L"/");
		return TRUE;
	}
	case WM_APP_LIST:
	{
		std::unique_ptr<ListResult> r ((ListResult *)lp);
		if (r->path != s_pick.cwd)
			return TRUE;
		s_pick.entries = r->entries;
		std::sort (s_pick.entries.begin (), s_pick.entries.end (), [] (Entry const &a, Entry const &b) {
			return a.dir != b.dir ? a.dir : _wcsicmp (a.name.c_str (), b.name.c_str ()) < 0;
		});
		ListView_DeleteAllItems (list);
		int row = 0;
		if (s_pick.cwd != L"/")
		{
			LVITEMW it{LVIF_TEXT | LVIF_IMAGE | LVIF_PARAM, row++};
			it.pszText = (LPWSTR)L"..";
			it.iImage  = iconFor (L"", true);
			it.lParam  = -1;
			ListView_InsertItem (list, &it);
		}
		for (size_t i = 0; i < s_pick.entries.size (); ++i)
		{
			LVITEMW it{LVIF_TEXT | LVIF_IMAGE | LVIF_PARAM, row++};
			it.pszText = (LPWSTR)s_pick.entries[i].name.c_str ();
			it.iImage  = iconFor (s_pick.entries[i].name, s_pick.entries[i].dir);
			it.lParam  = (LPARAM)i;
			ListView_InsertItem (list, &it);
		}
		return TRUE;
	}
	case WM_NOTIFY:
		if (((NMHDR *)lp)->idFrom == IDC_PICKLIST && ((NMHDR *)lp)->code == NM_DBLCLK)
		{
			LVITEMW it{LVIF_PARAM, ((NMITEMACTIVATE *)lp)->iItem};
			if (it.iItem < 0 || !ListView_GetItem (list, &it))
				return TRUE;
			if (it.lParam == -1)
				pickNavigate (dlg, parentRemote (s_pick.cwd));
			else if (s_pick.entries[it.lParam].dir)
				pickNavigate (dlg, joinRemote (s_pick.cwd, s_pick.entries[it.lParam].name));
			return TRUE;
		}
		break;
	case WM_COMMAND:
		switch (LOWORD (wp))
		{
		case IDC_PICKUP:
			if (s_pick.cwd != L"/")
				pickNavigate (dlg, parentRemote (s_pick.cwd));
			return TRUE;
		case IDOK:
		{
			for (int i = -1; (i = ListView_GetNextItem (list, i, LVNI_SELECTED)) >= 0;)
			{
				LVITEMW it{LVIF_PARAM, i};
				if (ListView_GetItem (list, &it) && it.lParam >= 0)
					s_pick.out->push_back (joinRemote (s_pick.cwd, s_pick.entries[it.lParam].name));
			}
			if (s_pick.out->empty ()) // nothing selected: take the folder being shown
				s_pick.out->push_back (s_pick.cwd);
			EndDialog (dlg, IDOK);
			return TRUE;
		}
		case IDCANCEL:
			EndDialog (dlg, IDCANCEL);
			return TRUE;
		}
		break;
	}
	return FALSE;
}

static bool pickRemote (HWND owner, std::vector<wstring> &out)
{
	s_pick = {};
	s_pick.out = &out;
	return DialogBoxW (g_inst, MAKEINTRESOURCEW (IDD_PICK), owner, pickDlgProc) == IDOK && !out.empty ();
}

static bool browseConnected () { return g_browse.state == Worker::Connected; }

// ---------------------------------------------------------------------------
// main window

enum
{
	ID_LIST = 1,
	ID_UP,
	ID_REFRESH,
	ID_MKDIR,
	ID_RECONNECT,
	ID_SETTINGS,
	ID_CANCEL,
	ID_SEND,
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
	for (HWND h : {g_list, g_path, g_btnUp, g_btnRefresh, g_btnMkdir, g_btnRename, g_btnSend, g_btnReconnect, g_btnSettings, g_btnCancel})
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
	for (HWND h : {g_btnUp, g_btnRefresh, g_btnMkdir, g_btnRename, g_btnSend, g_btnReconnect, g_btnSettings, g_btnCancel})
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
	RECT sep{S (470), sr.top + S (4), S (470) + 1, sr.bottom - S (4)};
	FillRect (dc, &sep, border);
	DeleteObject (panel);
	DeleteObject (border);

	HFONT old = (HFONT)SelectObject (dc, g_font);
	SetBkMode (dc, TRANSPARENT);
	SetTextColor (dc, t.text);
	UINT fmt   = DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX;
	RECT part0 = {S (8), sr.top + 1, S (462), sr.bottom};
	DrawTextW (dc, g_statusText[0].c_str (), -1, &part0, fmt);
	SetTextColor (dc, t.dim);
	RECT part1 = {S (478), sr.top + 1, rc.right - S (8), sr.bottom};
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
	place (g_btnRename, S (70));
	int right = rc.right - pad - S (96) - S (4) - S (110) - S (4) - S (80);
	MoveWindow (g_path, x, y + S (2), std::max (S (40), right - x - S (4)), h - S (4), TRUE);
	x = right;
	place (g_btnSend, S (96));
	place (g_btnReconnect, S (110));
	place (g_btnSettings, S (80));

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
	string version;
	{
		std::lock_guard lk (g_cfgLock);
		version = g_serverVersion;
	}
	wstring addr = wide (host) + L":" + std::to_wstring (port);
	wstring text = L"Waiting for 3DS at " + addr + L"...";
	if (g_browse.state == Worker::Connected)
	{
		text = L"Connected to " + addr;
		int mine = buildNumber (APP_VERSION), theirs = buildNumber (version);
		if (version.empty ())
			text += L"  -  3DS app outdated: install the new CIA";
		else
		{
			text += L"  -  3DS app v" + wide (version);
			if (mine >= 0 && theirs < mine) // no build number = from before versioned builds
				text += L" (older than this PC app: install the new CIA)";
		}
	}
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
	bool up, del;
	{
		std::lock_guard lk (g_prog.lock);
		del    = g_prog.del;
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
	else if (count && del)
		text = L"Deleting " + std::to_wstring (index) + L"/" + std::to_wstring (count) + L": " + name;
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
	case ID_SETTINGS:
		if (DialogBoxW (g_inst, MAKEINTRESOURCEW (IDD_SETTINGS), g_wnd, settingsDlgProc) == IDOK)
		{
			updateConnState ();
			navigate (L"/");
			g_browse.reconnect ();
			g_xfer.reconnect ();
		}
		break;
	case ID_SEND:
		sendToPc ();
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
		if (row < 0 || isParentRow (row) || ListView_GetSelectedCount (g_list) != 1)
		{
			setStatus (1, L"Select one file or folder to rename");
			break;
		}
		SetFocus (g_list);
		ListView_EditLabel (g_list, row);
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
		case 'S':
			if (GetKeyState (VK_CONTROL) < 0)
				sendToPc ();
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
		g_selectNext = name;
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
		g_btnRename    = makeButton (L"Rename", ID_RENAME);
		g_btnSend      = makeButton (L"Send to PC", ID_SEND);
		g_btnReconnect = makeButton (L"Force reconnect", ID_RECONNECT);
		g_btnSettings  = makeButton (L"Settings...", ID_SETTINGS);
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
		((MINMAXINFO *)lp)->ptMinTrackSize = {S (740), S (300)};
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
		if (s_settingsDlg)
			PostMessageW (s_settingsDlg, WM_APP_STATE, 0, 0);
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
	if (!configured && DialogBoxW (inst, MAKEINTRESOURCEW (IDD_SETTINGS), nullptr, settingsDlgProc) == IDCANCEL)
		return 0;

	WNDCLASSEXW wc{sizeof (wc)};
	wc.lpfnWndProc   = wndProc;
	wc.hInstance     = inst;
	wc.hCursor       = LoadCursor (nullptr, IDC_ARROW);
	wc.hbrBackground = nullptr; // painted in WM_PAINT
	wc.lpszClassName = L"Cement";
	wc.hIcon         = LoadIconW (inst, MAKEINTRESOURCEW (IDI_APP));
	RegisterClassExW (&wc);

	wstring title = L"Cement v" + wide (APP_VERSION);
	HWND wnd      = CreateWindowExW (0, L"Cement", title.c_str (), WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT, 0, 0, nullptr, nullptr, inst, nullptr);
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
