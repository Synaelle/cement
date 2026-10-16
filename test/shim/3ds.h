// PC test shim for the subset of libctru used by server/source/main.c.
// The SD card is a folder (env CEMENT_ROOT); creating the file named by
// env CEMENT_SLEEPFLAG simulates closing the lid until it is deleted.
#pragma once
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <windows.h>
#include <shellapi.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;
typedef int16_t s16;
typedef int32_t s32;
typedef int64_t s64;
typedef s32 Result;
typedef u32 Handle;
typedef u64 FS_Archive;

#define R_FAILED(r) ((r) < 0)
#define R_SUCCEEDED(r) ((r) >= 0)
#define BIT(n) (1u << (n))
#ifndef U64_MAX
#define U64_MAX UINT64_MAX
#endif
#define memalign(a, s) malloc (s)
#define strtok_r strtok_s

// ---- fs
typedef enum { PATH_INVALID, PATH_EMPTY, PATH_BINARY, PATH_ASCII, PATH_UTF16 } FS_PathType;
typedef enum { ARCHIVE_SDMC = 9 } FS_ArchiveID;
enum { FS_OPEN_READ = BIT (0), FS_OPEN_WRITE = BIT (1), FS_OPEN_CREATE = BIT (2) };
enum { FS_ATTRIBUTE_DIRECTORY = BIT (0) };
typedef struct { FS_PathType type; u32 size; void const *data; } FS_Path;
typedef struct
{
	u16 name[0x106];
	char shortName[0x0A];
	char shortExt[0x04];
	u8 valid;
	u8 reserved;
	u32 attributes;
	u64 fileSize;
} FS_DirectoryEntry;

typedef struct
{
	HANDLE file;
	HANDLE find;
	WIN32_FIND_DATAW fd;
	bool first, used;
} ShimHandle;
static ShimHandle s_h[256];

static wchar_t const *shimRoot (void)
{
	static wchar_t root[MAX_PATH];
	if (!root[0] && !GetEnvironmentVariableW (L"CEMENT_ROOT", root, MAX_PATH))
		wcscpy (root, L".");
	return root;
}

static void shimPath (FS_Path p, wchar_t *out)
{
	wcscpy (out, shimRoot ());
	size_t n = wcslen (out);
	for (u16 const *s = (u16 const *)p.data; p.type == PATH_UTF16 && *s; ++s)
		out[n++] = *s == '/' ? L'\\' : *s;
	out[n] = 0;
}

static Handle shimAlloc (void)
{
	for (u32 i = 1; i < 256; ++i)
		if (!s_h[i].used)
		{
			memset (&s_h[i], 0, sizeof (s_h[i]));
			s_h[i].used = true;
			return i;
		}
	return 0;
}

static inline FS_Path fsMakePath (FS_PathType type, void const *path)
{
	FS_Path p = {type, 0, path};
	if (type == PATH_UTF16)
	{
		u16 const *s = (u16 const *)path;
		while (*s++)
			p.size++;
		p.size = (p.size + 1) * 2;
	}
	return p;
}

static inline Result FSUSER_OpenArchive (FS_Archive *a, FS_ArchiveID id, FS_Path p)
{
	(void)id, (void)p;
	*a = 1;
	return 0;
}
static inline Result FSUSER_CloseArchive (FS_Archive a) { (void)a; return 0; }

static inline Result FSUSER_OpenFile (Handle *out, FS_Archive a, FS_Path p, u32 flags, u32 attr)
{
	(void)a, (void)attr;
	wchar_t path[2048];
	shimPath (p, path);
	DWORD attrs = GetFileAttributesW (path);
	if (attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_DIRECTORY))
		return -1;
	HANDLE f = CreateFileW (path,
	    (flags & FS_OPEN_WRITE) ? GENERIC_READ | GENERIC_WRITE : GENERIC_READ,
	    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
	    NULL,
	    (flags & FS_OPEN_CREATE) ? OPEN_ALWAYS : OPEN_EXISTING,
	    0,
	    NULL);
	if (f == INVALID_HANDLE_VALUE)
		return -1;
	*out           = shimAlloc ();
	s_h[*out].file = f;
	return 0;
}

