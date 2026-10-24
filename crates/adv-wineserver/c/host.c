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
#include <sys/uio.h>
#include <sys/ioctl.h>
#include <sys/un.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdatomic.h>
#include <string.h>
#include <time.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/resource.h>
#ifdef __APPLE__
#include <sys/event.h>
#include <mach/mach.h>
#include <mach/mach_time.h>
#else
#include <sys/epoll.h>
#endif

#define TICKS_PER_SEC 10000000

enum hitode_clock { HITODE_MONOTONIC, HITODE_UNIX };

extern void hitode_host_wait( int timeout );
extern uint64_t hitode_host_ticks( enum hitode_clock clock );
extern void hitode_host_exit( int status ) __attribute__((noreturn));
extern const char *hitode_host_getenv( const char *name );
extern const char *hitode_host_executable( void );
extern const char *hitode_host_server_root( void );

/* Standalone server users have no NT platform; Wine sessions replace these. */
__attribute__((weak)) int hitode_ntsync_open( void ) { errno = ENODEV; return -1; }
__attribute__((weak)) int hitode_ntsync_known( int fd ) { return 0; }
__attribute__((weak)) int hitode_ntsync_ioctl( int fd, unsigned long request, void *arg )
{
    return ioctl( fd, request, arg );
}
__attribute__((weak)) void hitode_ntsync_forget( int fd ) {}
__attribute__((weak)) void hitode_ntsync_transfer( int fd, int operation ) {}

int hitode_ioctl( int fd, unsigned long request, ... )
{
    va_list args;
    void *arg;
    va_start( args, request );
    arg = va_arg( args, void * );
    va_end( args );
    if (hitode_ntsync_known( fd ))
        return hitode_ntsync_ioctl( fd, request, arg );
    return ioctl( fd, request, arg );
}

static void ntsync_rights( const struct msghdr *msg, int operation )
{
    struct cmsghdr *cmsg;
    for (cmsg = CMSG_FIRSTHDR( (struct msghdr *)msg ); cmsg;
         cmsg = CMSG_NXTHDR( (struct msghdr *)msg, cmsg ))
    {
        size_t i, count;
        int *fds;
        if (cmsg->cmsg_level != SOL_SOCKET || cmsg->cmsg_type != SCM_RIGHTS) continue;
        fds = (int *)CMSG_DATA( cmsg );
        count = (cmsg->cmsg_len - CMSG_LEN(0)) / sizeof(int);
        for (i = 0; i < count; i++) hitode_ntsync_transfer( fds[i], operation );
    }
}

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
    if (!strcmp( path, "/dev/ntsync" )) return hitode_ntsync_open();
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

/* Channel descriptors retain their kernel identity, including across SCM_RIGHTS.
 * Only their bytes live here. The execution owner serializes server and clients. */
struct channel
{
    unsigned char *data;
    size_t size, capacity;
    unsigned int readers, writers;
    void (*notify)( void *, size_t );
    void *owner;
    size_t token;
    int server_read;
};
struct endpoint { struct channel *channel; int writing; };
static struct endpoint *endpoints;
static size_t endpoint_count;
static uint64_t queued_io, system_io;
static uint64_t kernel_waits, kernel_events, kernel_poll_fds;
static uint64_t server_input;
static int server_running, server_queue = -1, kernel_returned;
static atomic_int kernel_ready = 1;

void hitode_server_running( int running ) { server_running = running; }
uint64_t hitode_server_input( void ) { return server_input; }
int hitode_server_queue( void ) { return server_queue; }
int hitode_server_ready( void ) { return atomic_load( &kernel_ready ); }
void hitode_server_connected( void ) { atomic_store( &kernel_ready, 1 ); }

/* Only the watcher accesses its wait set. The server's set remains owned by
 * the execution owner; readiness crosses threads through one atomic flag. */
struct event_watch
{
    int queue, stop[2];
    pthread_t thread;
    void (*notify)( void * );
    void *owner;
};

