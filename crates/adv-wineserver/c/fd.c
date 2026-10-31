/* File classification and server-owned shared cursors. */
struct object_ops;
void *alloc_object( const struct object_ops *ops );
static void *alloc_with_position( const struct object_ops *ops );
#define alloc_object alloc_with_position
#include "../../../vendor/wine/server/fd.c"
#undef alloc_object

/* The cursor belongs to the server fd, including handles whose clients have
 * not admitted a descriptor yet. Wine's allocation and destruction own it. */
struct positioned_fd
{
    struct fd fd;
    void *position;
    void (*release_position)(void *);
};

static void destroy_with_position( struct object *obj )
{
    struct positioned_fd *fd = (struct positioned_fd *)obj;
    fd_destroy( obj );
    if (fd->position) fd->release_position( fd->position );
}

static void *alloc_with_position( const struct object_ops *ops )
{
    static struct object_ops positioned_ops;
    struct positioned_fd *fd;
    if (ops != &fd_ops) return alloc_object( ops );
    if (!positioned_ops.size)
    {
        positioned_ops = fd_ops;
        positioned_ops.size = sizeof(*fd);
        positioned_ops.destroy = destroy_with_position;
    }
    if ((fd = alloc_object( &positioned_ops ))) fd->position = NULL;
    return fd;
}

void *hitode_server_file_position( unsigned int tid, unsigned int handle,
                                  void *(*create)(int64_t), void (*release)(void *) )
{
    unsigned int saved_global = global_error, saved_error = get_error();
    struct thread *thread = get_thread_from_id( tid );
    struct fd *fd;
    void *position = NULL;
    if (thread)
    {
        if ((fd = get_handle_fd_obj( thread->process, handle, 0 )))
        {
            struct positioned_fd *shared = (struct positioned_fd *)fd;
            if (!shared->position)
            {
                int64_t offset = fd->fd_ops->get_fd_type( fd ) == FD_TYPE_FILE
                    ? lseek( fd->unix_fd, 0, SEEK_CUR ) : 0;
                shared->position = create( offset );
                shared->release_position = release;
            }
            position = shared->position;
            release_object( fd );
        }
        release_object( thread );
    }
    global_error = saved_global;
    if (current) current->error = saved_error;
    return position;
}

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
