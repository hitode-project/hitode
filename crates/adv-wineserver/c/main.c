/* Startup and process tracing functions of the in-process wineserver.
 *
 * They replace server/main.c and the tracing back ends (mach.c, ptrace.c,
 * procfs.c). The server always stays in the foreground: Hitode owns the
 * host process, its signals and its limits, so option parsing, daemonizing,
 * signal handlers and resource limits are left out. */

#include "config.h"

#include <string.h>

#include "ntstatus.h"
#define WIN32_NO_STATUS
#include "winternl.h"

#include "file.h"
#include "process.h"
#include "thread.h"
#include "request.h"
#include "unicode.h"

int debug_level = 0;
int foreground = 1;
timeout_t master_socket_timeout = 3 * -TICKS_PER_SEC;
const char *server_argv0 = "wineserver";

/* Runs the server until the last client leaves; the same order as main(). */
int hitode_server_main( int debug )
{
    debug_level = debug;
    sock_init();
    open_master_socket();
    set_current_time();
    init_memory();
    init_directories( load_intl_file() );
    init_threading();
    init_registry();
    main_loop();
    return 0;
}

/* Guest processes and threads are not host processes, so there is nothing
 * to attach to. The functions below answer as the Apple back end does when a
 * process has no task port, until the NT platform provides these services. */

void init_tracing_mechanism(void)
{
}

void init_process_tracing( struct process *process )
{
}

void finish_process_tracing( struct process *process )
{
}

void init_thread_context( struct thread *thread )
{
}

void sigchld_callback(void)
{
}

void get_thread_context( struct thread *thread, struct context_data *context, unsigned int flags )
{
    memset( &context->debug, 0, sizeof(context->debug) );
    context->flags |= SERVER_CTX_DEBUG_REGISTERS;
}

void set_thread_context( struct thread *thread, const struct context_data *context, unsigned int flags )
{
    set_error( STATUS_UNSUCCESSFUL );
}

int send_thread_signal( struct thread *thread, int sig )
{
    thread->unix_pid = -1;
    thread->unix_tid = -1;
    return 0;
}

int read_process_memory( struct process *process, client_ptr_t ptr, data_size_t size, char *dest )
{
    set_error( STATUS_ACCESS_DENIED );
    return 0;
}

int write_process_memory( struct process *process, client_ptr_t ptr, data_size_t size, const char *src,
                          data_size_t *written )
{
    set_error( STATUS_ACCESS_DENIED );
    return 0;
}