static void *event_watch_thread( void *arg )
{
    struct event_watch *watch = arg;
    for (;;)
    {
        int i, ret;
#ifdef __APPLE__
        struct kevent events[2];
        ret = kevent( watch->queue, NULL, 0, events, 2, NULL );
#else
        struct epoll_event events[2];
        ret = epoll_wait( watch->queue, events, 2, -1 );
#endif
        if (ret < 0 && errno == EINTR) continue;
        if (ret < 0) abort();
        for (i = 0; i < ret; i++)
        {
#ifdef __APPLE__
            if (events[i].ident == (uintptr_t)watch->stop[0]) return NULL;
#else
            if (events[i].data.fd == watch->stop[0]) return NULL;
#endif
        }
        atomic_store( &kernel_ready, 1 );
        watch->notify( watch->owner );
    }
}

void *hitode_event_watch( int queue, void (*notify)(void *), void *owner )
{
    struct event_watch *watch = calloc( 1, sizeof(*watch) );
    int error;
    if (!watch) return NULL;
#ifdef __APPLE__
    watch->queue = kqueue();
#else
    watch->queue = epoll_create1( EPOLL_CLOEXEC );
#endif
    if (watch->queue == -1) goto fail;
    if (pipe( watch->stop )) goto fail_queue;
    fcntl( watch->stop[0], F_SETFD, FD_CLOEXEC );
    fcntl( watch->stop[1], F_SETFD, FD_CLOEXEC );
#ifdef __APPLE__
    struct kevent events[2];
    EV_SET( &events[0], queue, EVFILT_READ, EV_ADD | EV_CLEAR, 0, 0, NULL );
    EV_SET( &events[1], watch->stop[0], EVFILT_READ, EV_ADD, 0, 0, NULL );
    if (kevent( watch->queue, events, 2, NULL, 0, NULL ) == -1) goto fail_pipe;
#else
    struct epoll_event event = { .events = EPOLLIN | EPOLLET, .data.fd = queue };
    if (epoll_ctl( watch->queue, EPOLL_CTL_ADD, queue, &event )) goto fail_pipe;
    event = (struct epoll_event){ .events = EPOLLIN, .data.fd = watch->stop[0] };
    if (epoll_ctl( watch->queue, EPOLL_CTL_ADD, watch->stop[0], &event )) goto fail_pipe;
#endif
    atomic_store( &kernel_ready, 1 );
    watch->notify = notify;
    watch->owner = owner;
    if ((error = pthread_create( &watch->thread, NULL, event_watch_thread, watch )))
    {
        errno = error;
        goto fail_pipe;
    }
    return watch;
fail_pipe:
    error = errno;
    close( watch->stop[0] );
    close( watch->stop[1] );
    errno = error;
fail_queue:
    error = errno;
    close( watch->queue );
    errno = error;
fail:
    free( watch );
    return NULL;
}

void hitode_event_unwatch( void *arg )
{
    struct event_watch *watch = arg;
    write( watch->stop[1], "x", 1 );
    pthread_join( watch->thread, NULL );
    close( watch->stop[0] );
    close( watch->stop[1] );
    close( watch->queue );
    free( watch );
}

struct channel_event
{
    struct channel_event *next;
    int queue, fd, filter;
#ifdef __APPLE__
    struct kevent event;
#else
    struct epoll_event event;
#endif
};
static struct channel_event *channel_events;

static struct channel_event **find_event( int queue, int fd, int filter )
{
    struct channel_event **ptr = &channel_events;
    while (*ptr && ((*ptr)->queue != queue || (*ptr)->fd != fd || (*ptr)->filter != filter))
        ptr = &(*ptr)->next;
    return ptr;
}

static struct endpoint *endpoint( int fd )
{
    return fd >= 0 && (size_t)fd < endpoint_count ? &endpoints[fd] : NULL;
}

static int attach( int fd, struct endpoint end )
{
    if ((size_t)fd >= endpoint_count)
    {
        size_t count = fd + 64;
        struct endpoint *table = realloc( endpoints, count * sizeof(*table) );
        if (!table) { errno = ENOMEM; return -1; }
        memset( table + endpoint_count, 0, (count - endpoint_count) * sizeof(*table) );
        endpoints = table;
        endpoint_count = count;
    }
    endpoints[fd] = end;
    if (end.writing) end.channel->writers++;
    else end.channel->readers++;
    return 0;
}

static void release( struct endpoint end )
{
    struct channel *channel = end.channel;
    if (end.writing) channel->writers--;
    else if (!--channel->readers) channel->notify = NULL;
    if (!channel->readers && !channel->writers)
    {
        free( channel->data );
        free( channel );
    }
}

