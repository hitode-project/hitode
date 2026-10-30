/* Native access to existing in-process synchronization objects. */
#include "../../../vendor/wine/server/inproc_sync.c"

int hitode_server_sync_fd( unsigned int tid, unsigned int handle, unsigned int access,
                           int expected, unsigned int *status )
{
    struct thread *thread;
    struct object *object;
    unsigned int saved_global = global_error, saved_error = get_error();
    int fd = -1, type = 0;
    *status = 0;
    if (!(thread = get_thread_from_id( tid ))) *status = STATUS_INVALID_CID;
    else
    {
        if (expected == -1) fd = get_inproc_sync_fd( thread->alert_sync );
        else if ((object = get_handle_obj( thread->process, handle, access, NULL )))
        {
            fd = get_obj_inproc_sync( object, &type );
            if (expected && type != expected) { *status = STATUS_OBJECT_TYPE_MISMATCH; fd = -1; }
            release_object( object );
        }
        else *status = get_error();
        release_object( thread );
    }
    global_error = saved_global;
    if (current) current->error = saved_error;
    return fd;
}