static inline Result FSFILE_Read (Handle h, u32 *n, u64 off, void *buf, u32 size)
{
	OVERLAPPED o = {0};
	o.Offset     = (DWORD)off;
	o.OffsetHigh = (DWORD)(off >> 32);
	DWORD got    = 0;
	if (!ReadFile (s_h[h].file, buf, size, &got, &o) && GetLastError () != ERROR_HANDLE_EOF)
		return -1;
	*n = got;
	return 0;
}

static inline Result FSFILE_Write (Handle h, u32 *n, u64 off, void const *buf, u32 size, u32 flags)
{
	(void)flags;
	OVERLAPPED o = {0};
	o.Offset     = (DWORD)off;
	o.OffsetHigh = (DWORD)(off >> 32);
	DWORD put    = 0;
	if (!WriteFile (s_h[h].file, buf, size, &put, &o))
		return -1;
	*n = put;
	return 0;
}

static inline Result FSFILE_GetSize (Handle h, u64 *size)
{
	LARGE_INTEGER li;
	if (!GetFileSizeEx (s_h[h].file, &li))
		return -1;
	*size = (u64)li.QuadPart;
	return 0;
}

static inline Result FSFILE_SetSize (Handle h, u64 size)
{
	LARGE_INTEGER li;
	li.QuadPart = (LONGLONG)size;
	return SetFilePointerEx (s_h[h].file, li, NULL, FILE_BEGIN) && SetEndOfFile (s_h[h].file) ? 0 : -1;
}

static inline Result FSFILE_Flush (Handle h) { return FlushFileBuffers (s_h[h].file) ? 0 : -1; }
static inline Result FSFILE_Close (Handle h)
{
	CloseHandle (s_h[h].file);
	s_h[h].used = false;
	return 0;
}

static inline Result FSUSER_OpenDirectory (Handle *out, FS_Archive a, FS_Path p)
{
	(void)a;
	wchar_t path[2048];
	shimPath (p, path);
	DWORD attrs = GetFileAttributesW (path);
	if (attrs == INVALID_FILE_ATTRIBUTES || !(attrs & FILE_ATTRIBUTE_DIRECTORY))
		return -1;
	wcscat (path, L"\\*");
	Handle h = shimAlloc ();
	s_h[h].find  = FindFirstFileW (path, &s_h[h].fd);
	s_h[h].first = true;
	*out         = h;
	return 0;
}

static inline Result FSDIR_Read (Handle h, u32 *count, u32 max, FS_DirectoryEntry *ents)
{
	ShimHandle *d = &s_h[h];
	*count        = 0;
	while (*count < max && d->find != INVALID_HANDLE_VALUE)
	{
		if (!d->first && !FindNextFileW (d->find, &d->fd))
			break;
		d->first = false;
		if (!wcscmp (d->fd.cFileName, L".") || !wcscmp (d->fd.cFileName, L".."))
			continue;
		FS_DirectoryEntry *e = &ents[(*count)++];
		memset (e, 0, sizeof (*e));
		memcpy (e->name, d->fd.cFileName, (wcslen (d->fd.cFileName) + 1) * 2);
		e->attributes = (d->fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) ? FS_ATTRIBUTE_DIRECTORY : 0;
		e->fileSize   = ((u64)d->fd.nFileSizeHigh << 32) | d->fd.nFileSizeLow;
	}
	return 0;
}

static inline Result FSDIR_Close (Handle h)
{
	if (s_h[h].find != INVALID_HANDLE_VALUE)
		FindClose (s_h[h].find);
	s_h[h].used = false;
	return 0;
}

static inline Result FSUSER_DeleteFile (FS_Archive a, FS_Path p)
{
	(void)a;
	wchar_t path[2048];
	shimPath (p, path);
	return DeleteFileW (path) ? 0 : -1;
}