/* Rust's OwnedFd closes after removing its registration. */
void hitode_channel_forget( int fd )
{
    struct endpoint *end = endpoint( fd );
    struct channel_event **ptr = &channel_events, *item;
    while ((item = *ptr))
    {
        if (item->fd != fd && item->queue != fd) { ptr = &item->next; continue; }
        *ptr = item->next;
        free( item );
    }
    if (end && end->channel)
    {
        if (!server_running && end->channel->server_read) server_input++;
        release( *end );
        end->channel = NULL;
    }
}

int hitode_close( int fd )
{
    hitode_channel_forget( fd );
    hitode_ntsync_forget( fd );
    return close( fd );
}

int hitode_pipe( int fds[2] )
{
    struct channel *channel;
    if (pipe( fds )) return -1;
    if (!(channel = calloc( 1, sizeof(*channel) ))) goto error;
    if (attach( fds[0], (struct endpoint){ channel, 0 } )) { free( channel ); goto error; }
    if (attach( fds[1], (struct endpoint){ channel, 1 } ))
    {
        hitode_channel_forget( fds[0] );
        goto error;
    }
    return 0;
error:
    close( fds[0] );
    close( fds[1] );
    errno = ENOMEM;
    return -1;
}

void hitode_channel_watch( int fd, void (*notify)(void *, size_t), void *owner, size_t token )
{
    struct channel *channel = endpoint( fd )->channel;
    channel->notify = notify;
    channel->owner = owner;
    channel->token = token;
    if (channel->size) notify( owner, token );
}

void hitode_channel_counts( uint64_t *queued, uint64_t *system )
{
    *queued = queued_io;
    *system = system_io;
}

ssize_t hitode_read( int fd, void *buf, size_t size )
{
    struct endpoint *end = endpoint( fd );
    struct channel *channel;
    if (!end || !(channel = end->channel)) { system_io++; return read( fd, buf, size ); }
    queued_io++;
    if (end->writing) { errno = EBADF; return -1; }
    if (!size) return 0;
    if (!channel->size)
    {
        if (!channel->writers) return 0;
        errno = EAGAIN;
        return -1;
    }
    if (size > channel->size) size = channel->size;
    memcpy( buf, channel->data, size );
    channel->size -= size;
    memmove( channel->data, channel->data + size, channel->size );
    return size;
}

ssize_t hitode_write( int fd, const void *buf, size_t size )
{
    struct endpoint *end = endpoint( fd );
    struct channel *channel;
    size_t old_size;
    if (!end || !(channel = end->channel)) { system_io++; return write( fd, buf, size ); }
    queued_io++;
    if (!end->writing) { errno = EBADF; return -1; }
    if (!channel->readers) { errno = EPIPE; return -1; }
    if (!size) return 0;
    old_size = channel->size;
    if (size > channel->capacity - old_size)
    {
        size_t capacity = old_size + size;
        unsigned char *data = realloc( channel->data, capacity );
        if (!data) { errno = ENOMEM; return -1; }
        channel->data = data;
        channel->capacity = capacity;
    }
    memcpy( channel->data + old_size, buf, size );
    channel->size += size;
    if (!server_running && channel->server_read) server_input++;
    if (!old_size && channel->notify) channel->notify( channel->owner, channel->token );
    return size;
}

static ssize_t channel_vector( int fd, const struct iovec *iov, int count, int writing )
{
    ssize_t total = 0, ret;
    int i;
    for (i = 0; i < count; i++)
    {
        if (!iov[i].iov_len) continue;
        ret = writing ? hitode_write( fd, iov[i].iov_base, iov[i].iov_len )
                      : hitode_read( fd, iov[i].iov_base, iov[i].iov_len );
        if (ret < 0) return total ? total : ret;
        total += ret;
        if ((size_t)ret < iov[i].iov_len) break;
    }
    return total;
}

ssize_t hitode_readv( int fd, const struct iovec *iov, int count )
{
    struct endpoint *end = endpoint( fd );
    if (end && end->channel) return channel_vector( fd, iov, count, 0 );
    system_io++;
    return readv( fd, iov, count );
}

