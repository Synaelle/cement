// Syna 3DS Tools - minimal, fast FTP server for the 3DS (SD card only)
//
// Design:
// - one non-blocking poll() loop on the main thread handles every socket
// - one disk thread performs all SD reads/writes, so network receive and
//   SD writes overlap (NBUF buffers of CHUNK bytes per session)
// - closing the lid lets the system sleep: the network is torn down in the
//   APT sleep hook and brought back up after wake-up
// - uploaded .cia files are listed on the bottom screen; A installs one
//   (same steps as FBI) and deletes the file afterwards

#include <3ds.h>

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <malloc.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef APP_VERSION
#define APP_VERSION    "dev" // set by the Makefile: <VERSION file>.<commit count>-<commit>[-dev]
#endif
#define VERSION        APP_VERSION
#define CTRL_PORT      5000
#define MAX_SESSIONS   4
#define SOC_ALIGN      0x1000
#define SOC_BUFSIZE    0x100000
#define SOCK_BUFSIZE   32768 // same value ftpd uses on 3DS
#define NBUF           4
#define CHUNK          (256 * 1024)
#define CMD_SIZE       4096
#define PATH_SIZE      1024
#define MAX_PENDING    16
#define QUEUE_SIZE     64
#define DATA_TIMEOUT   15000  // ms to wait for the data connection
#define IDLE_TIMEOUT   300000 // ms without any command before dropping a client

// ---------------------------------------------------------------------------
// disk thread

enum { B_FREE, B_FILL, B_BUSY, B_DONE };
enum { X_NONE, X_LIST, X_MLSD, X_NLST, X_STOR, X_RETR };

typedef struct
{
	Handle fh;
	bool up; // true: network -> SD, false: SD -> network
	u8 *buf[NBUF];
	u32 len[NBUF];
	u64 off[NBUF];
	volatile int st[NBUF];
	volatile Result rc[NBUF];
	int cur;     // STOR: buffer being filled, RETR: buffer being sent
	u32 sendPos; // RETR: bytes of buf[cur] already sent
	u64 pos;     // STOR: file offset of next byte, RETR: next offset to read
	u64 size;    // RETR: file size
	u64 done;    // bytes moved over the network
	bool err;
} Xfer;

typedef struct
{
	Xfer *x;
	int i;
} Job;

static Job s_queue[QUEUE_SIZE];
static int s_qHead, s_qTail;
static LightLock s_qLock;
static LightSemaphore s_qSem;
static Thread s_diskThread;

static void submit (Xfer *x, int i)
{
	x->st[i] = B_BUSY;
	LightLock_Lock (&s_qLock);
	s_queue[s_qHead] = (Job){x, i};
	s_qHead          = (s_qHead + 1) % QUEUE_SIZE;
	LightLock_Unlock (&s_qLock);
	LightSemaphore_Release (&s_qSem, 1);
}

static void diskThread (void *arg)
{
	(void)arg;
	for (;;)
	{
		LightSemaphore_Acquire (&s_qSem, 1);
		LightLock_Lock (&s_qLock);
		Job job = s_queue[s_qTail];
		s_qTail = (s_qTail + 1) % QUEUE_SIZE;
		LightLock_Unlock (&s_qLock);

		if (!job.x)
			break;

		Xfer *x = job.x;
		int i   = job.i;
		u32 n   = 0;
		Result r;
		if (x->up)
		{
			r = FSFILE_Write (x->fh, &n, x->off[i], x->buf[i], x->len[i], 0);
			if (R_SUCCEEDED (r) && n != x->len[i])
				r = -1;
		}
		else
		{
			r         = FSFILE_Read (x->fh, &n, x->off[i], x->buf[i], CHUNK);
			x->len[i] = n;
		}
		x->rc[i] = r;
		__atomic_store_n (&x->st[i], B_DONE, __ATOMIC_RELEASE);
	}
}

static int bufState (Xfer *x, int i)
{
	return __atomic_load_n (&x->st[i], __ATOMIC_ACQUIRE);
}

// wait until the disk thread is done with every buffer of this transfer
static void xferDrain (Xfer *x)
{
	for (int i = 0; i < NBUF; ++i)
	{
		while (bufState (x, i) == B_BUSY)
			svcSleepThread (1000000);
		if (x->st[i] == B_DONE && R_FAILED (x->rc[i]))
			x->err = true;
		x->st[i] = B_FREE;
	}
}

// ---------------------------------------------------------------------------
// state

typedef struct
{
	int ctrl, pasv, data;
	u32 peer; // client IP: one PC opens several connections
	char cmd[CMD_SIZE];
	int cmdLen;
	char cwd[PATH_SIZE];
	char rnfr[PATH_SIZE];
	u64 rest;
	u64 lastCmd;
	u64 dataSince;

	int mode; // X_*: current/pending data transfer
	bool started;
	char name[64];        // file name shown on screen
	char path[PATH_SIZE]; // full path of the current file transfer

	char *list;
	u32 listLen, listPos, listCap;

	Xfer x;
	bool xferOpen;

	// progress reporting (SITE STATUS)
	u64 allo;      // size announced with ALLO for the next STOR (0 = unknown)
	u64 xferBase;  // REST offset of the current transfer
	u64 xferTotal; // full size of the current file (0 = unknown)
	u64 xferStart; // osGetTime when the data started flowing

	// whole job announced with SITE BATCH <files> <bytes> <kind>: busy between files, ETA
	char batchKind[8]; // "up", "down" or "delete"
	u32 batchFiles, batchFilesDone;
	u64 batchBytes, batchBytesDone, batchStart;
} Session;

typedef struct
{
	int fd;
	u64 deadline;
} Pending;

static Session s_sess[MAX_SESSIONS];
static Pending s_pending[MAX_PENDING];
static int s_listen = -1;
static FS_Archive s_sd;
static u32 *s_socBuf;
static bool s_netUp;
static bool s_ndmLocked;
static u32 s_ip;
static u16 s_pasvPort = 5001;
static bool s_backlight = true;
static bool s_bottomOn;
static aptHookCookie s_hook;
static u64 s_nextNetCheck;
static u64 s_lastBytes, s_lastTime;
static u32 s_rate;
static char s_status[64] = "Starting...";
static u64 s_fsBusyUntil; // a delete/rename/mkdir happened recently (blocks CIA installs)
static u64 s_lastActivity; // last transfer end or file system change (SITE STATUS idle_ms)

// ---------------------------------------------------------------------------
// helpers

static void setBacklight (u32 screens, bool on)
{
	if (R_FAILED (gspLcdInit ()))
		return;
	if (on)
		GSPLCD_PowerOnBacklight (screens);
	else
		GSPLCD_PowerOffBacklight (screens);
	gspLcdExit ();
}

static void applyBacklight (void)
{
	// the bottom screen is only lit while it lists CIA files
	setBacklight (GSPLCD_SCREEN_BOTTOM, s_bottomOn);
	setBacklight (GSPLCD_SCREEN_TOP, s_backlight);
}

static void setNonBlocking (int fd)
{
	fcntl (fd, F_SETFL, fcntl (fd, F_GETFL, 0) | O_NONBLOCK);
}

static void setBuffers (int fd)
{
	int size = SOCK_BUFSIZE;
	setsockopt (fd, SOL_SOCKET, SO_RCVBUF, &size, sizeof (size));
	setsockopt (fd, SOL_SOCKET, SO_SNDBUF, &size, sizeof (size));
}

static void closeNow (int *fd)
{
	if (*fd >= 0)
		close (*fd);
	*fd = -1;
}

// half-close and wait for the peer to close, so no queued data gets lost
static void closeGraceful (int *fd)
{
	if (*fd < 0)
		return;

	shutdown (*fd, SHUT_WR);
	struct linger l = {1, 0};
	setsockopt (*fd, SOL_SOCKET, SO_LINGER, &l, sizeof (l));

	for (int i = 0; i < MAX_PENDING; ++i)
	{
		if (s_pending[i].fd < 0)
		{
			s_pending[i].fd       = *fd;
			s_pending[i].deadline = osGetTime () + 3000;
			*fd                   = -1;
			return;
		}
	}
	closeNow (fd);
}

static bool toFsPath (char const *path, u16 *buf, FS_Path *out)
{
	ssize_t n = utf8_to_utf16 (buf, (uint8_t const *)path, PATH_SIZE - 1);
	if (n < 0 || n >= PATH_SIZE - 1)
		return false;
	buf[n] = 0;
	*out   = fsMakePath (PATH_UTF16, buf);
	return true;
}

