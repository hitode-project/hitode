/* File classification without transferring or opening a descriptor. */
#include "../../../vendor/wine/server/fd.c"

int hitode_server_file_kind( unsigned int tid, unsigned int handle )
{
    unsigned int saved_global = global_error, saved_error = get_error();
    struct thread *thread = get_thread_from_id( tid );
    struct fd *fd;
    int kind = FD_TYPE_INVALID;
    if (thread)
    {
        if ((fd = get_handle_fd_obj( thread->process, handle, 0 )))
        {
            kind = fd->fd_ops->get_fd_type( fd );
            release_object( fd );
        }
        release_object( thread );
    }
    global_error = saved_global;
    if (current) current->error = saved_error;
    return kind;
}