ssize_t hitode_writev( int fd, const struct iovec *iov, int count )
{
    struct endpoint *end = endpoint( fd );
    if (end && end->channel) return channel_vector( fd, iov, count, 1 );
    system_io++;
    return writev( fd, iov, count );
}

/* Hold a reference while a registered endpoint is in a socket's rights queue.
 * fstat identifies the received duplicate without changing the socket protocol. */
struct transfer
{
    struct transfer *next;
    struct endpoint end;
    dev_t device;
    ino_t inode;
};
static struct transfer *transfers;

ssize_t hitode_sendmsg( int fd, const struct msghdr *msg, int flags )
{
    struct cmsghdr *cmsg;
    struct transfer *pending = NULL, *item;
    ssize_t ret;
    for (cmsg = CMSG_FIRSTHDR( (struct msghdr *)msg ); cmsg;
         cmsg = CMSG_NXTHDR( (struct msghdr *)msg, cmsg ))
    {
        size_t i, count;
        int *fds;
        if (cmsg->cmsg_level != SOL_SOCKET || cmsg->cmsg_type != SCM_RIGHTS) continue;
        fds = (int *)CMSG_DATA( cmsg );
        count = (cmsg->cmsg_len - CMSG_LEN(0)) / sizeof(int);
        for (i = 0; i < count; i++)
        {
            struct endpoint *end = endpoint( fds[i] );
            struct stat st;
            if (!end || !end->channel) continue;
            if (!(item = malloc( sizeof(*item) ))) { errno = ENOMEM; ret = -1; goto done; }
            fstat( fds[i], &st );
            *item = (struct transfer){ pending, *end, st.st_dev, st.st_ino };
            pending = item;
            if (end->writing) end->channel->writers++;
            else end->channel->readers++;
        }
    }
    ntsync_rights( msg, 0 );
    ret = sendmsg( fd, msg, flags );
    /* Client socket input is known before the watcher gets a turn. */
    if (ret >= 0 && !server_running) atomic_store( &kernel_ready, 1 );
    if (ret < 0) ntsync_rights( msg, -1 );
done:
    while ((item = pending))
    {
        pending = item->next;
        if (ret >= 0) { item->next = transfers; transfers = item; }
        else { release( item->end ); free( item ); }
    }
    return ret;
}

ssize_t hitode_recvmsg( int fd, struct msghdr *msg, int flags )
{
    ssize_t ret = recvmsg( fd, msg, flags );
    struct cmsghdr *cmsg;
    if (ret < 0) return ret;
    ntsync_rights( msg, 1 );
    for (cmsg = CMSG_FIRSTHDR( msg ); cmsg; cmsg = CMSG_NXTHDR( msg, cmsg ))
    {
        size_t i, count;
        int *fds;
        if (cmsg->cmsg_level != SOL_SOCKET || cmsg->cmsg_type != SCM_RIGHTS) continue;
        fds = (int *)CMSG_DATA( cmsg );
        count = (cmsg->cmsg_len - CMSG_LEN(0)) / sizeof(int);
        for (i = 0; i < count; i++)
        {
            struct stat st;
            struct transfer **ptr, *item;
            int writing = (fcntl( fds[i], F_GETFL ) & O_ACCMODE) == O_WRONLY;
            fstat( fds[i], &st );
            for (ptr = &transfers; (item = *ptr); ptr = &item->next)
                if (item->device == st.st_dev && item->inode == st.st_ino && item->end.writing == writing)
                {
                    if (attach( fds[i], item->end )) { close( fds[i] ); return -1; }
                    *ptr = item->next;
                    release( item->end );
                    free( item );
                    break;
                }
        }
    }
    return ret;
}

