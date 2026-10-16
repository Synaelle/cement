// Headless test of the client's FTP engine and workers against the PC build
// of the server. usage: test_client.exe <server_pc.exe>
#define wWinMain cement_wWinMain
#include "../client/src/main.cpp"
#undef wWinMain

#include <cstdio>
#include <fstream>
#include <random>

static int s_failures;
static std::vector<ListResult> s_lists;
static wstring s_root, s_flag, s_exe;
static PROCESS_INFORMATION s_srv;

static void check (bool ok, char const *what)
{
	printf ("  %s %s\n", ok ? "ok  " : "FAIL", what);
	fflush (stdout);
	s_failures += !ok;
}

static LRESULT CALLBACK testProc (HWND w, UINT m, WPARAM wp, LPARAM lp)
{
	if (m == WM_APP_LIST)
	{
		std::unique_ptr<ListResult> r ((ListResult *)lp);
		s_lists.push_back (*r);
		return 0;
	}
	if (m == WM_APP_MSG)
	{
		std::unique_ptr<wstring> t ((wstring *)lp);
		printf ("       [status] %s\n", utf8 (*t).c_str ());
		return 0;
	}
	return DefWindowProcW (w, m, wp, lp);
}

// pump messages until cond() is true or timeout; returns cond()
template <class F>
static bool waitFor (F cond, int ms)
{
	auto end = GetTickCount64 () + ms;
	while (GetTickCount64 () < end)
	{
		MSG msg;
		while (PeekMessageW (&msg, nullptr, 0, 0, PM_REMOVE))
			DispatchMessageW (&msg);
		if (cond ())
			return true;
		Sleep (10);
	}
	return cond ();
}

static void startServer ()
{
	SetEnvironmentVariableW (L"CEMENT_ROOT", s_root.c_str ());
	SetEnvironmentVariableW (L"CEMENT_SLEEPFLAG", s_flag.c_str ());
	STARTUPINFOW si{sizeof (si)};
	wstring cmd = L"\"" + s_exe + L"\"";
	CreateProcessW (nullptr, cmd.data (), nullptr, nullptr, FALSE, 0, nullptr, nullptr, &si, &s_srv);
}

static void stopServer ()
{
	TerminateProcess (s_srv.hProcess, 0);
	WaitForSingleObject (s_srv.hProcess, INFINITE);
	CloseHandle (s_srv.hProcess);
	CloseHandle (s_srv.hThread);
}

static std::string readFile (wstring const &p)
{
	std::ifstream f (p.c_str (), std::ios::binary);
	return std::string ((std::istreambuf_iterator<char> (f)), std::istreambuf_iterator<char> ());
}

static void writeRandom (wstring const &p, size_t size, unsigned seed)
{
	std::mt19937 rng (seed);
	std::string data (size, 0);
	for (auto &c : data)
		c = (char)rng ();
	std::ofstream (p.c_str (), std::ios::binary).write (data.data (), data.size ());
}

static uint64_t progDone ()
{
	std::lock_guard lk (g_prog.lock);
	return g_prog.done;
}

static JobPtr upload (std::vector<wstring> files, wstring dir)
{
	g_cancel    = false;
	auto job    = std::make_shared<Job> ();
	job->type   = Job::Upload;
	job->path   = dir;
	job->locals = std::move (files);
	job->doneEvent = CreateEventW (nullptr, TRUE, FALSE, nullptr);
	g_xfer.push (job);
	return job;
}

static bool finished (JobPtr const &j) { return WaitForSingleObject (j->doneEvent, 0) == WAIT_OBJECT_0; }