// resolve arg against cwd into an absolute, normalized path ("/", "/a/b").
// Only the SD card archive (s_sd) is ever opened and ".." can't climb above
// "/", so nothing outside the SD card is reachable.
static bool resolvePath (char const *cwd, char const *arg, char *out)
{
	char tmp[PATH_SIZE * 2];
	if (strchr (arg, '\\')) // not a valid name character on the SD card
		return false;
	if (arg[0] == '/')
		snprintf (tmp, sizeof (tmp), "%s", arg);
	else
		snprintf (tmp, sizeof (tmp), "%s/%s", cwd, arg);

	size_t len = 0;
	out[0]     = 0;
	char *save = NULL;
	for (char *tok = strtok_r (tmp, "/", &save); tok; tok = strtok_r (NULL, "/", &save))
	{
		if (strcmp (tok, ".") == 0 || tok[0] == 0)
			continue;
		if (strcmp (tok, "..") == 0)
		{
			char *slash = strrchr (out, '/');
			if (slash)
				*slash = 0;
			len = strlen (out);
			continue;
		}
		size_t tl = strlen (tok);
		if (len + 1 + tl >= PATH_SIZE)
			return false;
		out[len++] = '/';
		memcpy (out + len, tok, tl);
		len += tl;
		out[len] = 0;
	}
	if (len == 0)
		strcpy (out, "/");
	return true;
}

static bool isDir (char const *path)
{
	u16 buf[PATH_SIZE];
	FS_Path p;
	Handle h;
	if (!toFsPath (path, buf, &p) || R_FAILED (FSUSER_OpenDirectory (&h, s_sd, p)))
		return false;
	FSDIR_Close (h);
	return true;
}

static bool fileSize (char const *path, u64 *size)
{
	u16 buf[PATH_SIZE];
	FS_Path p;
	Handle h;
	if (!toFsPath (path, buf, &p) || R_FAILED (FSUSER_OpenFile (&h, s_sd, p, FS_OPEN_READ, 0)))
		return false;
	Result r = FSFILE_GetSize (h, size);
	FSFILE_Close (h);
	return R_SUCCEEDED (r);
}

static void setName (Session *s, char const *path)
{
	snprintf (s->path, sizeof (s->path), "%s", path);
	char const *base = strrchr (path, '/');
	snprintf (s->name, sizeof (s->name), "%.63s", base ? base + 1 : path);
}

// ---------------------------------------------------------------------------
// uploaded CIA list (bottom screen)

#define MAX_CIAS 64

typedef struct
{
	char path[PATH_SIZE];
	u64 size;
} Cia;

static Cia s_cias[MAX_CIAS];
static int s_ciaCount, s_ciaSel;
static bool s_bottomDirty = true;

static bool isCia (char const *path)
{
	size_t n = strlen (path);
	return n > 4 && strcasecmp (path + n - 4, ".cia") == 0;
}

// remove path, or everything under it when it is a folder
static void ciaRemove (char const *path)
{
	size_t n = strlen (path);
	for (int i = 0; i < s_ciaCount;)
	{
		char const *p = s_cias[i].path;
		if (strcmp (p, path) == 0 || (strncmp (p, path, n) == 0 && (p[n] == '/' || strcmp (path, "/") == 0)))
		{
			memmove (&s_cias[i], &s_cias[i + 1], (s_ciaCount - i - 1) * sizeof (Cia));
			--s_ciaCount;
			s_bottomDirty = true;
		}
		else
			++i;
	}
	if (s_ciaSel >= s_ciaCount)
		s_ciaSel = s_ciaCount ? s_ciaCount - 1 : 0;
}

static void ciaAdd (char const *path, u64 size)
{
	if (!isCia (path))
		return;
	ciaRemove (path);
	if (s_ciaCount == MAX_CIAS)
		return;
	snprintf (s_cias[s_ciaCount].path, PATH_SIZE, "%s", path);
	s_cias[s_ciaCount++].size = size;
	s_bottomDirty             = true;
}

// ---------------------------------------------------------------------------
// sessions

static void reply (Session *s, char const *fmt, ...)
{
	if (s->ctrl < 0)
		return;

	char buf[PATH_SIZE + 128];
	va_list ap;
	va_start (ap, fmt);
	int len = vsnprintf (buf, sizeof (buf), fmt, ap);
	va_end (ap);
	if (len < 0)
		return;
	if (len >= (int)sizeof (buf))
		len = sizeof (buf) - 1;

	int sent = 0, tries = 0;
	while (sent < len)
	{
		int n = send (s->ctrl, buf + sent, len - sent, 0);
		if (n > 0)
		{
			sent += n;
			continue;
		}
		if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK) && ++tries < 50)
		{
			struct pollfd p = {s->ctrl, POLLOUT, 0};
			poll (&p, 1, 100);
			continue;
		}
		closeNow (&s->ctrl);
		return;
	}
}

static void xferClose (Session *s)
{
	if (!s->xferOpen)
		return;
	xferDrain (&s->x);
	if (s->x.up)
		FSFILE_Flush (s->x.fh);
	FSFILE_Close (s->x.fh);
	s->xferOpen = false;
}

static void endTransfer (Session *s)
{
	xferClose (s);
	free (s->list);
	s->list    = NULL;
	s->listLen = s->listPos = s->listCap = 0;
	s->mode                              = X_NONE;
	s->started                           = false;
	s->name[0]                           = 0;
	s->x.done                            = 0;
}

static void closeSession (Session *s)
{
	endTransfer (s);
	s->allo = 0;
	s->batchFiles = s->batchFilesDone = 0;
	s->batchBytes = s->batchBytesDone = 0;
	closeNow (&s->data);
	closeNow (&s->pasv);
	closeNow (&s->ctrl);
	for (int i = 0; i < NBUF; ++i)
	{
		free (s->x.buf[i]);
		s->x.buf[i] = NULL;
	}
}

static bool allocBuffers (Session *s)
{
	for (int i = 0; i < NBUF; ++i)
	{
		if (!s->x.buf[i])
			s->x.buf[i] = memalign (64, CHUNK);
		if (!s->x.buf[i])
			return false;
	}
	return true;
}

static void listAppend (Session *s, char const *str, size_t n)
{
	if (s->listLen + n > s->listCap)
	{
		u32 cap = s->listCap ? s->listCap * 2 : 16384;
		while (cap < s->listLen + n)
			cap *= 2;
		char *p = realloc (s->list, cap);
		if (!p)
			return;
		s->list    = p;
		s->listCap = cap;
	}
	memcpy (s->list + s->listLen, str, n);
	s->listLen += n;
}

static void listEntry (Session *s, int mode, char const *name, bool dir, u64 size)
{
	char line[PATH_SIZE + 96];
	int n;
	if (mode == X_MLSD)
		n = snprintf (line, sizeof (line), "type=%s;size=%llu; %s\r\n", dir ? "dir" : "file", size, name);
	else if (mode == X_NLST)
		n = snprintf (line, sizeof (line), "%s\r\n", name);
	else
		n = snprintf (line,
		    sizeof (line),
		    "%s 1 3ds 3ds %llu Jan 01 2000 %s\r\n",
		    dir ? "drwxrwxrwx" : "-rw-rw-rw-",
		    size,
		    name);
	if (n > 0)
		listAppend (s, line, n < (int)sizeof (line) ? n : (int)sizeof (line) - 1);
}

static bool buildList (Session *s, char const *path, int mode)
{
	static FS_DirectoryEntry ents[32];
	u16 buf[PATH_SIZE];
	FS_Path p;
	Handle h;
	if (!toFsPath (path, buf, &p) || R_FAILED (FSUSER_OpenDirectory (&h, s_sd, p)))
		return false;

	u32 count = 0;
	while (R_SUCCEEDED (FSDIR_Read (h, &count, 32, ents)) && count > 0)
	{
		for (u32 i = 0; i < count; ++i)
		{
			char name[0x106 * 3 + 1];
			ssize_t n = utf16_to_utf8 ((uint8_t *)name, ents[i].name, sizeof (name) - 1);
			if (n <= 0)
				continue;
			name[n] = 0;
			bool dir = ents[i].attributes & FS_ATTRIBUTE_DIRECTORY;
			listEntry (s, mode, name, dir, dir ? 0 : ents[i].fileSize);
		}
	}
	FSDIR_Close (h);
	return true;
}

