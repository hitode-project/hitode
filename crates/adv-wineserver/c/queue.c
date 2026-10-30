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

/* Existing queue/desktop state only: zero asks the caller to perform server or
 * guest callback work. No queue, descriptor or reply buffer is created here. */
int hitode_server_user( unsigned int tid, unsigned int op, unsigned int a,
                        unsigned int b, unsigned int c, unsigned int d,
                        unsigned int out[4] )
{
    unsigned int saved_global = global_error, saved_error = get_error();
    struct thread *thread = get_thread_from_id( tid );
    struct msg_queue *queue;
    struct desktop *desktop = NULL;
    struct message *msg;
    struct timer *timer;
    unsigned int filter, clear = 0, state;
    user_handle_t win;
    int complete = 0;

    if (!thread || !(queue = thread->queue)) goto done;
    if (op != 0 && op != 1 && queue->shared->internal_bits) goto done;
    switch (op)
    {
    case 0: /* asynchronous key state */
    case 1: /* cursor position, on the driver's 96-DPI desktop */
        if (!(desktop = get_thread_desktop( thread, 0 ))) goto done;
        complete = 1;
        if (!(desktop->winstation->flags & WSF_VISIBLE))
        {
            out[3] = op ? ERROR_REQUIRES_INTERACTIVE_WINDOWSTATION : ERROR_ACCESS_DENIED;
            break;
        }
        if (desktop->winstation->input_desktop != desktop)
        {
            out[3] = ERROR_ACCESS_DENIED;
            break;
        }
        if (op == 1)
        {
            out[0] = 1;
            out[1] = desktop->shared->cursor.x;
            out[2] = desktop->shared->cursor.y;
            break;
        }
        if (a >= 256) { out[3] = ERROR_INVALID_PARAMETER; break; }
        if (queue->shared->internal_bits) { complete = 0; goto done; }
        state = desktop->shared->keystate[a];
        out[0] = ((state & 0x80) << 8) | !!(state & 0x40);
        if (state & 0x40)
        {
            SHARED_WRITE_BEGIN( desktop->shared, desktop_shm_t )
            {
                shared->keystate[a] &= ~0x40;
                ++shared->keystate_serial;
            }
            SHARED_WRITE_END;
        }
        break;
    case 2: /* GetQueueStatus */
        /* Deadline processing belongs to the server owner, not this snapshot. */
        if ((a & QS_TIMER) && !list_empty( &queue->pending_timers )) goto done;
        out[0] = (queue->shared->changed_bits & a) | ((queue->shared->wake_bits & a) << 16);
        clear = a;
        complete = 1;
        break;
    case 3: /* GetInputState */
        out[0] = queue->shared->wake_bits & (QS_KEY | QS_MOUSEBUTTON);
        complete = 1;
        break;
    case 4: /* empty PeekMessage */
        win = get_user_full_handle( a == 0xffff ? -1 : a );
        if (win && win != 1 && win != -1 && !get_user_object( win, NTUSER_OBJ_WINDOW ))
        {
            out[3] = ERROR_INVALID_WINDOW_HANDLE;
            complete = 1;
            break;
        }
        if (!list_empty( &queue->msg_list[SEND_MESSAGE] )) goto done;
        if (!b && !c) c = ~0U;
        if (!(filter = d >> 16)) filter = QS_ALLINPUT;
        if (filter & QS_RAWINPUT) filter |= QS_KEY | QS_MOUSEMOVE | QS_MOUSEBUTTON;
        LIST_FOR_EACH_ENTRY( msg, &queue->msg_list[POST_MESSAGE], struct message, entry )
        {
            if (!(filter & QS_POSTMESSAGE) && !((filter & QS_HOTKEY) && msg->msg == WM_HOTKEY)) continue;
            if (match_window( win, msg->win ) && check_msg_filter( msg->msg, b, c )) goto done;
        }
        if ((filter & QS_POSTMESSAGE) && queue->quit_message) goto done;
        if ((filter & QS_INPUT) && filter_contains_hw_range( b, c ) &&
            !list_empty( &queue->input->msg_list )) goto done;
        /* Paint retrieval mutates the chosen window; timers may restart. */
        if ((filter & QS_PAINT) && queue->paint_count && check_msg_filter( WM_PAINT, b, c )) goto done;
        if (filter & QS_TIMER)
        {
            if (find_expired_timer( queue, win, b, c, 0 )) goto done;
            LIST_FOR_EACH_ENTRY( timer, &queue->pending_timers, struct timer, entry )
                if ((!win || timer->win == win) && check_msg_filter( timer->msg, b, c )) goto done;
        }
        if (filter & QS_POSTMESSAGE)
        {
            clear |= QS_POSTMESSAGE | QS_HOTKEY | QS_TIMER;
            if (!b && c == ~0U) clear |= QS_ALLPOSTMESSAGE;
        }
        if (filter & QS_INPUT) clear |= QS_INPUT;
        if (filter & QS_PAINT) clear |= QS_PAINT;
        complete = 1;
        break;
    }
    if (complete && op != 1 && !out[3])
    {
        SHARED_WRITE_BEGIN( queue->shared, queue_shm_t )
        {
            shared->access_time = monotonic_time;
            shared->wake_mask = shared->changed_mask = 0;
            shared->changed_bits &= ~clear;
        }
        SHARED_WRITE_END;
        if (!get_queue_status( queue )) reset_sync( queue->sync );
    }
 done:
    if (desktop) release_object( desktop );
    if (thread) release_object( thread );
    global_error = saved_global;
    if (current) current->error = saved_error;
    return complete;
}