static int channel_poll( struct pollfd *fds, nfds_t count )
{
    struct pollfd other[count ? count : 1];
    nfds_t i, rest = 0;
    int ret, ready = 0;
    for (i = 0; i < count; i++)
    {
        struct endpoint *end = endpoint( fds[i].fd );
        struct channel *channel;
        fds[i].revents = 0;
        if (!end || !(channel = end->channel)) { other[rest++] = fds[i]; continue; }
#ifdef __APPLE__
        if (!fds[i].events) continue;
        if (!(end->writing ? channel->readers : channel->writers))
        {
            fds[i].revents = POLLHUP | (fds[i].events & POLLIN);
            continue;
        }
#endif
        if (end->writing)
        {
            if (!channel->readers) fds[i].revents |= POLLERR | (fds[i].events & POLLOUT);
            else fds[i].revents |= fds[i].events & POLLOUT;
        }
        else
        {
            if (channel->size) fds[i].revents |= fds[i].events & POLLIN;
            if (!channel->writers) fds[i].revents |= POLLHUP;
        }
    }
    kernel_poll_fds += rest;
    if (rest && (ret = poll( other, rest, 0 )) < 0) return ret;
    rest = 0;
    for (i = 0; i < count; i++)
    {
        struct endpoint *end = endpoint( fds[i].fd );
        if (!end || !end->channel) fds[i].revents = other[rest++].revents;
        if (fds[i].revents) ready++;
    }
    return ready;
}

/* The server asks once for ready descriptors. With none ready it returns to
 * the runtime, which resumes it on new input or at the timeout; the server
 * then sees whatever is ready by then and computes its next wait itself. */
int hitode_poll( struct pollfd *fds, nfds_t count, int timeout )
{
    int ret = channel_poll( fds, count );

    if (ret || !timeout) return ret;
    hitode_host_wait( timeout );
    return channel_poll( fds, count );
}

/* Only channel registrations live here; ordinary descriptors stay in Wine's
 * incremental kernel event set. Every kernel wait has a zero timeout. */
void hitode_event_counts( uint64_t *waits, uint64_t *events, uint64_t *poll_fds )
{
    *waits = kernel_waits;
    *events = kernel_events;
    *poll_fds = kernel_poll_fds;
}

#ifdef __APPLE__
static int channel_kevent( int queue, struct kevent *events, int capacity )
{
    const struct timespec zero = {0};
    struct kevent queued[capacity];
    struct channel_event *item;
    int count = 0, ret;
    for (item = channel_events; item && count < capacity; item = item->next)
    {
        struct endpoint *end;
        struct channel *channel;
        int eof;
        if (item->queue != queue || (item->event.flags & EV_DISABLE)) continue;
        end = endpoint( item->fd );
        channel = end->channel;
        eof = !(end->writing ? channel->readers : channel->writers);
        if (!eof && !(item->filter == EVFILT_READ ? channel->size : end->writing)) continue;
        EV_SET( &queued[count], item->fd, item->filter, eof ? EV_EOF : 0, 0,
                item->filter == EVFILT_READ ? channel->size : 0, item->event.udata );
        count++;
    }
    if (count == capacity) { memcpy( events, queued, count * sizeof(*events) ); return count; }
    ret = 0;
    if (atomic_exchange( &kernel_ready, 0 ))
    {
        kernel_waits++;
        ret = kevent( queue, NULL, 0, events, capacity - count, &zero );
        if (ret > 0)
        {
            atomic_store( &kernel_ready, 1 );
            kernel_returned = 1;
        }
    }
    if (ret < 0) return ret;
    kernel_events += ret;
    /* Dispatch socket closure before channel closure, as Wine's poll array does. */
    memcpy( events + ret, queued, count * sizeof(*events) );
    return count + ret;
}

