/* Offload periodic registry branch writes off the execution thread while
 * keeping vendor/wine/server/registry.c unmodified (WINE-SRC-1). */

#include "config.h"
#include "ntstatus.h"
#define WIN32_NO_STATUS
#include "object.h"
#include "file.h"

extern int hitode_registry_enqueue( int dir_fd, const char *filename, char *data,
                                    size_t size, unsigned int branch );
extern void hitode_registry_wait( void );
extern unsigned int hitode_registry_take_failed( void );
extern void hitode_registry_saving( int active );

static void hitode_periodic_save( void *arg );
static struct timeout_user *hitode_registry_add_timeout_user(
    timeout_t when, timeout_callback func, void *private );

#define add_timeout_user hitode_registry_add_timeout_user
#define flush_registry wine_flush_registry
#include "../../../vendor/wine/server/registry.c"
#undef flush_registry
#undef add_timeout_user

static int save_branch_sync_fallback( struct key *key, const char *filename )
{
    int ret = 0;

    if (fchdir( config_dir_fd ) == -1) return 0;
    hitode_registry_saving( 1 );
    ret = save_branch( key, filename );
    hitode_registry_saving( 0 );
    if (fchdir( server_dir_fd ) == -1)
        fatal_error( "chdir to server dir: %s\n", strerror( errno ) );
    return ret;
}

static int save_branch_async( struct key *key, const char *filename, unsigned int branch, int force )
{
    char *buf = NULL;
    size_t size = 0;
    FILE *f;

    if (!force && !(key->flags & KEY_DIRTY))
    {
        if (debug_level > 1) dump_operation( key, NULL, "Not saving clean" );
        return 1;
    }

    if (!(f = open_memstream( &buf, &size ))) return 0;
    if (debug_level > 1)
    {
        fprintf( stderr, "%s: ", filename );
        dump_operation( key, NULL, "saving" );
    }
    save_all_subkeys( key, f );
    if (fclose( f ) != 0)
    {
        free( buf );
        return 0;
    }
    if (!hitode_registry_enqueue( config_dir_fd, filename, buf, size, branch ))
    {
        free( buf );
        return 0;
    }
    make_clean( key );
    return 1;
}

static void hitode_periodic_save( void *arg )
{
    int i;
    unsigned int failed = hitode_registry_take_failed();

    save_timeout_user = NULL;
    for (i = 0; i < save_branch_count; i++)
    {
        int force = (failed & (1u << i)) != 0;
        if (!save_branch_async( save_branch_info[i].key, save_branch_info[i].filename,
                                (unsigned int)i, force ) && force)
            save_branch_info[i].key->flags |= KEY_DIRTY;
    }
    set_periodic_save_timer();
}

static struct timeout_user *hitode_registry_add_timeout_user(
    timeout_t when, timeout_callback func, void *private )
{
    if (func == periodic_save) func = hitode_periodic_save;
    return add_timeout_user( when, func, private );
}

void flush_registry( void )
{
    int i;
    unsigned int failed = hitode_registry_take_failed();
    unsigned int sync_mask = 0;

    for (i = 0; i < save_branch_count; i++)
    {
        struct key *key = save_branch_info[i].key;
        int force = (failed & (1u << i)) != 0;
        if (!(key->flags & KEY_DIRTY) && !force) continue;
        if (!save_branch_async( key, save_branch_info[i].filename, (unsigned int)i, force ))
        {
            if (force) key->flags |= KEY_DIRTY;
            sync_mask |= (1u << i);
        }
    }
    /* Wait for every in-flight background write before any synchronous
     * fallback so an older background rename cannot overwrite a newer hive. */
    hitode_registry_wait();
    sync_mask |= hitode_registry_take_failed();
    for (i = 0; i < save_branch_count; i++)
    {
        struct key *key;
        if (!(sync_mask & (1u << i))) continue;
        key = save_branch_info[i].key;
        key->flags |= KEY_DIRTY;
        if (!save_branch_sync_fallback( key, save_branch_info[i].filename ))
        {
            fprintf( stderr, "wineserver: could not save registry branch to %s",
                     save_branch_info[i].filename );
            perror( " " );
        }
    }
}
