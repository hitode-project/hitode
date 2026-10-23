/* Host services behind shim.h.
 *
 * This file is compiled without the shim, so the calls here reach the real
 * system functions. The hitode_host_* functions come from the Rust facade:
 * the runtime owns waiting, time and the server's lifetime. */

#ifndef __APPLE__
#define _GNU_SOURCE /* unshare */
#include <sched.h>
#endif
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <string.h>
#include <time.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/resource.h>
#ifdef __APPLE__
#include <mach/mach.h>
#include <mach/mach_time.h>
#endif

#define TICKS_PER_SEC 10000000

enum hitode_clock { HITODE_MONOTONIC, HITODE_UNIX };

extern void hitode_host_wait( int timeout );
extern uint64_t hitode_host_ticks( enum hitode_clock clock );
extern void hitode_host_exit( int status ) __attribute__((noreturn));
extern const char *hitode_host_getenv( const char *name );
extern const char *hitode_host_executable( void );
extern const char *hitode_host_server_root( void );

static int current_dir = AT_FDCWD;

/* Wine constructs its per-user base here, under /tmp or, on Android, the
 * configuration directory; the caller owns that directory. */
int hitode_asprintf( char **str, const char *format, ... )
{
    va_list args;
    int ret;

    if (!strcmp( format, "/tmp/.wine-%u" ) || !strcmp( format, "%s/.wineserver" ))
    {
        *str = strdup( hitode_host_server_root() );
        return *str ? strlen( *str ) : -1;
    }
    va_start( args, format );
    ret = vasprintf( str, format, args );
    va_end( args );
    return ret;
}

int hitode_chdir( const char *path )
{
    int fd = openat( current_dir, path, O_RDONLY | O_DIRECTORY | O_CLOEXEC );

    if (fd == -1) return -1;
    if (current_dir != AT_FDCWD) close( current_dir );
    current_dir = fd;
    return 0;
}

int hitode_fchdir( int fd )
{
    struct stat st;
    int copy;

    if (fstat( fd, &st ) == -1) return -1;
    if (!S_ISDIR(st.st_mode))
    {
        errno = ENOTDIR;
        return -1;
    }
    if ((copy = fcntl( fd, F_DUPFD_CLOEXEC, 0 )) == -1) return -1;
    if (current_dir != AT_FDCWD) close( current_dir );
    current_dir = copy;
    return 0;
}

int hitode_open( const char *path, int flags, ... )
{
    va_list args;
    mode_t mode = 0;

    if (flags & O_CREAT)
    {
        va_start( args, flags );
        mode = va_arg( args, int );
        va_end( args );
    }
    return openat( current_dir, path, flags, mode );
}

/* The server opens streams only for reading registry files. */
FILE *hitode_fopen( const char *path, const char *mode )
{
    int fd = openat( current_dir, path, O_RDONLY );
    FILE *file;

    if (fd == -1) return NULL;
    if (!(file = fdopen( fd, mode ))) close( fd );
    return file;
}

int hitode_stat( const char *path, struct stat *st )
{
    return fstatat( current_dir, path, st, 0 );
}

int hitode_lstat( const char *path, struct stat *st )
{
    return fstatat( current_dir, path, st, AT_SYMLINK_NOFOLLOW );
}

int hitode_mkdir( const char *path, mode_t mode )
{
    return mkdirat( current_dir, path, mode );
}

int hitode_unlink( const char *path )
{
    return unlinkat( current_dir, path, 0 );
}

int hitode_rmdir( const char *path )
{
    return unlinkat( current_dir, path, AT_REMOVEDIR );
}

int hitode_rename( const char *from, const char *to )
{
    return renameat( current_dir, from, current_dir, to );
}

int hitode_link( const char *from, const char *to )
{
    return linkat( current_dir, from, current_dir, to, AT_SYMLINK_FOLLOW );
}

ssize_t hitode_readlink( const char *path, char *buf, size_t size )
{
    return readlinkat( current_dir, path, buf, size );
}