static inline Result FSUSER_DeleteDirectoryRecursively (FS_Archive a, FS_Path p)
{
	(void)a;
	wchar_t path[2050] = {0};
	shimPath (p, path);
	DWORD attrs = GetFileAttributesW (path);
	if (attrs == INVALID_FILE_ATTRIBUTES || !(attrs & FILE_ATTRIBUTE_DIRECTORY))
		return -1;
	path[wcslen (path) + 1] = 0;
	SHFILEOPSTRUCTW op      = {0};
	op.wFunc                = FO_DELETE;
	op.pFrom                = path;
	op.fFlags               = FOF_NO_UI;
	return SHFileOperationW (&op) == 0 ? 0 : -1;
}

static inline Result FSUSER_CreateDirectory (FS_Archive a, FS_Path p, u32 attr)
{
	(void)a, (void)attr;
	wchar_t path[2048];
	shimPath (p, path);
	return CreateDirectoryW (path, NULL) ? 0 : -1;
}

static inline Result shimRename (FS_Path from, FS_Path to, bool dir)
{
	wchar_t a[2048], b[2048];
	shimPath (from, a);
	shimPath (to, b);
	DWORD attrs = GetFileAttributesW (a);
	if (attrs == INVALID_FILE_ATTRIBUTES || !!(attrs & FILE_ATTRIBUTE_DIRECTORY) != dir)
		return -1;
	return MoveFileW (a, b) ? 0 : -1;
}
static inline Result FSUSER_RenameFile (FS_Archive a, FS_Path f, FS_Archive b, FS_Path t)
{
	(void)a, (void)b;
	return shimRename (f, t, false);
}
static inline Result FSUSER_RenameDirectory (FS_Archive a, FS_Path f, FS_Archive b, FS_Path t)
{
	(void)a, (void)b;
	return shimRename (f, t, true);
}

// ---- utf
static inline ssize_t utf8_to_utf16 (uint16_t *out, uint8_t const *in, size_t len)
{
	int n = MultiByteToWideChar (CP_UTF8, MB_ERR_INVALID_CHARS, (char const *)in, -1, (wchar_t *)out, (int)len);
	return n > 0 ? n - 1 : -1;
}
static inline ssize_t utf16_to_utf8 (uint8_t *out, uint16_t const *in, size_t len)
{
	int n = WideCharToMultiByte (CP_UTF8, 0, (wchar_t const *)in, -1, (char *)out, (int)len, NULL, NULL);
	return n > 0 ? n - 1 : -1;
}

// ---- threads and sync
typedef HANDLE Thread;
typedef void (*ThreadFunc) (void *);
typedef CRITICAL_SECTION LightLock;
typedef HANDLE LightSemaphore;
typedef struct { ThreadFunc fn; void *arg; } ShimThreadArg;
static DWORD WINAPI shimThreadMain (LPVOID p)
{
	ShimThreadArg a = *(ShimThreadArg *)p;
	free (p);
	a.fn (a.arg);
	return 0;
}
static inline Thread threadCreate (ThreadFunc fn, void *arg, size_t stack, int prio, int core, bool detached)
{
	(void)prio, (void)core, (void)detached;
	ShimThreadArg *a = malloc (sizeof (*a));
	a->fn            = fn;
	a->arg           = arg;
	return CreateThread (NULL, stack, shimThreadMain, a, 0, NULL);
}
static inline void threadJoin (Thread t, u64 timeout) { (void)timeout; WaitForSingleObject (t, INFINITE); }
static inline void threadFree (Thread t) { CloseHandle (t); }
static inline void LightLock_Init (LightLock *l) { InitializeCriticalSection (l); }
static inline void LightLock_Lock (LightLock *l) { EnterCriticalSection (l); }
static inline void LightLock_Unlock (LightLock *l) { LeaveCriticalSection (l); }
static inline void LightSemaphore_Init (LightSemaphore *s, s16 init, s16 max) { *s = CreateSemaphoreW (NULL, init, max, NULL); }
static inline void LightSemaphore_Acquire (LightSemaphore *s, s32 n) { while (n--) WaitForSingleObject (*s, INFINITE); }
static inline void LightSemaphore_Release (LightSemaphore *s, s32 n) { ReleaseSemaphore (*s, n, NULL); }
static inline void svcSleepThread (s64 ns) { Sleep ((DWORD)(ns / 1000000)); }
static inline u64 osGetTime (void) { return GetTickCount64 (); }
static inline void osSetSpeedupEnable (bool e) { (void)e; }