static void openPasv (Session *s, bool extended)
{
	closeNow (&s->data);
	closeNow (&s->pasv);

	s->pasv = socket (AF_INET, SOCK_STREAM, 0);
	if (s->pasv < 0)
	{
		reply (s, "425 Can't open data socket\r\n");
		return;
	}
	setBuffers (s->pasv);

	// binding to port 0 is unreliable on 3DS (ftpd works around it the same way)
	struct sockaddr_in addr = {0};
	addr.sin_family         = AF_INET;
	addr.sin_addr.s_addr    = s_ip;
	bool bound              = false;
	for (int tries = 0; tries < 20 && !bound; ++tries)
	{
		if (s_pasvPort > 10000)
			s_pasvPort = 5001;
		addr.sin_port = htons (s_pasvPort++);
		bound         = bind (s->pasv, (struct sockaddr *)&addr, sizeof (addr)) == 0;
	}
	if (!bound || listen (s->pasv, 1) != 0)
	{
		closeNow (&s->pasv);
		reply (s, "425 Can't open data socket\r\n");
		return;
	}
	setNonBlocking (s->pasv);
	s->dataSince = osGetTime ();

	u16 port    = ntohs (addr.sin_port);
	u8 const *a = (u8 const *)&s_ip;
	if (extended)
		reply (s, "229 Entering Extended Passive Mode (|||%u|)\r\n", port);
	else
		reply (s, "227 Entering Passive Mode (%u,%u,%u,%u,%u,%u)\r\n", a[0], a[1], a[2], a[3], port >> 8, port & 0xFF);
}

// returns false when the transfer could not be prepared (reply already sent)
static bool prepareFile (Session *s, char const *path, int mode, bool append)
{
	u16 buf[PATH_SIZE];
	FS_Path p;
	Xfer *x = &s->x;
	if (!toFsPath (path, buf, &p))
	{
		reply (s, "553 Bad file name\r\n");
		return false;
	}
	if (!allocBuffers (s))
	{
		reply (s, "451 Out of memory\r\n");
		return false;
	}

	memset (x->len, 0, sizeof (x->len));
	for (int i = 0; i < NBUF; ++i)
		x->st[i] = B_FREE;
	x->cur = 0;
	x->sendPos = 0;
	x->done    = 0;
	x->err     = false;
	x->up      = mode == X_STOR;

	if (x->up)
	{
		ciaRemove (path);
		if (R_FAILED (FSUSER_OpenFile (&x->fh, s_sd, p, FS_OPEN_WRITE | FS_OPEN_CREATE, 0)))
		{
			reply (s, "550 Can't create file\r\n");
			return false;
		}
		u64 size = 0;
		FSFILE_GetSize (x->fh, &size);
		u64 start = append ? size : s->rest;
		if (start > size || R_FAILED (FSFILE_SetSize (x->fh, start)))
		{
			FSFILE_Close (x->fh);
			reply (s, "554 Invalid restart position\r\n");
			return false;
		}
		x->pos       = start;
		s->xferBase  = start;
		s->xferTotal = s->allo;
		s->allo      = 0;
	}
	else
	{
		if (R_FAILED (FSUSER_OpenFile (&x->fh, s_sd, p, FS_OPEN_READ, 0)))
		{
			reply (s, "550 Can't open file\r\n");
			return false;
		}
		FSFILE_GetSize (x->fh, &x->size);
		if (s->rest > x->size)
		{
			FSFILE_Close (x->fh);
			reply (s, "554 Invalid restart position\r\n");
			return false;
		}
		x->pos       = s->rest;
		s->xferBase  = s->rest;
		s->xferTotal = x->size;
	}
	s->xferOpen = true;
	return true;
}

static void startData (Session *s)
{
	s->started   = true;
	s->xferStart = osGetTime ();
	if (s->mode == X_RETR)
	{
		// read ahead into every buffer
		Xfer *x = &s->x;
		for (int i = 0; i < NBUF && x->pos < x->size; ++i)
		{
			x->off[i] = x->pos;
			x->pos += CHUNK;
			submit (x, i);
		}
	}
}

static void finishData (Session *s, bool ok, char const *msg)
{
	bool upload = s->mode == X_STOR;
	u64 size    = s->x.pos;
	if (s->xferOpen)
	{
		xferClose (s);
		if (s->x.err)
			ok = false, msg = "451 SD card error";
	}
	if (upload && ok)
		ciaAdd (s->path, size);
	if ((s->mode == X_STOR || s->mode == X_RETR) && s->batchFiles)
	{
		s->batchBytesDone += s->x.done;
		if (ok)
			s->batchFilesDone++;
	}
	s_lastActivity = osGetTime ();
	if (ok)
		closeGraceful (&s->data);
	else
		closeNow (&s->data);
	endTransfer (s);
	reply (s, "%s\r\n", msg);
}

static void siteStatus (Session *self);

