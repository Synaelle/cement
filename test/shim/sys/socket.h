// PC test shim: BSD socket calls used by the 3DS server, mapped onto Winsock
#pragma once
#include <winsock2.h>
#include <ws2tcpip.h>
#include <errno.h>
#include <fcntl.h>
#include <io.h>
#include <unistd.h>
#include <stddef.h>

#ifndef O_NONBLOCK
#define O_NONBLOCK 0x4000
#endif
#define F_GETFL 3
#define F_SETFL 4
#define SHUT_WR SD_SEND

static inline int shim_err (int rc)
{
	if (rc < 0)
		errno = WSAGetLastError () == WSAEWOULDBLOCK ? EAGAIN : ECONNRESET;
	return rc;
}

static inline int shim_socket (int a, int b, int c) { return (int)socket (a, b, c); }
static inline int shim_recv (int fd, void *p, size_t n, int f) { return shim_err (recv ((SOCKET)fd, (char *)p, (int)n, f)); }
static inline int shim_send (int fd, void const *p, size_t n, int f) { return shim_err (send ((SOCKET)fd, (char const *)p, (int)n, f)); }
static inline int shim_accept (int fd, struct sockaddr *a, socklen_t *l)
{
	SOCKET s = accept ((SOCKET)fd, a, l);
	return s == INVALID_SOCKET ? shim_err (-1) : (int)s;
}
static inline int shim_setsockopt (int fd, int lvl, int opt, void const *v, int len)
{
	return setsockopt ((SOCKET)fd, lvl, opt, (char const *)v, len);
}
static inline int shim_close (int fd) { return closesocket ((SOCKET)fd); }
static inline int shim_poll (struct pollfd *f, unsigned n, int t)
{
	// WSAPoll rejects an empty set, and sleeps are needed for the timeout
	if (n == 0)
	{
		Sleep (t);
		return 0;
	}
	return WSAPoll (f, n, t);
}
static inline int shim_fcntl (int fd, int cmd, int v)
{
	if (cmd == F_SETFL)
	{
		u_long nb = (v & O_NONBLOCK) != 0;
		return ioctlsocket ((SOCKET)fd, FIONBIO, &nb);
	}
	return 0;
}

#define socket shim_socket
#define recv shim_recv
#define send shim_send
#define accept shim_accept
#define setsockopt shim_setsockopt
#define close shim_close
#define poll shim_poll
#define fcntl shim_fcntl
#define bind(fd, a, l) bind ((SOCKET)(fd), a, l)
#define listen(fd, n) listen ((SOCKET)(fd), n)
#define shutdown(fd, h) shutdown ((SOCKET)(fd), h)