// ---- apt / hid / gfx / lcd
typedef enum { APTHOOK_ONSUSPEND, APTHOOK_ONRESTORE, APTHOOK_ONSLEEP, APTHOOK_ONWAKEUP, APTHOOK_ONEXIT } APT_HookType;
typedef void (*aptHookFn) (APT_HookType, void *);
typedef struct { aptHookFn fn; void *param; } aptHookCookie;
static aptHookCookie *s_cookie;
static inline void aptHook (aptHookCookie *c, aptHookFn fn, void *param)
{
	c->fn    = fn;
	c->param = param;
	s_cookie = c;
}
static inline void aptUnhook (aptHookCookie *c) { (void)c; s_cookie = NULL; }
static inline bool aptMainLoop (void)
{
	static wchar_t flag[MAX_PATH];
	if (!flag[0] && !GetEnvironmentVariableW (L"CEMENT_SLEEPFLAG", flag, MAX_PATH))
		wcscpy (flag, L"sleep.flag");
	if (s_cookie && GetFileAttributesW (flag) != INVALID_FILE_ATTRIBUTES)
	{
		fprintf (stderr, "[shim] sleeping\n");
		s_cookie->fn (APTHOOK_ONSLEEP, s_cookie->param);
		while (GetFileAttributesW (flag) != INVALID_FILE_ATTRIBUTES)
			Sleep (50);
		fprintf (stderr, "[shim] woke up\n");
		s_cookie->fn (APTHOOK_ONWAKEUP, s_cookie->param);
	}
	return true;
}
#define KEY_A BIT (0)
#define KEY_START BIT (3)
#define KEY_SELECT BIT (2)
#define KEY_UP BIT (6)
#define KEY_DOWN BIT (7)
static u32 s_keys;
// keys are injected by writing "A", "UP" or "DOWN" into the file named by
// env CEMENT_KEYS; the file is consumed on the next frame
static inline void hidScanInput (void)
{
	static wchar_t file[MAX_PATH];
	s_keys = 0;
	if (!file[0] && !GetEnvironmentVariableW (L"CEMENT_KEYS", file, MAX_PATH))
		wcscpy (file, L"keys.txt");
	FILE *f = _wfopen (file, L"r");
	if (!f)
		return;
	char buf[16] = {0};
	fread (buf, 1, sizeof (buf) - 1, f);
	fclose (f);
	_wremove (file);
	s_keys = !strncmp (buf, "A", 1) ? KEY_A : !strncmp (buf, "UP", 2) ? KEY_UP : !strncmp (buf, "DOWN", 4) ? KEY_DOWN : 0;
}
static inline u32 hidKeysDown (void) { return s_keys; }
static inline void gfxInitDefault (void) {}
static inline void gfxExit (void) {}
static inline void gspWaitForVBlank (void) { Sleep (16); }
#define GFX_TOP 0
#define GFX_BOTTOM 1
typedef struct { int dummy; } PrintConsole;
static inline void *consoleInit (int s, void *c) { (void)s, (void)c; return NULL; }
static inline void consoleSelect (PrintConsole *c) { (void)c; }