static void handleCommand (Session *s, char *line)
{
	char *arg = strchr (line, ' ');
	if (arg)
		*arg++ = 0;
	else
		arg = line + strlen (line);
	for (char *c = line; *c; ++c)
		if (*c >= 'a' && *c <= 'z')
			*c -= 32;

	char path[PATH_SIZE];
	u16 buf[PATH_SIZE], buf2[PATH_SIZE];
	FS_Path p, p2;
	char const *cmd = line;
	s->lastCmd      = osGetTime ();

	// a deletion is a stream of DELE/RMD commands: count it as busy for a moment
	if (!strcmp (cmd, "DELE") || !strcmp (cmd, "RMD") || !strcmp (cmd, "XRMD") || !strcmp (cmd, "RNFR") ||
	    !strcmp (cmd, "RNTO") || !strcmp (cmd, "MKD") || !strcmp (cmd, "XMKD"))
		s_fsBusyUntil = s_lastActivity = s->lastCmd + 2000;
	if ((!strcmp (cmd, "DELE") || !strcmp (cmd, "RMD") || !strcmp (cmd, "XRMD")) && s->batchFiles &&
	    !strcmp (s->batchKind, "delete") && s->batchFilesDone < s->batchFiles)
		s->batchFilesDone++;

	bool busy = s->mode != X_NONE;
	if (busy && (strcmp (cmd, "PASV") == 0 || strcmp (cmd, "EPSV") == 0 || strcmp (cmd, "LIST") == 0 ||
	                strcmp (cmd, "NLST") == 0 || strcmp (cmd, "MLSD") == 0 || strcmp (cmd, "RETR") == 0 ||
	                strcmp (cmd, "STOR") == 0 || strcmp (cmd, "APPE") == 0))
		reply (s, "450 Transfer in progress\r\n");
	else if (strcmp (cmd, "USER") == 0 || strcmp (cmd, "PASS") == 0)
		reply (s, "230 Logged in\r\n");
	else if (strcmp (cmd, "SYST") == 0)
		reply (s, "215 UNIX Type: L8\r\n");
	else if (strcmp (cmd, "FEAT") == 0)
		reply (s,
		    "211-Features:\r\n MLST type*;size*;\r\n MLSD\r\n SIZE\r\n REST STREAM\r\n UTF8\r\n EPSV\r\n"
		    "211 End\r\n");
	else if (strcmp (cmd, "OPTS") == 0 || strcmp (cmd, "TYPE") == 0 || strcmp (cmd, "MODE") == 0 ||
	         strcmp (cmd, "STRU") == 0 || strcmp (cmd, "NOOP") == 0)
		reply (s, "200 OK\r\n");
	else if (strcmp (cmd, "ALLO") == 0)
	{
		s->allo = strtoull (arg, NULL, 10); // size of the next STOR, for progress/ETA
		reply (s, "200 OK\r\n");
	}
	else if (strcmp (cmd, "SITE") == 0)
	{
		char sub[16] = {0};
		sscanf (arg, "%15s", sub);
		for (char *c = sub; *c; ++c)
			if (*c >= 'a' && *c <= 'z')
				*c -= 32;
		if (strcmp (sub, "STATUS") == 0)
			siteStatus (s);
		else if (strcmp (sub, "BATCH") == 0)
		{
			// SITE BATCH <files> <bytes> [up|down|delete]; "SITE BATCH 0 0" ends it
			unsigned long long files = 0, bytes = 0;
			char kind[8]             = "up";
			sscanf (arg + 5, "%llu %llu %7s", &files, &bytes, kind);
			s->batchFiles     = (u32)files;
			s->batchBytes     = bytes;
			s->batchFilesDone = 0;
			s->batchBytesDone = 0;
			s->batchStart     = osGetTime ();
			snprintf (s->batchKind, sizeof (s->batchKind), "%s", kind);
			s_lastActivity = s->batchStart;
			reply (s, "200 OK\r\n");
		}
		else
			reply (s, "502 SITE STATUS or SITE BATCH\r\n");
	}
	else if (strcmp (cmd, "PWD") == 0 || strcmp (cmd, "XPWD") == 0)
		reply (s, "257 \"%s\"\r\n", s->cwd);
	else if (strcmp (cmd, "CWD") == 0 || strcmp (cmd, "CDUP") == 0 || strcmp (cmd, "XCUP") == 0)
	{
		if (!resolvePath (s->cwd, strcmp (cmd, "CWD") == 0 ? arg : "..", path) || !isDir (path))
			reply (s, "550 No such directory\r\n");
		else
		{
			strcpy (s->cwd, path);
			reply (s, "250 OK\r\n");
		}
	}
	else if (strcmp (cmd, "PASV") == 0)
		openPasv (s, false);
	else if (strcmp (cmd, "EPSV") == 0)
		openPasv (s, true);
	else if (strcmp (cmd, "PORT") == 0 || strcmp (cmd, "EPRT") == 0)
		reply (s, "502 Use PASV\r\n");
	else if (strcmp (cmd, "REST") == 0)
	{
		s->rest = strtoull (arg, NULL, 10);
		reply (s, "350 Restarting at %llu\r\n", s->rest);
		return; // keep rest for the next command
	}
	else if (strcmp (cmd, "LIST") == 0 || strcmp (cmd, "NLST") == 0 || strcmp (cmd, "MLSD") == 0)
	{
		int mode = cmd[0] == 'L' ? X_LIST : cmd[0] == 'N' ? X_NLST : X_MLSD;
		while (*arg == '-') // skip "-a", "-la" ...
		{
			while (*arg && *arg != ' ')
				++arg;
			while (*arg == ' ')
				++arg;
		}
		if (s->pasv < 0 && s->data < 0)
			reply (s, "425 Use PASV first\r\n");
		else if (!resolvePath (s->cwd, arg, path) || !buildList (s, path, mode))
		{
			endTransfer (s);
			reply (s, "550 No such directory\r\n");
		}
		else
		{
			s->mode = mode;
			reply (s, "150 Listing\r\n");
		}
	}
	else if (strcmp (cmd, "MLST") == 0)
	{
		u64 size = 0;
		if (!resolvePath (s->cwd, arg, path))
			reply (s, "550 Bad path\r\n");
		else if (isDir (path))
			reply (s, "250-Listing\r\n type=dir;size=0; %s\r\n250 End\r\n", path);
		else if (fileSize (path, &size))
			reply (s, "250-Listing\r\n type=file;size=%llu; %s\r\n250 End\r\n", size, path);
		else
			reply (s, "550 Not found\r\n");
	}
	else if (strcmp (cmd, "SIZE") == 0)
	{
		u64 size = 0;
		if (resolvePath (s->cwd, arg, path) && fileSize (path, &size))
			reply (s, "213 %llu\r\n", size);
		else
			reply (s, "550 Not found\r\n");
	}
	else if (strcmp (cmd, "RETR") == 0 || strcmp (cmd, "STOR") == 0 || strcmp (cmd, "APPE") == 0)
	{
		int mode = cmd[0] == 'R' ? X_RETR : X_STOR;
		if (s->pasv < 0 && s->data < 0)
			reply (s, "425 Use PASV first\r\n");
		else if (!resolvePath (s->cwd, arg, path))
			reply (s, "553 Bad file name\r\n");
		else if (prepareFile (s, path, mode, cmd[0] == 'A'))
		{
			s->mode = mode;
			setName (s, path);
			reply (s, "150 Opening data connection\r\n");
		}
	}
	else if (strcmp (cmd, "DELE") == 0)
	{
		if (resolvePath (s->cwd, arg, path) && toFsPath (path, buf, &p) && R_SUCCEEDED (FSUSER_DeleteFile (s_sd, p)))
		{
			ciaRemove (path);
			reply (s, "250 Deleted\r\n");
		}
		else
			reply (s, "550 Delete failed\r\n");
	}
	else if (strcmp (cmd, "RMD") == 0 || strcmp (cmd, "XRMD") == 0)
	{
		// recursive on purpose: removing a folder in one command is much faster
		if (resolvePath (s->cwd, arg, path) && strcmp (path, "/") != 0 && toFsPath (path, buf, &p) &&
		    R_SUCCEEDED (FSUSER_DeleteDirectoryRecursively (s_sd, p)))
		{
			ciaRemove (path);
			reply (s, "250 Removed\r\n");
		}
		else
			reply (s, "550 Remove failed\r\n");
	}
	else if (strcmp (cmd, "MKD") == 0 || strcmp (cmd, "XMKD") == 0)
	{
		if (resolvePath (s->cwd, arg, path) && toFsPath (path, buf, &p) &&
		    R_SUCCEEDED (FSUSER_CreateDirectory (s_sd, p, 0)))
			reply (s, "257 \"%s\" created\r\n", path);
		else
			reply (s, "550 Create failed\r\n");
	}
	else if (strcmp (cmd, "RNFR") == 0)
	{
		u64 size;
		if (resolvePath (s->cwd, arg, s->rnfr) && (isDir (s->rnfr) || fileSize (s->rnfr, &size)))
			reply (s, "350 Ready\r\n");
		else
		{
			s->rnfr[0] = 0;
			reply (s, "550 Not found\r\n");
		}
	}
	else if (strcmp (cmd, "RNTO") == 0)
	{
		if (s->rnfr[0] && resolvePath (s->cwd, arg, path) && toFsPath (s->rnfr, buf, &p) &&
		    toFsPath (path, buf2, &p2) &&
		    (R_SUCCEEDED (FSUSER_RenameFile (s_sd, p, s_sd, p2)) ||
		        R_SUCCEEDED (FSUSER_RenameDirectory (s_sd, p, s_sd, p2))))
		{
			ciaRemove (s->rnfr);
			u64 size = 0;
			if (fileSize (path, &size))
				ciaAdd (path, size);
			reply (s, "250 Renamed\r\n");
		}
		else
			reply (s, "550 Rename failed\r\n");
		s->rnfr[0] = 0;
	}
	else if (strcmp (cmd, "ABOR") == 0)
	{
		if (s->mode != X_NONE)
		{
			closeNow (&s->data);
			endTransfer (s);
			reply (s, "426 Aborted\r\n");
		}
		reply (s, "226 Abort OK\r\n");
	}
	else if (strcmp (cmd, "QUIT") == 0)
	{
		reply (s, "221 Bye\r\n");
		closeSession (s);
	}
	else
		reply (s, "502 Not implemented\r\n");

	s->rest = 0;
}

static void readCommands (Session *s)
{
	for (;;)
	{
		int n = recv (s->ctrl, s->cmd + s->cmdLen, CMD_SIZE - 1 - s->cmdLen, 0);
		if (n == 0 || (n < 0 && errno != EAGAIN && errno != EWOULDBLOCK))
		{
			closeSession (s);
			return;
		}
		if (n < 0)
			return;
		s->cmdLen += n;

		// strip telnet IAC sequences (sent by some clients before ABOR)
		int w = 0;
		for (int r = 0; r < s->cmdLen; ++r)
		{
			if ((u8)s->cmd[r] == 0xFF)
			{
				++r;
				continue;
			}
			s->cmd[w++] = s->cmd[r];
		}
		s->cmdLen = w;

		char *start = s->cmd;
		char *nl;
		while (s->ctrl >= 0 && (nl = memchr (start, '\n', s->cmdLen - (start - s->cmd))))
		{
			*nl = 0;
			if (nl > start && nl[-1] == '\r')
				nl[-1] = 0;
			handleCommand (s, start);
			start = nl + 1;
		}
		if (s->ctrl < 0)
			return;
		s->cmdLen -= start - s->cmd;
		memmove (s->cmd, start, s->cmdLen);
		if (s->cmdLen >= CMD_SIZE - 1) // line too long
			s->cmdLen = 0;
	}
}

