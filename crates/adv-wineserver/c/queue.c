/* Native access to Wine's queue keyboard state. */
#include "../../../vendor/wine/server/queue.c"

int hitode_server_key_state( unsigned int tid, unsigned int key )
{
    unsigned int saved_global_error = global_error, saved_error = get_error();
    struct thread *thread = get_thread_from_id( tid );
    int state = -1;

    global_error = saved_global_error;
    if (current) current->error = saved_error;
    if (!thread) return -1;
    if (thread->queue)
    {
        struct thread_input *input = thread->queue->input;
        sync_input_keystate( input );
        state = input->shared->keystate[key & 0xff];
    }
    release_object( thread );
    return state;
}

int hitode_server_keyboard_state( unsigned int tid, unsigned char *state )
{
    unsigned int saved_global = global_error, saved_error = get_error(), i;
    struct thread *thread = get_thread_from_id( tid );
    int found = 0;
    if (thread)
    {
        if (thread->queue)
        {
            if (state) for (i = 0; i < 256; i++) state[i] = thread->queue->input->shared->keystate[i] & 0x81;
            found = 1;
        }
        release_object( thread );
    }
    global_error = saved_global;
    if (current) current->error = saved_error;
    return found;
}