int hitode_chmod( const char *path, mode_t mode )
{
    return fchmodat( current_dir, path, mode, 0 );
}

#ifndef __APPLE__
/* The name of the directory behind `fd`. */
static int dir_path( int fd, char *dir )
{
    char link[32];
    ssize_t len;

    snprintf( link, sizeof(link), "/proc/self/fd/%d", fd );
    if ((len = readlink( link, dir, PATH_MAX - 1 )) == -1) return -1;
    dir[len] = 0;
    return 0;
}
#endif

char *hitode_realpath( const char *path, char *resolved )
{
    char dir[PATH_MAX], *absolute, *ret;

#ifdef __linux__
    /* The server finds its data files relative to its own executable, so it
     * is given a path inside the Wine installation, which need not exist. */
    if (!strcmp( path, "/proc/self/exe" ))
    {
        const char *exe = hitode_host_executable();
        if (!resolved) return strdup( exe );
        if (strlen( exe ) >= PATH_MAX)
        {
            errno = ENAMETOOLONG;
            return NULL;
        }
        return strcpy( resolved, exe );
    }
#endif
    if (path[0] == '/' || current_dir == AT_FDCWD) return realpath( path, resolved );
#ifdef __APPLE__
    if (fcntl( current_dir, F_GETPATH, dir ) == -1) return NULL;
#else
    if (dir_path( current_dir, dir ) == -1) return NULL;
#endif
    if (asprintf( &absolute, "%s/%s", dir, path ) == -1) return NULL;
    ret = realpath( absolute, resolved );
    free( absolute );
    return ret;
}

#ifdef __APPLE__
extern int pthread_fchdir_np( int fd );
#endif

/* There is no bindat/connectat. Resolve relative Unix socket names on a
 * helper thread with its own working directory, so neither the process nor
 * the caller's thread cwd changes, and the name fits sun_path however long
 * the directory's path is. */
struct socket_call
{
    int (*func)( int, const struct sockaddr *, socklen_t );
    int dir, fd;
    const struct sockaddr *addr;
    socklen_t len;
    int ret, error;
};

static void *socket_call_thread( void *arg )
{
    struct socket_call *call = arg;

#ifdef __APPLE__
    call->ret = pthread_fchdir_np( call->dir );
#else
    call->ret = unshare( CLONE_FS );
    if (!call->ret) call->ret = fchdir( call->dir );
#endif
    if (!call->ret) call->ret = call->func( call->fd, call->addr, call->len );
    call->error = errno;
    return NULL;
}

static int socket_call_at( struct socket_call *call )
{
    pthread_t thread;
    int error;

    if ((error = pthread_create( &thread, NULL, socket_call_thread, call )))
    {
        errno = error;
        return -1;
    }
    pthread_join( thread, NULL );
    errno = call->error;
    return call->ret;
}

int hitode_bind( int fd, const struct sockaddr *addr, socklen_t len )
{
    if (addr->sa_family != AF_UNIX || ((const struct sockaddr_un *)addr)->sun_path[0] == '/'
        || current_dir == AT_FDCWD)
        return bind( fd, addr, len );
    struct socket_call call = { bind, current_dir, fd, addr, len, 0, 0 };
    return socket_call_at( &call );
}

int hitode_connect( int fd, const struct sockaddr *addr, socklen_t len )
{
    if (addr->sa_family != AF_UNIX || ((const struct sockaddr_un *)addr)->sun_path[0] == '/'
        || current_dir == AT_FDCWD)
        return connect( fd, addr, len );
    struct socket_call call = { connect, current_dir, fd, addr, len, 0, 0 };
    return socket_call_at( &call );
}

/* The server asks once for ready descriptors. With none ready it returns to
 * the runtime, which resumes it on new input or at the timeout; the server
 * then sees whatever is ready by then and computes its next wait itself. */
int hitode_poll( struct pollfd *fds, nfds_t count, int timeout )
{
    int ret = poll( fds, count, 0 );

    if (ret || !timeout) return ret;
    hitode_host_wait( timeout );
    return poll( fds, count, 0 );
}