// network -> SD
static void pumpStor (Session *s)
{
	Xfer *x = &s->x;
	for (;;)
	{
		int i = x->cur;
		if (bufState (x, i) == B_BUSY)
			return; // all buffers are being written: wait for the SD card
		if (x->st[i] == B_DONE)
		{
			if (R_FAILED (x->rc[i]))
			{
				finishData (s, false, "451 SD card write error");
				return;
			}
			x->st[i] = B_FREE;
		}
		if (x->st[i] == B_FREE)
		{
			x->st[i]  = B_FILL;
			x->len[i] = 0;
			x->off[i] = x->pos;
		}

		int n = recv (s->data, x->buf[i] + x->len[i], CHUNK - x->len[i], 0);
		if (n > 0)
		{
			x->len[i] += n;
			x->pos += n;
			x->done += n;
			if (x->len[i] == CHUNK)
			{
				submit (x, i);
				x->cur = (i + 1) % NBUF;
			}
			continue;
		}
		if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
			return;

		// n == 0: upload complete. n < 0: connection lost
		if (x->len[i] > 0)
			submit (x, i);
		else
			x->st[i] = B_FREE;
		if (n == 0)
			finishData (s, true, "226 Transfer complete");
		else
			finishData (s, false, "426 Connection lost");
		return;
	}
}

// SD -> network
static void pumpRetr (Session *s)
{
	Xfer *x = &s->x;
	for (;;)
	{
		int i = x->cur;
		if (x->st[i] == B_FREE)
		{
			finishData (s, true, "226 Transfer complete");
			return;
		}
		if (bufState (x, i) != B_DONE)
			return; // waiting for the SD card
		if (R_FAILED (x->rc[i]))
		{
			finishData (s, false, "451 SD card read error");
			return;
		}
		if (x->len[i] == 0)
		{
			finishData (s, true, "226 Transfer complete");
			return;
		}

		int n = send (s->data, x->buf[i] + x->sendPos, x->len[i] - x->sendPos, 0);
		if (n < 0)
		{
			if (errno != EAGAIN && errno != EWOULDBLOCK)
				finishData (s, false, "426 Connection lost");
			return;
		}
		x->sendPos += n;
		x->done += n;
		if (x->sendPos < x->len[i])
			continue;

		// buffer fully sent: refill it with the next chunk
		x->sendPos = 0;
		x->cur     = (i + 1) % NBUF;
		if (x->pos < x->size)
		{
			x->off[i] = x->pos;
			x->pos += CHUNK;
			submit (x, i);
		}
		else
			x->st[i] = B_FREE;
	}
}

static void pumpList (Session *s)
{
	while (s->listPos < s->listLen)
	{
		int n = send (s->data, s->list + s->listPos, s->listLen - s->listPos, 0);
		if (n < 0)
		{
			if (errno != EAGAIN && errno != EWOULDBLOCK)
				finishData (s, false, "426 Connection lost");
			return;
		}
		s->listPos += n;
	}
	finishData (s, true, "226 Done");
}

// ---------------------------------------------------------------------------
// network lifecycle

static void netDown (void)
{
	for (int i = 0; i < MAX_SESSIONS; ++i)
		closeSession (&s_sess[i]);
	for (int i = 0; i < MAX_PENDING; ++i)
		closeNow (&s_pending[i].fd);
	closeNow (&s_listen);

	if (s_ndmLocked)
	{
		NDMU_UnlockState ();
		NDMU_LeaveExclusiveState ();
		s_ndmLocked = false;
	}
	if (s_netUp)
		socExit ();
	s_netUp = false;
	s_ip  = 0;
}

static bool wifiConnected (void)
{
	u32 wifi = 0;
	return R_SUCCEEDED (ACU_GetWifiStatus (&wifi)) && wifi != 0;
}

static void netUp (void)
{
	if (!wifiConnected ())
	{
		snprintf (s_status, sizeof (s_status), "Waiting for Wi-Fi...");
		return;
	}
	if (R_FAILED (socInit (s_socBuf, SOC_BUFSIZE)))
	{
		snprintf (s_status, sizeof (s_status), "socInit failed, retrying...");
		return;
	}
	s_netUp = true;
	s_ip  = gethostid ();

	// keep StreetPass/SpotPass from using the Wi-Fi radio while we serve
	if (R_SUCCEEDED (NDMU_EnterExclusiveState (NDM_EXCLUSIVE_STATE_INFRASTRUCTURE)))
	{
		if (R_SUCCEEDED (NDMU_LockState ()))
			s_ndmLocked = true;
		else
			NDMU_LeaveExclusiveState ();
	}

	struct sockaddr_in addr = {0};
	addr.sin_family         = AF_INET;
	addr.sin_port           = htons (CTRL_PORT);
	addr.sin_addr.s_addr    = s_ip;
	s_listen                = socket (AF_INET, SOCK_STREAM, 0);
	if (s_ip == 0 || s_ip == INADDR_BROADCAST || s_listen < 0 ||
	    bind (s_listen, (struct sockaddr *)&addr, sizeof (addr)) != 0 || listen (s_listen, MAX_SESSIONS) != 0)
	{
		netDown ();
		snprintf (s_status, sizeof (s_status), "Network not ready, retrying...");
		return;
	}
	setNonBlocking (s_listen);
	snprintf (s_status, sizeof (s_status), "Ready");
}

static void onApt (APT_HookType type, void *param)
{
	(void)param;
	switch (type)
	{
	case APTHOOK_ONSLEEP:
		// lid closed: Wi-Fi goes away, drop everything cleanly
		netDown ();
		setBacklight (GSPLCD_SCREEN_BOTH, true);
		break;
	case APTHOOK_ONSUSPEND:
		setBacklight (GSPLCD_SCREEN_BOTH, true); // so the HOME menu is visible
		break;
	case APTHOOK_ONWAKEUP:
	case APTHOOK_ONRESTORE:
		applyBacklight ();
		s_nextNetCheck = 0;
		break;
	default:
		break;
	}
}

static void acceptClient (void)
{
	struct sockaddr_in addr;
	socklen_t len = sizeof (addr);
	int fd        = accept (s_listen, (struct sockaddr *)&addr, &len);
	if (fd < 0)
		return;

	for (int i = 0; i < MAX_SESSIONS; ++i)
	{
		Session *s = &s_sess[i];
		if (s->ctrl >= 0)
			continue;
		s->ctrl = fd;
		s->peer = addr.sin_addr.s_addr;
		setNonBlocking (fd);
		s->cmdLen  = 0;
		s->rest    = 0;
		s->rnfr[0] = 0;
		s->lastCmd = osGetTime ();
		strcpy (s->cwd, "/");
		reply (s, "220 Syna 3DS Tools ready\r\n");
		return;
	}
	char const msg[] = "421 Too many connections\r\n";
	send (fd, msg, sizeof (msg) - 1, 0);
	close (fd);
}

