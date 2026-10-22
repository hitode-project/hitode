/* Host services behind shim.h.
 *
 * This file is compiled without the shim, so the calls here reach the real
 * system functions. The hitode_host_* functions come from the Rust facade:
 * the runtime owns waiting, time and the server's lifetime. */

#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <string.h>
#include <time.h>
#include <sys/time.h>
#include <sys/types.h>
#include <mach/mach.h>
#include <mach/mach_time.h>

#define TICKS_PER_SEC 10000000

enum hitode_clock { HITODE_MONOTONIC, HITODE_UNIX };

extern void hitode_host_wait( int timeout );
extern uint64_t hitode_host_ticks( enum hitode_clock clock );
extern void hitode_host_exit( int status ) __attribute__((noreturn));
extern const char *hitode_host_getenv( const char *name );
extern const char *hitode_host_executable( void );

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