char *hitode_getenv( const char *name )
{
    return (char *)hitode_host_getenv( name );
}

static void (*exit_funcs[8])(void);
static int exit_count;

int hitode_atexit( void (*func)(void) )
{
    if (exit_count == sizeof(exit_funcs) / sizeof(exit_funcs[0])) return -1;
    exit_funcs[exit_count++] = func;
    return 0;
}

void hitode_exit( int status )
{
    while (exit_count) exit_funcs[--exit_count]();
    hitode_host_exit( status );
}

/* Guest processes are not host processes: a pid the server holds never
 * names anything to signal, wait for or fork from. */
int hitode_kill( pid_t pid, int sig )
{
    errno = ESRCH;
    return -1;
}

pid_t hitode_fork( void )
{
    errno = ENOSYS;
    return -1;
}

pid_t hitode_setsid( void )
{
    errno = ENOSYS;
    return -1;
}

pid_t hitode_waitpid( pid_t pid, int *status, int options )
{
    errno = ECHILD;
    return -1;
}

/* The watchdog alarm guards against a hung client in a separate server
 * process; the runtime schedules every client, so it has nothing to watch. */
unsigned int hitode_alarm( unsigned int seconds )
{
    return 0;
}

int hitode_usleep( useconds_t usec )
{
    return 0;
}

int hitode_gettimeofday( struct timeval *tv, void *tz )
{
    uint64_t ticks = hitode_host_ticks( HITODE_UNIX );

    tv->tv_sec = ticks / TICKS_PER_SEC;
    tv->tv_usec = ticks % TICKS_PER_SEC / 10;
    return 0;
}

int hitode_clock_gettime( clockid_t clock, struct timespec *ts )
{
    uint64_t ticks = hitode_host_ticks( clock == CLOCK_REALTIME ? HITODE_UNIX : HITODE_MONOTONIC );

    ts->tv_sec = ticks / TICKS_PER_SEC;
    ts->tv_nsec = ticks % TICKS_PER_SEC * 100;
    return 0;
}

time_t hitode_time( time_t *t )
{
    time_t now = hitode_host_ticks( HITODE_UNIX ) / TICKS_PER_SEC;

    if (t) *t = now;
    return now;
}

/* Priorities and limits belong to the host process, which Hitode owns. */
int hitode_setpriority( int which, id_t who, int prio )
{
    errno = EPERM;
    return -1;
}

int hitode_setrlimit( int resource, const struct rlimit *limit )
{
    errno = EPERM;
    return -1;
}

#ifdef __APPLE__
/* Nanoseconds with a 1:1 timebase. */
uint64_t hitode_mach_continuous_time( void )
{
    return hitode_host_ticks( HITODE_MONOTONIC ) * 100;
}

kern_return_t hitode_mach_timebase_info( mach_timebase_info_t info )
{
    info->numer = 1;
    info->denom = 1;
    return KERN_SUCCESS;
}

/* Guest threads have no host thread ports; scheduling policy is the
 * runtime's. */
kern_return_t hitode_thread_policy_set( thread_act_t thread, thread_policy_flavor_t flavor,
                                        thread_policy_t policy, mach_msg_type_number_t count )
{
    return KERN_SUCCESS;
}

kern_return_t hitode_mach_port_extract_right( ipc_space_t task, mach_port_name_t name,
                                              mach_msg_type_name_t desired, mach_port_t *right,
                                              mach_msg_type_name_t *acquired )
{
    return KERN_INVALID_NAME;
}

kern_return_t hitode_mach_port_deallocate( ipc_space_t task, mach_port_name_t name )
{
    return KERN_SUCCESS;
}

/* The server finds its data files relative to its own executable, so it is
 * given a path inside the Wine installation instead of the host program's. */
int hitode_NSGetExecutablePath( char *buf, uint32_t *size )
{
    const char *path = hitode_host_executable();
    size_t len = strlen( path ) + 1;

    if (len > *size)
    {
        *size = len;
        return -1;
    }
    memcpy( buf, path, len );
    return 0;
}
#endif
