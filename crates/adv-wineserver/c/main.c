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
#include "handle.h"
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
    /* The server derives its machines from the host CPU. Hitode's guests are
     * i386 on every host, so the server takes the state an i386 build has
     * with a 32-bit prefix; init_registry() reads the machines only to mark
     * WoW registry keys, which a single machine does not have. */
    supported_machines[0] = IMAGE_FILE_MACHINE_I386;
    supported_machines_count = 1;
    native_machine = IMAGE_FILE_MACHINE_I386;
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
    /* The last guest thread is gone; there is no host process to reap. */
    process->unix_pid = -1;
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

extern int hitode_host_signal( int pid, int tid, int sig );
extern int hitode_host_memory( int pid, client_ptr_t ptr, data_size_t size, void *data, int write );

int send_thread_signal( struct thread *thread, int sig )
{
    return hitode_host_signal( thread->unix_pid, thread->unix_tid, sig );
}

int read_process_memory( struct process *process, client_ptr_t ptr, data_size_t size, char *dest )
{
    unsigned int status = hitode_host_memory( process->unix_pid, ptr, size, dest, 0 );
    if (status) set_error( status );
    return !status;
}

int write_process_memory( struct process *process, client_ptr_t ptr, data_size_t size, const char *src,
                          data_size_t *written )
{
    unsigned int status = hitode_host_memory( process->unix_pid, ptr, size, (void *)src, 1 );
    if (written) *written = status ? 0 : size;
    if (status) set_error( status );
    return !status;
}

/* context.rs marshals the i386 members of Wine's architecture unions. */
C_ASSERT( sizeof(struct context_data) == 1720 );
C_ASSERT( offsetof(struct context_data, ctl) == 8 );
C_ASSERT( offsetof(struct context_data, integer) == 40 );
C_ASSERT( offsetof(struct context_data, seg) == 288 );
C_ASSERT( offsetof(struct context_data, fp) == 304 );
C_ASSERT( offsetof(struct context_data, debug) == 824 );
C_ASSERT( offsetof(struct context_data, ext) == 944 );

/* The execution owner calls this while the server coroutine is suspended. */
unsigned int hitode_server_close_handle( unsigned int tid, unsigned int handle )
{
    struct thread *saved_current = current, *thread;
    unsigned int saved_global = global_error, saved_error = get_error(), status;
    thread = get_thread_from_id( tid );
    if (!thread) status = STATUS_INVALID_CID;
    else
    {
        current = thread;
        status = close_handle( thread->process, handle );
        current = saved_current;
        release_object( thread );
    }
    global_error = saved_global;
    if (current) current->error = saved_error;
    return status;
}

/* Cold descriptor admission uses object identity to share file positions. */
uintptr_t hitode_server_object( unsigned int tid, unsigned int handle )
{
    struct thread *thread;
    struct object *obj;
    uintptr_t identity = 0;
    unsigned int saved_global = global_error, saved_error = get_error();
    if ((thread = get_thread_from_id( tid )))
    {
        if ((obj = get_handle_obj( thread->process, handle, 0, NULL )))
        {
            identity = (uintptr_t)obj;
            release_object( obj );
        }
        release_object( thread );
    }
    global_error = saved_global;
    if (current) current->error = saved_error;
    return identity;
}