// one iteration of the network loop
static void serve (void)
{
	struct pollfd fds[1 + MAX_SESSIONS * 2 + MAX_PENDING];
	void *owner[1 + MAX_SESSIONS * 2 + MAX_PENDING];
	int kind[1 + MAX_SESSIONS * 2 + MAX_PENDING]; // 0 listen, 1 ctrl, 2 pasv, 3 data, 4 pending
	int nfds     = 0;
	bool diskWait = false;
	u64 now      = osGetTime ();

#define ADD(fd_, ev_, s_, k_) \
	(fds[nfds] = (struct pollfd){(fd_), (ev_), 0}, owner[nfds] = (s_), kind[nfds++] = (k_))

	ADD (s_listen, POLLIN, NULL, 0);
	for (int i = 0; i < MAX_SESSIONS; ++i)
	{
		Session *s = &s_sess[i];
		if (s->ctrl < 0)
		{
			// control connection died while replying: release the rest
			if (s->data >= 0 || s->pasv >= 0 || s->xferOpen || s->list || s->x.buf[0])
				closeSession (s);
			continue;
		}
		if (s->mode == X_NONE && now - s->lastCmd > IDLE_TIMEOUT)
		{
			closeSession (s);
			continue;
		}
		ADD (s->ctrl, POLLIN, s, 1);
		if (s->pasv >= 0)
		{
			if (now - s->dataSince > DATA_TIMEOUT)
			{
				closeNow (&s->pasv);
				if (s->mode != X_NONE)
					finishData (s, false, "425 No data connection");
			}
			else
				ADD (s->pasv, POLLIN, s, 2);
		}
		if (s->data >= 0 && s->mode != X_NONE)
		{
			if (!s->started)
				startData (s);
			if (s->mode == X_STOR)
			{
				int c = s->x.cur;
				if (bufState (&s->x, c) == B_BUSY)
					diskWait = true;
				else
					ADD (s->data, POLLIN, s, 3);
			}
			else if (s->mode == X_RETR && s->x.st[s->x.cur] != B_FREE && bufState (&s->x, s->x.cur) != B_DONE)
				diskWait = true;
			else
				ADD (s->data, POLLOUT, s, 3);
		}
	}
	for (int i = 0; i < MAX_PENDING; ++i)
	{
		if (s_pending[i].fd < 0)
			continue;
		if (now > s_pending[i].deadline)
			closeNow (&s_pending[i].fd);
		else
			ADD (s_pending[i].fd, POLLIN, &s_pending[i], 4);
	}
#undef ADD

	int rc = poll (fds, nfds, diskWait ? 1 : 30);
	if (rc < 0)
	{
		// the network went away underneath us
		netDown ();
		return;
	}
	if (rc == 0)
		return;

	for (int k = 0; k < nfds; ++k)
	{
		short ev = fds[k].revents;
		if (!ev)
			continue;
		Session *s = owner[k];
		switch (kind[k])
		{
		case 0:
			acceptClient ();
			break;
		case 1:
			if (s->ctrl == fds[k].fd)
				readCommands (s);
			break;
		case 2:
			if (s->pasv == fds[k].fd)
			{
				int fd = accept (s->pasv, NULL, NULL);
				if (fd >= 0)
				{
					closeNow (&s->pasv);
					closeNow (&s->data);
					s->data = fd;
					setNonBlocking (fd);
					setBuffers (fd);
				}
			}
			break;
		case 3:
			if (s->data != fds[k].fd || s->ctrl < 0)
				break;
			if (!s->started)
				startData (s);
			if (s->mode == X_STOR)
				pumpStor (s);
			else if (s->mode == X_RETR)
				pumpRetr (s);
			else if (s->mode != X_NONE)
				pumpList (s);
			break;
		case 4:
		{
			Pending *pd = owner[k];
			char junk[256];
			if (pd->fd == fds[k].fd && recv (pd->fd, junk, sizeof (junk), 0) <= 0)
				closeNow (&pd->fd);
			break;
		}
		}
	}
}

// ---------------------------------------------------------------------------
// CIA install, following FBI (source/fbi/action/installcias.c)

#define INSTALL_CHUNK (256 * 1024)

static struct
{
	char path[PATH_SIZE];
	char err[48];
	volatile u64 done;
	u64 total;
	volatile Result rc;
	volatile bool finished;
	bool running;
	Thread thread;
} s_inst;

static char s_ciaMsg[48];
static u64 s_ciaMsgTime;

// FBI fs_get_title_destination: DSiWare, system and DLP titles go to NAND
static FS_MediaType titleDestination (u64 titleId)
{
	u16 platform  = (u16)((titleId >> 48) & 0xFFFF);
	u16 category  = (u16)((titleId >> 32) & 0xFFFF);
	u8 variation  = (u8)(titleId & 0xFF);
	bool nand     = platform == 0x0003 ||
	            (platform == 0x0004 && ((category & 0x8011) != 0 || (category == 0x0000 && variation == 0x02)));
	return nand ? MEDIATYPE_NAND : MEDIATYPE_SD;
}

// FBI cia_get_title_id + tmd_get_title_id
static bool ciaTitleId (u8 const *cia, u32 size, u64 *titleId)
{
	static u32 const sigSizes[6] = {0x240, 0x140, 0x80, 0x240, 0x140, 0x80};
	if (size < 0x10)
		return false;
	u32 hdr[4];
	memcpy (hdr, cia, sizeof (hdr));
	u32 offset = ((hdr[0] + 0x3F) & ~0x3F) + ((hdr[2] + 0x3F) & ~0x3F) + ((hdr[3] + 0x3F) & ~0x3F);
	if (offset + 4 > size || cia[offset + 3] >= 6)
		return false;
	u32 pos = offset + sigSizes[cia[offset + 3]] + 0x4C;
	if (pos + 8 > size)
		return false;
	u64 id = 0;
	for (int i = 0; i < 8; ++i)
		id = (id << 8) | cia[pos + i];
	*titleId = id;
	return true;
}

static void installThread (void *arg)
{
	(void)arg;
	Result r     = 0;
	Handle src   = 0, dst = 0;
	bool started = false, opened = false;
	u64 titleId  = 0;
	u16 wpath[PATH_SIZE];
	FS_Path p;
	u8 *buf = memalign (64, INSTALL_CHUNK);

#define FAIL(msg) (r = R_FAILED (r) ? r : -1, snprintf (s_inst.err, sizeof (s_inst.err), "%s", msg))

	if (!buf)
		FAIL ("Out of memory");
	else if (!toFsPath (s_inst.path, wpath, &p) || R_FAILED (r = FSUSER_OpenFile (&src, s_sd, p, FS_OPEN_READ, 0)))
		FAIL ("Can't open the file");
	else
	{
		opened = true;
		u64 off = 0;
		if (s_inst.total == 0)
			FAIL ("Empty file");
		while (R_SUCCEEDED (r) && off < s_inst.total)
		{
			u32 n = 0;
			if (R_FAILED (r = FSFILE_Read (src, &n, off, buf, INSTALL_CHUNK)) || n == 0)
			{
				FAIL ("SD card read error");
				break;
			}
			if (!started)
			{
				if (!ciaTitleId (buf, n, &titleId))
				{
					FAIL ("Not a valid CIA file");
					break;
				}
				FS_MediaType dest = titleDestination (titleId);
				bool n3ds         = false;
				if (R_SUCCEEDED (APT_CheckNew3DS (&n3ds)) && !n3ds && ((titleId >> 28) & 0xF) == 2)
				{
					FAIL ("This title is for New 3DS only");
					break;
				}
				// like FBI: replace an existing install, except when updating ourselves
				u64 selfId          = 0;
				FS_MediaType selfMt = MEDIATYPE_NAND;
				if (envIsHomebrew () ||
				    R_FAILED (APT_GetAppletInfo ((NS_APPID)envGetAptAppId (), &selfId, (u8 *)&selfMt, NULL, NULL, NULL)) ||
				    titleId != selfId || dest != selfMt)
				{
					AM_DeleteTitle (dest, titleId);
					AM_DeleteTicket (titleId);
					if (dest == MEDIATYPE_SD)
						AM_QueryAvailableExternalTitleDatabase (NULL);
				}
				if (R_FAILED (r = AM_StartCiaInstall (dest, &dst)))
				{
					FAIL ("Install could not start");
					break;
				}
				started = true;
			}
			u32 w = 0;
			if (R_FAILED (r = FSFILE_Write (dst, &w, off, buf, n, 0)))
			{
				FAIL ("Install write failed");
				break;
			}
			off += n;
			s_inst.done = off;
		}
	}

	if (started)
	{
		if (R_SUCCEEDED (r))
		{
			if (R_FAILED (r = AM_FinishCiaInstall (dst)))
				FAIL ("Install could not finish");
			else if ((titleId & 0xFFFFFFF) == 0x0000002 && R_FAILED (r = AM_InstallFirm (titleId)))
				FAIL ("FIRM install failed");
		}
		else
			AM_CancelCIAInstall (dst);
	}
	if (opened)
		FSFILE_Close (src);
	if (R_SUCCEEDED (r))
		FSUSER_DeleteFile (s_sd, p);
	free (buf);
#undef FAIL

	s_inst.rc = r;
	__atomic_store_n (&s_inst.finished, true, __ATOMIC_RELEASE);
}

static char const *baseName (char const *path)
{
	char const *slash = strrchr (path, '/');
	return slash ? slash + 1 : path;
}

// installs wait for transfers and deletions: they share the SD card
static bool fsBusy (void)
{
	if (osGetTime () < s_fsBusyUntil)
		return true;
	for (int i = 0; i < MAX_SESSIONS; ++i)
		if (s_sess[i].ctrl >= 0 && (s_sess[i].mode == X_STOR || s_sess[i].mode == X_RETR || s_sess[i].batchFiles))
			return true;
	return false;
}

