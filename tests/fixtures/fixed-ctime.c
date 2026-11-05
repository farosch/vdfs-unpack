/*
 * LD_PRELOAD shim for regenerate.sh. mkfs.vdfs stores each source file's
 * st_ctim, which is set by the kernel to real time and cannot be changed
 * with touch. libfaketime does not intercept the fstat/lstat64 symbols that
 * mkfs imports, so this shim reports a fixed ctime (2020-01-01 00:00:00 UTC,
 * matching the faketime clock) using raw syscalls.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#define FIXED_CTIME_SEC 1577836800L

static int fix(int r, struct stat *st)
{
	if (r == 0) {
		st->st_ctim.tv_sec = FIXED_CTIME_SEC;
		st->st_ctim.tv_nsec = 0;
	}
	return r;
}

int stat(const char *path, struct stat *st)
{
	return fix(syscall(SYS_newfstatat, AT_FDCWD, path, st, 0), st);
}

int lstat(const char *path, struct stat *st)
{
	return fix(syscall(SYS_newfstatat, AT_FDCWD, path, st,
			   AT_SYMLINK_NOFOLLOW), st);
}

int fstat(int fd, struct stat *st)
{
	return fix(syscall(SYS_fstat, fd, st), st);
}

int fstatat(int dirfd, const char *path, struct stat *st, int flags)
{
	return fix(syscall(SYS_newfstatat, dirfd, path, st, flags), st);
}

int stat64(const char *path, struct stat64 *st)
{
	return stat(path, (struct stat *)st);
}

int lstat64(const char *path, struct stat64 *st)
{
	return lstat(path, (struct stat *)st);
}

int fstat64(int fd, struct stat64 *st)
{
	return fstat(fd, (struct stat *)st);
}