// ---- am: a CIA "install" is written to the file named by env CEMENT_AMOUT
typedef enum { MEDIATYPE_NAND = 0, MEDIATYPE_SD = 1, MEDIATYPE_GAME_CARD = 2 } FS_MediaType;
typedef int NS_APPID;
static inline Result amInit (void) { return 0; }
static inline void amExit (void) {}
static inline Result AM_InitializeExternalTitleDatabase (bool o) { (void)o; return 0; }
static inline Result AM_QueryAvailableExternalTitleDatabase (bool *a) { if (a) *a = true; return 0; }
static inline Result AM_DeleteTitle (FS_MediaType m, u64 id)
{
	fprintf (stderr, "[shim] AM_DeleteTitle media=%d id=%016llx\n", (int)m, (unsigned long long)id);
	return 0;
}
static inline Result AM_DeleteTicket (u64 id) { (void)id; return 0; }
static inline Result AM_StartCiaInstall (FS_MediaType m, Handle *h)
{
	wchar_t out[MAX_PATH];
	if (!GetEnvironmentVariableW (L"CEMENT_AMOUT", out, MAX_PATH))
		wcscpy (out, L"am-installed.bin");
	HANDLE f = CreateFileW (out, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, 0, NULL);
	if (f == INVALID_HANDLE_VALUE)
		return -1;
	fprintf (stderr, "[shim] AM_StartCiaInstall media=%d\n", (int)m);
	*h           = shimAlloc ();
	s_h[*h].file = f;
	return 0;
}
static inline Result AM_FinishCiaInstall (Handle h)
{
	fprintf (stderr, "[shim] AM_FinishCiaInstall\n");
	CloseHandle (s_h[h].file);
	s_h[h].used = false;
	return 0;
}
static inline Result AM_CancelCIAInstall (Handle h)
{
	fprintf (stderr, "[shim] AM_CancelCIAInstall\n");
	CloseHandle (s_h[h].file);
	s_h[h].used = false;
	return 0;
}
static inline Result AM_InstallFirm (u64 id) { (void)id; return 0; }
static inline Result APT_CheckNew3DS (bool *n3ds) { *n3ds = false; return 0; }
static inline bool envIsHomebrew (void) { return false; }
static inline int envGetAptAppId (void) { return 0x300; }
static inline Result APT_GetAppletInfo (NS_APPID id, u64 *tid, u8 *mt, bool *a, bool *b, void *c)
{
	(void)id, (void)a, (void)b, (void)c;
	*tid = 0x000400000F7A5100ull; // Cement itself
	*mt  = MEDIATYPE_SD;
	return 0;
}
static inline void aptSetSleepAllowed (bool a) { fprintf (stderr, "[shim] sleep allowed=%d\n", (int)a); }
#define printf(...) ((void)0) // keep the test log readable
enum { GSPLCD_SCREEN_TOP = 1, GSPLCD_SCREEN_BOTTOM = 2, GSPLCD_SCREEN_BOTH = 3 };
static inline Result gspLcdInit (void) { return 0; }
static inline void gspLcdExit (void) {}
static inline Result GSPLCD_PowerOnBacklight (u32 s) { (void)s; return 0; }
static inline Result GSPLCD_PowerOffBacklight (u32 s) { (void)s; return 0; }

// ---- network services
static inline Result acInit (void) { return 0; }
static inline void acExit (void) {}
static inline Result ACU_GetWifiStatus (u32 *out) { *out = 1; return 0; }
typedef enum { NDM_EXCLUSIVE_STATE_INFRASTRUCTURE = 1 } ndmExclusiveState;
static inline Result ndmuInit (void) { return 0; }
static inline void ndmuExit (void) {}
static inline Result NDMU_EnterExclusiveState (ndmExclusiveState s) { (void)s; return 0; }
static inline Result NDMU_LeaveExclusiveState (void) { return 0; }
static inline Result NDMU_LockState (void) { return 0; }
static inline Result NDMU_UnlockState (void) { return 0; }
static inline Result socInit (u32 *buf, u32 size)
{
	(void)buf, (void)size;
	WSADATA w;
	return WSAStartup (MAKEWORD (2, 2), &w) == 0 ? 0 : -1;
}
static inline Result socExit (void)
{
	fprintf (stderr, "[shim] socExit\n");
	WSACleanup ();
	return 0;
}
static inline long gethostid (void) { return (long)inet_addr ("127.0.0.1"); }