static void startInstall (void)
{
	if (s_inst.running || s_ciaSel >= s_ciaCount)
		return;
	if (fsBusy ())
	{
		snprintf (s_ciaMsg, sizeof (s_ciaMsg), "Busy: wait for the transfer to end");
		s_ciaMsgTime  = osGetTime ();
		s_bottomDirty = true;
		return;
	}
	snprintf (s_inst.path, sizeof (s_inst.path), "%s", s_cias[s_ciaSel].path);
	s_inst.total    = s_cias[s_ciaSel].size;
	s_inst.done     = 0;
	s_inst.err[0]   = 0;
	s_inst.finished = false;
	s_inst.running  = true;
	s_ciaMsg[0]     = 0;
	aptSetSleepAllowed (false); // never sleep in the middle of an install
	s_inst.thread = threadCreate (installThread, NULL, 0x4000, 0x30, -2, false);
	if (!s_inst.thread)
	{
		s_inst.running = false;
		aptSetSleepAllowed (true);
		snprintf (s_ciaMsg, sizeof (s_ciaMsg), "Could not start the install");
		s_ciaMsgTime = osGetTime ();
	}
	s_bottomDirty = true;
}

static void pollInstall (void)
{
	if (!s_inst.running || !__atomic_load_n (&s_inst.finished, __ATOMIC_ACQUIRE))
		return;
	threadJoin (s_inst.thread, U64_MAX);
	threadFree (s_inst.thread);
	s_inst.running = false;
	aptSetSleepAllowed (true);
	if (R_SUCCEEDED (s_inst.rc))
	{
		ciaRemove (s_inst.path);
		snprintf (s_ciaMsg, sizeof (s_ciaMsg), "Installed %.30s", baseName (s_inst.path));
	}
	else
		snprintf (s_ciaMsg, sizeof (s_ciaMsg), "%.30s (%08lX)", s_inst.err, (unsigned long)s_inst.rc);
	s_ciaMsgTime  = osGetTime ();
	s_bottomDirty = true;
}

// ---------------------------------------------------------------------------
// SITE STATUS: what the other connections are doing, for agents and scripts

typedef struct
{
	bool active;                   // a file transfer is flowing right now
	u64 done, size, bps;           // current file (size 0 = unknown)
	long long eta;                 // seconds, -1 = unknown
	u64 batchDone, batchBytes;     // whole announced job
	long long batchEta;
} Progress;

static Progress progressOf (Session const *s, u64 now)
{
	Progress p   = {0};
	p.eta        = -1;
	p.batchEta   = -1;
	p.active     = s->started && (s->mode == X_STOR || s->mode == X_RETR);
	if (p.active)
	{
		u64 ms = now - s->xferStart;
		p.done = s->xferBase + s->x.done;
		p.size = s->xferTotal;
		p.bps  = ms >= 500 ? s->x.done * 1000 / ms : 0;
		if (p.size >= p.done && p.bps)
			p.eta = (long long)((p.size - p.done) / p.bps);
	}
	if (s->batchFiles)
	{
		u64 ms       = now - s->batchStart;
		p.batchDone  = s->batchBytesDone + (p.active ? s->x.done : 0);
		p.batchBytes = s->batchBytes;
		u64 bps      = ms >= 500 ? p.batchDone * 1000 / ms : 0;
		if (p.batchBytes >= p.batchDone && bps)
			p.batchEta = (long long)((p.batchBytes - p.batchDone) / bps);
		else if (!strcmp (s->batchKind, "delete") && s->batchFilesDone && ms >= 500)
			p.batchEta = (long long)((u64)(s->batchFiles - s->batchFilesDone) * ms / s->batchFilesDone / 1000);
	}
	return p;
}

static void siteStatus (Session *self)
{
	u64 now     = osGetTime ();
	bool busy   = s_inst.running || now < s_fsBusyUntil;
	int clients = 0;
	for (int i = 0; i < MAX_SESSIONS; ++i)
	{
		Session *s = &s_sess[i];
		if (s->ctrl < 0)
			continue;
		++clients;
		if (s != self && (s->mode == X_STOR || s->mode == X_RETR || s->batchFiles))
			busy = true;
	}
	reply (self, "211-Status\r\n");
	reply (self,
	    " busy=%d idle_ms=%llu installing=%d connections=%d version=" VERSION "\r\n",
	    busy,
	    busy || now < s_lastActivity ? 0ull : (unsigned long long)(now - s_lastActivity),
	    s_inst.running,
	    clients);
	for (int i = 0; i < MAX_SESSIONS; ++i)
	{
		Session *s = &s_sess[i];
		if (s->ctrl < 0 || s == self || !(s->mode == X_STOR || s->mode == X_RETR || s->batchFiles))
			continue;
		Progress p  = progressOf (s, now);
		u8 const *a = (u8 const *)&s->peer;
		char batch[160] = "";
		if (s->batchFiles)
			snprintf (batch,
			    sizeof (batch),
			    " batch=%s batch_files_done=%lu batch_files=%lu batch_bytes_done=%llu batch_bytes=%llu batch_eta=%lld",
			    s->batchKind,
			    (unsigned long)s->batchFilesDone,
			    (unsigned long)s->batchFiles,
			    (unsigned long long)p.batchDone,
			    (unsigned long long)p.batchBytes,
			    p.batchEta);
		reply (self,
		    " transfer peer=%u.%u.%u.%u dir=%s done=%llu size=%llu bps=%llu eta=%lld%s file=%s\r\n",
		    a[0], a[1], a[2], a[3],
		    !p.active ? "none" : s->mode == X_STOR ? "up" : "down",
		    (unsigned long long)p.done,
		    (unsigned long long)p.size,
		    (unsigned long long)p.bps,
		    p.eta,
		    batch,
		    p.active ? s->path : "");
	}
	if (s_inst.running)
		reply (self,
		    " install done=%llu size=%llu file=%s\r\n",
		    (unsigned long long)s_inst.done,
		    (unsigned long long)s_inst.total,
		    s_inst.path);
	reply (self, "211 End\r\n");
}

static void ciaInput (u32 down)
{
	if (s_inst.running || !s_ciaCount)
		return;
	if (down & KEY_UP)
		s_ciaSel = (s_ciaSel + s_ciaCount - 1) % s_ciaCount;
	else if (down & KEY_DOWN)
		s_ciaSel = (s_ciaSel + 1) % s_ciaCount;
	else if (down & KEY_A)
		startInstall ();
	else
		return;
	s_bottomDirty = true;
}

// ---------------------------------------------------------------------------
// screen

static void formatSize (char *out, size_t n, u64 v)
{
	if (v >= 1024 * 1024)
		snprintf (out, n, "%.1f MB", v / (1024.0 * 1024.0));
	else
		snprintf (out, n, "%.1f KB", v / 1024.0);
}

static PrintConsole s_topConsole, s_bottomConsole;

static void drawBottom (void)
{
	u64 now      = osGetTime ();
	bool showMsg = s_ciaMsg[0] && now - s_ciaMsgTime < 8000;
	bool on      = s_ciaCount > 0 || s_inst.running || showMsg;
	if (on != s_bottomOn)
	{
		s_bottomOn = on;
		setBacklight (GSPLCD_SCREEN_BOTTOM, on);
	}
	static bool lastBusy;
	bool busy = fsBusy ();
	if (busy != lastBusy)
	{
		lastBusy      = busy;
		s_bottomDirty = true;
	}
	if (!on || (!s_bottomDirty && !s_inst.running && !showMsg))
		return;
	s_bottomDirty = false;

	consoleSelect (&s_bottomConsole);
	printf ("\x1b[1;1H\x1b[36m CIA files ready to install\x1b[0m\x1b[K\n");
	printf (" --------------------------------------\x1b[K\n");
	enum { ROWS = 20 };
	int first = s_ciaSel >= ROWS ? s_ciaSel - ROWS + 1 : 0;
	for (int row = 0; row < ROWS; ++row)
	{
		int i = first + row;
		if (i >= s_ciaCount)
		{
			printf ("\x1b[K\n");
			continue;
		}
		char size[16];
		formatSize (size, sizeof (size), s_cias[i].size);
		bool sel = i == s_ciaSel;
		printf ("%s%c %-27.27s %9s\x1b[0m\x1b[K\n", sel ? "\x1b[33m" : "", sel ? '>' : ' ', baseName (s_cias[i].path), size);
	}

	printf ("\x1b[K\n");
	if (s_inst.running)
	{
		char done[16], total[16];
		formatSize (done, sizeof (done), s_inst.done);
		formatSize (total, sizeof (total), s_inst.total);
		printf (" Installing %.27s\x1b[K\n", baseName (s_inst.path));
		printf ("   %s / %s  %u%%\x1b[K\n", done, total, s_inst.total ? (unsigned)(s_inst.done * 100 / s_inst.total) : 0);
	}
	else
		printf ("\x1b[K\n\x1b[K\n");
	printf (" %s%.38s\x1b[0m\x1b[K\n", showMsg ? (strncmp (s_ciaMsg, "Installed", 9) == 0 ? "\x1b[32m" : "\x1b[31m") : "", showMsg ? s_ciaMsg : "");
	if (busy && !s_inst.running)
		printf ("\x1b[30;1H\x1b[33m A: install (waits for transfers)\x1b[0m\x1b[K");
	else
		printf ("\x1b[30;1H A: install + delete   Up/Down: select\x1b[K");
	consoleSelect (&s_topConsole);
}