int main (int argc, char **argv)
{
	if (argc < 2)
		return 2;
	s_exe = wide (argv[1]);
	wchar_t tmp[MAX_PATH];
	GetTempPathW (MAX_PATH, tmp);
	wstring base = wstring (tmp) + L"cement-client-test-" + std::to_wstring (GetTickCount64 ());
	s_root       = base + L"-sd";
	s_flag       = base + L".sleep";
	wstring local = base + L"-local", back = base + L"-back";
	CreateDirectoryW (s_root.c_str (), nullptr);
	CreateDirectoryW (local.c_str (), nullptr);
	CreateDirectoryW (back.c_str (), nullptr);

	WSADATA wsa;
	WSAStartup (MAKEWORD (2, 2), &wsa);
	WNDCLASSW wc{};
	wc.lpfnWndProc   = testProc;
	wc.lpszClassName = L"cement-test";
	RegisterClassW (&wc);
	g_wnd  = CreateWindowW (L"cement-test", L"", 0, 0, 0, 0, 0, HWND_MESSAGE, nullptr, nullptr, nullptr);
	g_host = "127.0.0.1";
	g_port = 5000;

	printf ("waits for the server, then connects\n");
	g_browse.start ();
	g_xfer.start ();
	waitFor ([] { return false; }, 2500);
	check (g_browse.state == Worker::Waiting, "waiting while the server is down");
	startServer ();
	check (waitFor ([] { return g_browse.state == Worker::Connected && g_xfer.state == Worker::Connected; }, 8000),
	    "both connections up once the server starts");
	check (waitFor ([] { return !s_lists.empty () && s_lists.back ().path == L"/" && s_lists.back ().ok; }, 3000),
	    "folder refreshed automatically after connecting");

	printf ("upload a folder tree\n");
	CreateDirectoryW ((local + L"\\tree").c_str (), nullptr);
	CreateDirectoryW ((local + L"\\tree\\sub dir").c_str (), nullptr);
	CreateDirectoryW ((local + L"\\tree\\empty").c_str (), nullptr);
	writeRandom (local + L"\\tree\\a.bin", 3 << 20, 1);
	writeRandom (local + L"\\tree\\sub dir\\b été.bin", 777777, 2);
	writeRandom (local + L"\\tree\\zero.bin", 0, 3);
	writeRandom (local + L"\\single.bin", 1 << 20, 4);
	auto job = upload ({local + L"\\tree", local + L"\\single.bin"}, L"/");
	check (waitFor ([&] { return finished (job); }, 20000) && job->ok, "upload job finished");
	check (readFile (s_root + L"\\tree\\a.bin") == readFile (local + L"\\tree\\a.bin"), "tree/a.bin intact");
	check (readFile (s_root + L"\\tree\\sub dir\\b été.bin") == readFile (local + L"\\tree\\sub dir\\b été.bin"),
	    "nested unicode file intact");
	check (GetFileAttributesW ((s_root + L"\\tree\\empty").c_str ()) & FILE_ATTRIBUTE_DIRECTORY, "empty folder created");
	check (GetFileAttributesW ((s_root + L"\\tree\\zero.bin").c_str ()) != INVALID_FILE_ATTRIBUTES, "empty file created");
	check (readFile (s_root + L"\\single.bin") == readFile (local + L"\\single.bin"), "single file intact");
	check (waitFor ([] { return !s_lists.empty () && s_lists.back ().entries.size () >= 2; }, 3000),
	    "listing refreshed after upload");

	printf ("lid closes mid-upload, transfer resumes after wake\n");
	writeRandom (local + L"\\big.bin", 400 << 20, 5);
	job = upload ({local + L"\\big.bin"}, L"/");
	waitFor ([] { return progDone () > (60u << 20); }, 20000);
	CloseHandle (CreateFileW (s_flag.c_str (), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS, 0, nullptr));
	check (waitFor ([] { return g_xfer.state == Worker::Waiting; }, 15000), "client notices the 3DS went to sleep");
	check (!finished (job), "job kept while waiting");
	waitFor ([] { return false; }, 2500);
	DeleteFileW (s_flag.c_str ());
	check (waitFor ([&] { return finished (job); }, 60000) && job->ok, "upload resumed and finished after wake");
	check (readFile (s_root + L"\\big.bin") == readFile (local + L"\\big.bin"), "resumed 400 MB file intact");

	printf ("force reconnect mid-upload\n");
	DeleteFileW ((s_root + L"\\big.bin").c_str ());
	job = upload ({local + L"\\big.bin"}, L"/");
	waitFor ([] { return progDone () > (100u << 20); }, 20000);
	g_xfer.reconnect ();
	g_browse.reconnect ();
	check (waitFor ([&] { return finished (job); }, 60000) && job->ok, "upload finished after forced reconnect");
	check (readFile (s_root + L"\\big.bin") == readFile (local + L"\\big.bin"), "file intact after forced reconnect");

	printf ("cancel removes the partial remote file\n");
	DeleteFileW ((s_root + L"\\big.bin").c_str ());
	job = upload ({local + L"\\big.bin"}, L"/");
	waitFor ([] { return progDone () > (50u << 20); }, 20000);
	g_cancel = true;
	g_xfer.clearPending ();
	check (waitFor ([&] { return finished (job); }, 15000) && !job->ok, "upload cancelled");
	check (waitFor ([&] { return GetFileAttributesW ((s_root + L"\\big.bin").c_str ()) == INVALID_FILE_ATTRIBUTES; }, 5000),
	    "partial file deleted on the 3DS");
	check (waitFor ([] { return g_xfer.state == Worker::Connected; }, 5000), "transfer connection still usable");

	printf ("download a folder (drag-out path)\n");
	g_cancel = false;
	HANDLE ev = CreateEventW (nullptr, TRUE, FALSE, nullptr);
	{
		std::lock_guard lk (g_cwdLock);
		g_cwd = L"/";
	}
	startDownload ({Entry{L"tree", true, 0}}, back, ev);
	check (waitFor ([&] { return WaitForSingleObject (ev, 0) == WAIT_OBJECT_0; }, 20000), "download job finished");
	check (readFile (back + L"\\tree\\sub dir\\b été.bin") == readFile (local + L"\\tree\\sub dir\\b été.bin"),
	    "recursive download intact");
	check (GetFileAttributesW ((back + L"\\tree\\empty").c_str ()) & FILE_ATTRIBUTE_DIRECTORY, "empty folder downloaded");

	printf ("send to PC (paths from settings, each item directly in the PC folder)\n");
	{
		wstring dest = back + L"\\sendto";
		SHCreateDirectoryExW (nullptr, (s_root + L"\\3ds\\Checkpoint\\saves\\Game é").c_str (), nullptr);
		writeRandom (s_root + L"\\3ds\\Checkpoint\\saves\\Game é\\main", 123456, 7);
		writeRandom (s_root + L"\\boot.firm", 5000, 8);
		g_cancel       = false;
		auto job       = std::make_shared<Job> ();
		job->type      = Job::SendToPc;
		job->path2     = dest;
		job->sendPaths = {L"/3ds/Checkpoint/saves", L"/boot.firm", L"/does/not/exist"};
		job->doneEvent = CreateEventW (nullptr, TRUE, FALSE, nullptr);
		SHCreateDirectoryExW (nullptr, dest.c_str (), nullptr);
		g_xfer.push (job);
		check (waitFor ([&] { return finished (job); }, 20000) && job->ok, "send to PC job finished");
		check (readFile (dest + L"\\saves\\Game é\\main") == readFile (s_root + L"\\3ds\\Checkpoint\\saves\\Game é\\main"),
		    "folder copied directly into the PC folder");
		check (GetFileAttributesW ((dest + L"\\3ds").c_str ()) == INVALID_FILE_ATTRIBUTES, "no parent folders created");
		check (readFile (dest + L"\\boot.firm") == readFile (s_root + L"\\boot.firm"), "single file copied");
		check (job->missing == 1, "missing path skipped and counted");
		check (escapeIni (L"/3ds/café 100%") == L"/3ds/caf%C3%A9 100%25" &&
		           unescapeIni (escapeIni (L"/3ds/café 100%")) == L"/3ds/café 100%",
		    "config escaping round-trips");
		check (normalizeRemote (L" 3ds\\\\Checkpoint//saves/ ") == L"/3ds/Checkpoint/saves", "typed paths normalized");
	}

	printf ("send to PC with \"replace PC folder contents\"\n");
	{
		wstring dest = back + L"\\sendto";
		writeRandom (dest + L"\\old file.txt", 10, 9);
		SHCreateDirectoryExW (nullptr, (dest + L"\\old dir\\deep").c_str (), nullptr);
		g_cancel       = false;
		auto job       = std::make_shared<Job> ();
		job->type      = Job::SendToPc;
		job->path2     = dest;
		job->sendPaths = {L"/boot.firm"};
		job->replace   = true;
		job->recycle   = false; // keep the test out of the Recycle Bin
		job->doneEvent = CreateEventW (nullptr, TRUE, FALSE, nullptr);
		g_xfer.push (job);
		check (waitFor ([&] { return finished (job); }, 20000) && job->ok, "replace job finished");
		check (GetFileAttributesW ((dest + L"\\old file.txt").c_str ()) == INVALID_FILE_ATTRIBUTES &&
		           GetFileAttributesW ((dest + L"\\old dir").c_str ()) == INVALID_FILE_ATTRIBUTES &&
		           GetFileAttributesW ((dest + L"\\3ds").c_str ()) == INVALID_FILE_ATTRIBUTES,
		    "old PC folder contents removed");
		check (readFile (dest + L"\\boot.firm") == readFile (s_root + L"\\boot.firm"), "3DS content copied after emptying");
		check (GetFileAttributesW (dest.c_str ()) & FILE_ATTRIBUTE_DIRECTORY, "the PC folder itself is kept");

		PWSTR docs = nullptr;
		SHGetKnownFolderPath (FOLDERID_Documents, 0, nullptr, &docs);
		check (unsafeReplaceTarget (L"C:\\") && unsafeReplaceTarget (docs) && unsafeReplaceTarget (wstring (docs) + L"\\") &&
		           !unsafeReplaceTarget (wstring (docs) + L"\\3DS backup"),
		    "drives and personal folders refused, subfolders allowed");
		CoTaskMemFree (docs);

		job            = std::make_shared<Job> ();
		job->type      = Job::SendToPc;
		job->path2     = L"C:\\";
		job->sendPaths = {L"/boot.firm"};
		job->replace   = true;
		job->doneEvent = CreateEventW (nullptr, TRUE, FALSE, nullptr);
		g_xfer.push (job);
		check (waitFor ([&] { return finished (job); }, 10000) && !job->ok, "worker refuses to empty a drive root");
	}

	printf ("explorer operations\n");
	auto run = [] (JobPtr j) { g_browse.push (j); };
	auto mk  = [] (Job::Type t, wstring a, wstring b = L"") {
		auto j   = std::make_shared<Job> ();
		j->type  = t;
		j->path  = a;
		j->path2 = b;
		return j;
	};
	run (mk (Job::Mkdir, L"/newdir"));
	check (waitFor ([&] { return GetFileAttributesW ((s_root + L"\\newdir").c_str ()) != INVALID_FILE_ATTRIBUTES; }, 5000), "new folder");
	run (mk (Job::Rename, L"/single.bin", L"/newdir/moved.bin"));
	check (waitFor ([&] { return GetFileAttributesW ((s_root + L"\\newdir\\moved.bin").c_str ()) != INVALID_FILE_ATTRIBUTES; }, 5000), "rename/move");
	auto del     = mk (Job::Delete, L"/");
	del->remotes = {Entry{L"tree", true, 0}, Entry{L"newdir", true, 0}, Entry{L"3ds", true, 0}, Entry{L"boot.firm", false, 0}};
	run (del);
	check (waitFor ([&] { return GetFileAttributesW ((s_root + L"\\tree").c_str ()) == INVALID_FILE_ATTRIBUTES &&
		                         GetFileAttributesW ((s_root + L"\\newdir").c_str ()) == INVALID_FILE_ATTRIBUTES; }, 5000),
	    "delete folders recursively");
	s_lists.clear ();
	navigate (L"/");
	check (waitFor ([] { return !s_lists.empty (); }, 5000) && s_lists.back ().entries.empty (), "root listing empty after delete");

	printf ("tracked deletion of many files, surviving a forced reconnect\n");
	{
		for (int d = 0; d < 8; ++d)
		{
			wstring dir = s_root + L"\\many\\d" + std::to_wstring (d) + L"\\sub";
			SHCreateDirectoryExW (nullptr, dir.c_str (), nullptr);
			for (int f = 0; f < 60; ++f)
				writeRandom (dir + L"\\f" + std::to_wstring (f) + L".bin", 100, f);
		}
		g_cancel       = false;
		auto job       = std::make_shared<Job> ();
		job->type      = Job::Delete;
		job->path      = L"/";
		job->remotes   = {Entry{L"many", true, 0}};
		job->doneEvent = CreateEventW (nullptr, TRUE, FALSE, nullptr);
		g_xfer.push (job);
		bool sawProgress = false;
		waitFor ([&] {
			std::lock_guard lk (g_prog.lock);
			sawProgress |= g_prog.del && g_prog.fileCount == 8 * 60 + 8 * 2 + 1;
			return g_prog.del && g_prog.done > 120;
		}, 20000);
		g_xfer.reconnect ();
		check (sawProgress, "deletion reports progress per item (497 items)");
		check (waitFor ([&] { return finished (job); }, 60000) && job->ok, "deletion finished without errors after a reconnect");
		check (GetFileAttributesW ((s_root + L"\\many").c_str ()) == INVALID_FILE_ATTRIBUTES, "every file and folder deleted");
	}

	printf ("opening folders while the 3DS does not answer (no ghost paths)\n");
	{
		INITCOMMONCONTROLSEX icc{sizeof (icc), ICC_LISTVIEW_CLASSES};
		InitCommonControlsEx (&icc);
		HWND host = CreateWindowW (L"STATIC", L"", WS_POPUP, 0, 0, 300, 300, nullptr, nullptr, nullptr, nullptr);
		g_list    = CreateWindowW (WC_LISTVIEWW, L"", WS_CHILD | LVS_REPORT, 0, 0, 300, 300, host, nullptr, nullptr, nullptr);
		LVCOLUMNW col{LVCF_TEXT | LVCF_WIDTH, 0, 200, (LPWSTR)L"Name"};
		ListView_InsertColumn (g_list, 0, &col);
		ListView_InsertColumn (g_list, 1, &col);
		navigate (L"/");
		ListResult root{L"/", {Entry{L"luma", true, 0}}, true};
		showList (&root);
		check (ListView_GetItemCount (g_list) == 1, "root listing shown");
		openRow (0); // -> /luma; its listing has not arrived yet
		check (ListView_GetItemCount (g_list) == 0, "old listing cleared when opening a folder");
		openRow (0); // clicking again where "luma" was
		openRow (0);
		check (currentDir () == L"/luma", "path stays /luma, not /luma/luma/luma");
		DestroyWindow (host);
		g_list = nullptr;
	}

	g_cancel = true;
	g_browse.stop ();
	g_xfer.stop ();
	stopServer ();
	for (auto const &d : {s_root, local, back})
		deleteTree (d);
	printf ("\n%d failure(s)\n", s_failures);
	return s_failures ? 1 : 0;
}