int hitode_kevent( int queue, const struct kevent *changes, int count,
                   struct kevent *events, int capacity, const struct timespec *timeout )
{
    struct kevent other[count ? count : 1];
    int i, rest = 0, ret, wait = -1;
    if (server_running) server_queue = queue;
    for (i = 0; i < count; i++)
    {
        const struct kevent *ev = &changes[i];
        struct endpoint *end = endpoint( ev->ident );
        struct channel_event **ptr, *item;
        if (!end || !end->channel) { other[rest++] = *ev; continue; }
        if (server_running && ev->filter == EVFILT_READ) end->channel->server_read = 1;
        ptr = find_event( queue, ev->ident, ev->filter );
        item = *ptr;
        if (ev->flags & EV_DELETE)
        {
            if (!item) { errno = ENOENT; return -1; }
            *ptr = item->next;
            free( item );
            continue;
        }
        if (!item)
        {
            if (!(ev->flags & EV_ADD)) { errno = ENOENT; return -1; }
            if (!(item = calloc( 1, sizeof(*item) ))) { errno = ENOMEM; return -1; }
            item->queue = queue;
            item->fd = ev->ident;
            item->filter = ev->filter;
            *ptr = item;
        }
        if (ev->flags & EV_ADD) item->event = *ev;
        if (ev->flags & EV_DISABLE) item->event.flags |= EV_DISABLE;
        if (ev->flags & EV_ENABLE) item->event.flags &= ~EV_DISABLE;
    }
    if (rest)
    {
        if (kevent( queue, other, rest, NULL, 0, NULL ) < 0) return -1;
        atomic_store( &kernel_ready, 1 );
    }
    if (!capacity) return 0;
    if (timeout)
    {
        int64_t ms = (int64_t)timeout->tv_sec * 1000 + (timeout->tv_nsec + 999999) / 1000000;
        wait = ms > INT_MAX ? INT_MAX : ms;
    }
    /* Level readiness belongs to the next step, even if it stays ready. */
    if (server_running && kernel_returned)
    {
        kernel_returned = 0;
        hitode_host_wait( 0 );
    }
    ret = channel_kevent( queue, events, capacity );
    if (ret || !wait) return ret;
    hitode_host_wait( wait );
    return channel_kevent( queue, events, capacity );
}
#else
int hitode_epoll_ctl( int queue, int op, int fd, struct epoll_event *event )
{
    struct endpoint *end = endpoint( fd );
    struct channel_event **ptr, *item;
    if (server_running) server_queue = queue;
    if (!end || !end->channel)
    {
        int ret = epoll_ctl( queue, op, fd, event );
        if (!ret) atomic_store( &kernel_ready, 1 );
        return ret;
    }
    if (server_running && op != EPOLL_CTL_DEL && (event->events & EPOLLIN))
        end->channel->server_read = 1;
    ptr = find_event( queue, fd, 0 );
    item = *ptr;
    if (op == EPOLL_CTL_ADD)
    {
        if (item) { errno = EEXIST; return -1; }
        if (!(item = calloc( 1, sizeof(*item) ))) { errno = ENOMEM; return -1; }
        item->queue = queue;
        item->fd = fd;
        *ptr = item;
    }
    else if (!item) { errno = ENOENT; return -1; }
    if (op == EPOLL_CTL_DEL) { *ptr = item->next; free( item ); }
    else item->event = *event;
    return 0;
}

static int channel_epoll( int queue, struct epoll_event *events, int capacity )
{
    struct epoll_event queued[capacity];
    struct channel_event *item;
    int count = 0, ret;
    for (item = channel_events; item && count < capacity; item = item->next)
    {
        struct endpoint *end;
        struct channel *channel;
        unsigned int ready = 0;
        if (item->queue != queue) continue;
        end = endpoint( item->fd );
        channel = end->channel;
        if (end->writing)
        {
            ready = item->event.events & EPOLLOUT;
            if (!channel->readers) ready |= EPOLLERR;
        }
        else
        {
            if (channel->size) ready = item->event.events & EPOLLIN;
            if (!channel->writers) ready |= EPOLLHUP;
        }
        if (!ready) continue;
        queued[count] = item->event;
        queued[count++].events = ready;
    }
    if (count == capacity) { memcpy( events, queued, count * sizeof(*events) ); return count; }
    ret = 0;
    if (atomic_exchange( &kernel_ready, 0 ))
    {
        kernel_waits++;
        ret = epoll_wait( queue, events, capacity - count, 0 );
        if (ret > 0)
        {
            atomic_store( &kernel_ready, 1 );
            kernel_returned = 1;
        }
    }
    if (ret < 0) return ret;
    kernel_events += ret;
    memcpy( events + ret, queued, count * sizeof(*events) );
    return count + ret;
}

int hitode_epoll_wait( int queue, struct epoll_event *events, int capacity, int timeout )
{
    int ret;
    /* Level readiness belongs to the next step, even if it stays ready. */
    if (server_running && kernel_returned)
    {
        kernel_returned = 0;
        hitode_host_wait( 0 );
    }
    ret = channel_epoll( queue, events, capacity );
    if (ret || !timeout) return ret;
    hitode_host_wait( timeout );
    return channel_epoll( queue, events, capacity );
}
#endif

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
    extern int hitode_host_signal( int pid, int tid, int sig );
    if (hitode_host_signal( pid, -1, sig )) return 0;
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