static void draw (void)
{
	u64 now = osGetTime ();
	drawBottom ();
	if (now - s_lastTime < 500)
		return;

	int clients = 0; // distinct PCs: the client opens a browse and a transfer connection
	u64 bytes   = 0;
	Session *active = NULL;
	for (int i = 0; i < MAX_SESSIONS; ++i)
	{
		if (s_sess[i].ctrl < 0)
			continue;
		bool seen = false;
		for (int j = 0; j < i; ++j)
			seen |= s_sess[j].ctrl >= 0 && s_sess[j].peer == s_sess[i].peer;
		clients += !seen;
		bytes += s_sess[i].x.done;
		if (s_sess[i].name[0] && (s_sess[i].mode == X_STOR || s_sess[i].mode == X_RETR))
			active = &s_sess[i];
	}
	u32 rate = 0;
	if (active && bytes >= s_lastBytes)
		rate = (u32)((bytes - s_lastBytes) * 1000 / (now - s_lastTime));
	s_rate      = (s_rate * 1 + rate * 3) / 4;
	s_lastBytes = bytes;
	s_lastTime  = now;

	if (s_netUp && s_listen >= 0)
		snprintf (s_status, sizeof (s_status), clients ? "%d PC(s) connected" : "Ready", clients);

	// every line on a fixed row, cleared to the end and kept under the 50
	// column width (a wrapped line used to leave old text behind)
	u8 const *a = (u8 const *)&s_ip;
	printf ("\x1b[1;1H\x1b[31m Syna 3DS Tools\x1b[0m v" VERSION "\x1b[K");
	printf ("\x1b[2;1H ------------------------------------------------\x1b[K");
	if (s_ip)
		printf ("\x1b[4;1H IP   : \x1b[33m%u.%u.%u.%u\x1b[0m\x1b[K", a[0], a[1], a[2], a[3]);
	else
		printf ("\x1b[4;1H IP   : -\x1b[K");
	printf ("\x1b[5;1H Port : \x1b[33m%u\x1b[0m\x1b[K", CTRL_PORT);
	printf ("\x1b[7;1H Status: %.40s\x1b[K", s_status);

	if (active)
	{
		char done[32], total[32], speed[32];
		formatSize (done, sizeof (done), active->x.done);
		formatSize (speed, sizeof (speed), s_rate);
		printf ("\x1b[9;1H %s %.36s\x1b[K", active->mode == X_STOR ? "Receiving" : "Sending  ", active->name);
		if (active->mode == X_RETR)
		{
			formatSize (total, sizeof (total), active->x.size);
			printf ("\x1b[10;1H   %s / %s   %s/s\x1b[K", done, total, speed);
		}
		else
			printf ("\x1b[10;1H   %s   %s/s\x1b[K", done, speed);
	}
	else
		printf ("\x1b[9;1H\x1b[K\x1b[10;1H\x1b[K");

	// whole job announced by the PC (SITE BATCH): files left and time left
	Session *batch = NULL;
	for (int i = 0; i < MAX_SESSIONS && !batch; ++i)
		if (s_sess[i].ctrl >= 0 && s_sess[i].batchFiles)
			batch = &s_sess[i];
	if (batch)
	{
		Progress p = progressOf (batch, now);
		char left[48] = "";
		if (p.batchEta >= 0)
			snprintf (left, sizeof (left), ", %lld:%02lld left", p.batchEta / 60, p.batchEta % 60);
		printf ("\x1b[11;1H %s %lu/%lu files%s\x1b[K",
		    !strcmp (batch->batchKind, "delete") ? "Deleting" : !strcmp (batch->batchKind, "down") ? "Sending  " : "Receiving",
		    (unsigned long)batch->batchFilesDone,
		    (unsigned long)batch->batchFiles,
		    left);
	}
	else
		printf ("\x1b[11;1H\x1b[K");

	printf ("\x1b[26;1H START: exit   SELECT: top screen on/off\x1b[K");
	printf ("\x1b[27;1H Close the lid to sleep (Wi-Fi turns off)\x1b[K");
}

// ---------------------------------------------------------------------------

int main (int argc, char **argv)
{
	(void)argc;
	(void)argv;

	osSetSpeedupEnable (true);
	acInit ();
	ndmuInit ();
	gfxInitDefault ();
	amInit ();
	AM_InitializeExternalTitleDatabase (false); // as FBI does at startup
	consoleInit (GFX_BOTTOM, &s_bottomConsole);
	consoleInit (GFX_TOP, &s_topConsole);

	for (int i = 0; i < MAX_SESSIONS; ++i)
		s_sess[i].ctrl = s_sess[i].pasv = s_sess[i].data = -1;
	for (int i = 0; i < MAX_PENDING; ++i)
		s_pending[i].fd = -1;

	s_socBuf = memalign (SOC_ALIGN, SOC_BUFSIZE);
	if (!s_socBuf || R_FAILED (FSUSER_OpenArchive (&s_sd, ARCHIVE_SDMC, fsMakePath (PATH_EMPTY, ""))))
	{
		printf ("Init failed (memory or SD card).\nPress START to exit.\n");
		while (aptMainLoop ())
		{
			hidScanInput ();
			if (hidKeysDown () & KEY_START)
				break;
			gspWaitForVBlank ();
		}
		gfxExit ();
		return 0;
	}

	LightLock_Init (&s_qLock);
	LightSemaphore_Init (&s_qSem, 0, QUEUE_SIZE);
	// New 3DS: put the disk thread on the extra core; fall back to the app core
	s_diskThread = threadCreate (diskThread, NULL, 0x4000, 0x2F, 2, false);
	if (!s_diskThread)
		s_diskThread = threadCreate (diskThread, NULL, 0x4000, 0x2F, -2, false);

	aptHook (&s_hook, onApt, NULL);
	applyBacklight ();

	while (aptMainLoop ())
	{
		hidScanInput ();
		u32 down = hidKeysDown ();
		pollInstall ();
		if (down & KEY_START)
		{
			if (!s_inst.running)
				break;
			snprintf (s_ciaMsg, sizeof (s_ciaMsg), "Wait for the install to finish");
			s_ciaMsgTime  = osGetTime ();
			s_bottomDirty = true;
		}
		ciaInput (down);
		if (down & KEY_SELECT)
		{
			s_backlight = !s_backlight;
			setBacklight (GSPLCD_SCREEN_TOP, s_backlight);
		}

		u64 now = osGetTime ();
		if (now >= s_nextNetCheck)
		{
			s_nextNetCheck = now + 1000;
			if (!s_netUp)
				netUp ();
			else if (!wifiConnected ())
			{
				netDown ();
				snprintf (s_status, sizeof (s_status), "Wi-Fi lost, waiting...");
			}
		}

		if (s_netUp)
			serve ();
		else
			svcSleepThread (50000000); // 50 ms

		draw ();
	}

	netDown ();

	// stop the disk thread
	LightLock_Lock (&s_qLock);
	s_queue[s_qHead] = (Job){NULL, 0};
	s_qHead          = (s_qHead + 1) % QUEUE_SIZE;
	LightLock_Unlock (&s_qLock);
	LightSemaphore_Release (&s_qSem, 1);
	threadJoin (s_diskThread, U64_MAX);
	threadFree (s_diskThread);

	aptUnhook (&s_hook);
	setBacklight (GSPLCD_SCREEN_BOTH, true);
	FSUSER_CloseArchive (s_sd);
	free (s_socBuf);
	ndmuExit ();
	amExit ();
	acExit ();
	gfxExit ();
	return 0;
}
