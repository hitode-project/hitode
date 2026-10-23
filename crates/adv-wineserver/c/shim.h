/* Host services of the in-process wineserver.
 *
 * Every server source is compiled with this header forced in front. It pulls
 * in the system headers first, so their declarations keep the real names,
 * then routes the calls that would block, leave, signal or read the clock
 * of the host process to host.c. */

#ifndef HITODE_SHIM_H
#define HITODE_SHIM_H

#include <stdarg.h>
#include <stdlib.h>
#include <stdio.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <unistd.h>
#include <poll.h>
#include <signal.h>
#include <time.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <sys/resource.h>
#ifdef __APPLE__
#include <mach/mach_types.h>
#include <mach/mach_time.h>
#endif

int hitode_asprintf( char **str, const char *format, ... );
int hitode_chdir( const char *path );
int hitode_fchdir( int fd );
int hitode_open( const char *path, int flags, ... );
FILE *hitode_fopen( const char *path, const char *mode );
int hitode_stat( const char *path, struct stat *st );
int hitode_lstat( const char *path, struct stat *st );
int hitode_mkdir( const char *path, mode_t mode );
int hitode_unlink( const char *path );
int hitode_rmdir( const char *path );
int hitode_rename( const char *from, const char *to );
int hitode_link( const char *from, const char *to );
ssize_t hitode_readlink( const char *path, char *buf, size_t size );
int hitode_chmod( const char *path, mode_t mode );
char *hitode_realpath( const char *path, char *resolved );
int hitode_bind( int fd, const struct sockaddr *addr, socklen_t len );
int hitode_connect( int fd, const struct sockaddr *addr, socklen_t len );

int hitode_poll( struct pollfd *fds, nfds_t count, int timeout );
char *hitode_getenv( const char *name );
void hitode_exit( int status ) __attribute__((noreturn));
int hitode_atexit( void (*func)(void) );
int hitode_kill( pid_t pid, int sig );
pid_t hitode_fork( void );
pid_t hitode_setsid( void );
pid_t hitode_waitpid( pid_t pid, int *status, int options );
unsigned int hitode_alarm( unsigned int seconds );
int hitode_usleep( useconds_t usec );
int hitode_gettimeofday( struct timeval *tv, void *tz );
int hitode_clock_gettime( clockid_t clock, struct timespec *ts );
time_t hitode_time( time_t *t );
int hitode_setpriority( int which, id_t who, int prio );
int hitode_setrlimit( int resource, const struct rlimit *limit );
#ifdef __APPLE__
uint64_t hitode_mach_continuous_time( void );
kern_return_t hitode_mach_timebase_info( mach_timebase_info_t info );
kern_return_t hitode_thread_policy_set( thread_act_t thread, thread_policy_flavor_t flavor,
                                        thread_policy_t policy, mach_msg_type_number_t count );
kern_return_t hitode_mach_port_extract_right( ipc_space_t task, mach_port_name_t name,
                                              mach_msg_type_name_t desired, mach_port_t *right,
                                              mach_msg_type_name_t *acquired );
kern_return_t hitode_mach_port_deallocate( ipc_space_t task, mach_port_name_t name );
int hitode_NSGetExecutablePath( char *buf, uint32_t *size );
#endif

#define asprintf(...) hitode_asprintf(__VA_ARGS__)
#define chdir(...) hitode_chdir(__VA_ARGS__)
#define fchdir(...) hitode_fchdir(__VA_ARGS__)
#define open(...) hitode_open(__VA_ARGS__)
#define fopen(...) hitode_fopen(__VA_ARGS__)
#define stat(...) hitode_stat(__VA_ARGS__)
#define lstat(...) hitode_lstat(__VA_ARGS__)
#define mkdir(...) hitode_mkdir(__VA_ARGS__)
#define unlink(...) hitode_unlink(__VA_ARGS__)
#define rmdir(...) hitode_rmdir(__VA_ARGS__)
#define rename(...) hitode_rename(__VA_ARGS__)
#define link(...) hitode_link(__VA_ARGS__)
#define readlink(...) hitode_readlink(__VA_ARGS__)
#define chmod(...) hitode_chmod(__VA_ARGS__)
#define realpath(...) hitode_realpath(__VA_ARGS__)
#define bind(...) hitode_bind(__VA_ARGS__)
#define connect(...) hitode_connect(__VA_ARGS__)

#define poll(...) hitode_poll(__VA_ARGS__)
#define getenv(...) hitode_getenv(__VA_ARGS__)
#define exit(...) hitode_exit(__VA_ARGS__)
#define _exit(...) hitode_exit(__VA_ARGS__)
#define atexit(...) hitode_atexit(__VA_ARGS__)
#define kill(...) hitode_kill(__VA_ARGS__)
#define fork(...) hitode_fork(__VA_ARGS__)
#define setsid(...) hitode_setsid(__VA_ARGS__)
#define waitpid(...) hitode_waitpid(__VA_ARGS__)
#define alarm(...) hitode_alarm(__VA_ARGS__)
#define usleep(...) hitode_usleep(__VA_ARGS__)
#define gettimeofday(...) hitode_gettimeofday(__VA_ARGS__)
#define clock_gettime(...) hitode_clock_gettime(__VA_ARGS__)
#define time(...) hitode_time(__VA_ARGS__)
#define setpriority(...) hitode_setpriority(__VA_ARGS__)
#define setrlimit(...) hitode_setrlimit(__VA_ARGS__)
#ifdef __APPLE__
#define mach_continuous_time(...) hitode_mach_continuous_time(__VA_ARGS__)
#define mach_timebase_info(...) hitode_mach_timebase_info(__VA_ARGS__)
#define thread_policy_set(...) hitode_thread_policy_set(__VA_ARGS__)
#define mach_port_extract_right(...) hitode_mach_port_extract_right(__VA_ARGS__)
#define mach_port_deallocate(...) hitode_mach_port_deallocate(__VA_ARGS__)
#define _NSGetExecutablePath(...) hitode_NSGetExecutablePath(__VA_ARGS__)
#endif

#endif
