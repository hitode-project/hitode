/* Offload periodic registry branch writes off the execution thread while
 * keeping vendor/wine/server/registry.c unmodified (WINE-SRC-1). */

#include "config.h"
#include <stdint.h>
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

/* Direct byte-buffer serializer: produces output byte-identical to Wine's
 * save_all_subkeys() without stdio/open_memstream locking, fprintf parsing, or
 * repeated ancestor path recursion, and pre-sizes each branch from its prior
 * serialized size to avoid realloc storms on adv-execution. */
struct reg_buf
{
    char *data;
    size_t len;
    size_t cap;
    int oom;
};

static size_t branch_size_hint[MAX_SAVE_BRANCH_INFO];
static int periodic_next_branch;
static unsigned int periodic_retry_mask;
static const timeout_t stagger_period = 1 * -TICKS_PER_SEC;

static int reg_buf_init( struct reg_buf *buf, size_t initial_cap )
{
    if (initial_cap < 4096) initial_cap = 4096;
    buf->data = malloc( initial_cap );
    buf->len = 0;
    buf->cap = buf->data ? initial_cap : 0;
    buf->oom = buf->data == NULL;
    return !buf->oom;
}

static int reg_buf_grow( struct reg_buf *buf, size_t extra )
{
    size_t need, new_cap;
    char *new_data;

    if (buf->oom) return 0;
    if (extra > SIZE_MAX - buf->len)
    {
        buf->oom = 1;
        return 0;
    }
    need = buf->len + extra;
    new_cap = buf->cap ? buf->cap : 4096;
    while (new_cap < need)
    {
        if (new_cap > SIZE_MAX / 2)
        {
            new_cap = need;
            break;
        }
        new_cap *= 2;
    }
    new_data = realloc( buf->data, new_cap );
    if (!new_data)
    {
        buf->oom = 1;
        return 0;
    }
    buf->data = new_data;
    buf->cap = new_cap;
    return 1;
}

static inline int reg_buf_reserve( struct reg_buf *buf, size_t extra )
{
    if (buf->oom) return 0;
    if (extra <= buf->cap - buf->len) return 1;
    return reg_buf_grow( buf, extra );
}

static inline void reg_buf_put_char( struct reg_buf *buf, char ch )
{
    if (!reg_buf_reserve( buf, 1 )) return;
    buf->data[buf->len++] = ch;
}

static inline void reg_buf_put_bytes( struct reg_buf *buf, const char *src, size_t n )
{
    if (!n || !reg_buf_reserve( buf, n )) return;
    memcpy( buf->data + buf->len, src, n );
    buf->len += n;
}

#define reg_buf_put_lit( buf, lit ) reg_buf_put_bytes( (buf), (lit), sizeof(lit) - 1 )

static void reg_buf_put_u32_dec( struct reg_buf *buf, unsigned int val )
{
    char tmp[16];
    size_t n = 0;

    do
    {
        tmp[n++] = (char)('0' + (val % 10));
        val /= 10;
    } while (val);

    if (!reg_buf_reserve( buf, n )) return;
    while (n--) buf->data[buf->len++] = tmp[n];
}

static void reg_buf_put_hex_u32( struct reg_buf *buf, unsigned int val, int min_digits )
{
    static const char hex[16] = "0123456789abcdef";
    char tmp[8];
    int n = 0;

    do
    {
        tmp[n++] = hex[val & 0xf];
        val >>= 4;
    } while (val || n < min_digits);

    if (!reg_buf_reserve( buf, (size_t)n )) return;
    while (n--) buf->data[buf->len++] = tmp[n];
}

/* Exact match to vendor/wine/server/unicode.c:dump_strW(). */
static int reg_buf_put_strW( struct reg_buf *buf, const WCHAR *str,
                             data_size_t len, const char escape[2] )
{
    static const char escapes[32] = ".......abtnvfr.............e....";
    static const char hex[16] = "0123456789abcdef";
    size_t count = len / sizeof(WCHAR);
    size_t start_len = buf->len;

    while (count)
    {
        size_t chunk = count < 1024 ? count : 1024;
        size_t i;
        char *dst;

        if (!reg_buf_reserve( buf, chunk * 6 )) return 0;
        dst = buf->data + buf->len;
        for (i = 0; i < chunk; i++, str++, count--)
        {
            WCHAR ch = *str;
            if (ch >= 32 && ch <= 127)
            {
                if (ch == '\\' || ch == (unsigned char)escape[0] || ch == (unsigned char)escape[1])
                    *dst++ = '\\';
                *dst++ = (char)ch;
            }
            else if (ch > 127)
            {
                *dst++ = '\\';
                *dst++ = 'x';
                if (count > 1 && str[1] < 128 && isxdigit( (unsigned char)str[1] ))
                {
                    *dst++ = hex[(ch >> 12) & 0xf];
                    *dst++ = hex[(ch >> 8) & 0xf];
                    *dst++ = hex[(ch >> 4) & 0xf];
                    *dst++ = hex[ch & 0xf];
                }
                else
                {
                    if (ch >= 0x1000) *dst++ = hex[(ch >> 12) & 0xf];
                    if (ch >= 0x100) *dst++ = hex[(ch >> 8) & 0xf];
                    *dst++ = hex[(ch >> 4) & 0xf];
                    *dst++ = hex[ch & 0xf];
                }
            }
            else
            {
                *dst++ = '\\';
                if (escapes[ch] != '.')
                {
                    *dst++ = escapes[ch];
                }
                else if (count > 1 && str[1] >= '0' && str[1] <= '7')
                {
                    *dst++ = (char)('0' + ((ch >> 6) & 7));
                    *dst++ = (char)('0' + ((ch >> 3) & 7));
                    *dst++ = (char)('0' + (ch & 7));
                }
                else
                {
                    if (ch >= 8) *dst++ = (char)('0' + ((ch >> 3) & 7));
                    *dst++ = (char)('0' + (ch & 7));
                }
            }
        }
        buf->len = (size_t)(dst - buf->data);
    }
    return (int)(buf->len - start_len);
}

static void reg_buf_put_root_path( struct reg_buf *buf, const struct key *key )
{
    if (get_parent( key ))
    {
        reg_buf_put_root_path( buf, get_parent( key ) );
        reg_buf_put_lit( buf, "\\\\" );
    }
    reg_buf_put_strW( buf, key->obj.name->name, key->obj.name->len, "[]" );
}

static void reg_buf_put_value( struct reg_buf *buf, const struct key_value *value )
{
    static const char hex[16] = "0123456789abcdef";
    unsigned int i, dw;
    int count;

    if (value->namelen)
    {
        reg_buf_put_char( buf, '\"' );
        count = 1 + reg_buf_put_strW( buf, value->name, value->namelen, "\"\"" );
        reg_buf_put_lit( buf, "\"=" );
        count += 2;
    }
    else
    {
        reg_buf_put_lit( buf, "@=" );
        count = 2;
    }

    switch (value->type)
    {
    case REG_SZ:
    case REG_EXPAND_SZ:
    case REG_MULTI_SZ:
        if (value->len < sizeof(WCHAR)) break;
        if (value->len % sizeof(WCHAR)) break;
        if (((const WCHAR *)value->data)[value->len / sizeof(WCHAR) - 1]) break;
        if (value->type != REG_SZ)
        {
            reg_buf_put_lit( buf, "str(" );
            reg_buf_put_hex_u32( buf, value->type, 1 );
            reg_buf_put_lit( buf, "):" );
        }
        reg_buf_put_char( buf, '\"' );
        reg_buf_put_strW( buf, (const WCHAR *)value->data, value->len - sizeof(WCHAR), "\"\"" );
        reg_buf_put_lit( buf, "\"\n" );
        return;

    case REG_DWORD:
        if (value->len != sizeof(dw)) break;
        memcpy( &dw, value->data, sizeof(dw) );
        reg_buf_put_lit( buf, "dword:" );
        reg_buf_put_hex_u32( buf, dw, 8 );
        reg_buf_put_char( buf, '\n' );
        return;
    }

    if (value->type == REG_BINARY)
    {
        reg_buf_put_lit( buf, "hex:" );
        count += 4;
    }
    else
    {
        size_t before = buf->len;
        reg_buf_put_lit( buf, "hex(" );
        reg_buf_put_hex_u32( buf, value->type, 1 );
        reg_buf_put_lit( buf, "):" );
        count += (int)(buf->len - before);
    }

    if (!value->len)
    {
        reg_buf_put_char( buf, '\n' );
        return;
    }
    /* Each byte emits at most 3 bytes ("xx,") plus a 4-byte continuation every
     * ~25 bytes, so len * 4 + 16 always covers the entire value. */
    if (!reg_buf_reserve( buf, (size_t)value->len * 4 + 16 )) return;
    {
        const unsigned char *bytes = (const unsigned char *)value->data;
        char *dst = buf->data + buf->len;
        for (i = 0; i < value->len; i++)
        {
            unsigned char b = bytes[i];
            *dst++ = hex[b >> 4];
            *dst++ = hex[b & 0xf];
            count += 2;
            if (i < value->len - 1)
            {
                *dst++ = ',';
                if (++count > 76)
                {
                    *dst++ = '\\';
                    *dst++ = '\n';
                    *dst++ = ' ';
                    *dst++ = ' ';
                    count = 2;
                }
            }
        }
        *dst++ = '\n';
        buf->len = (size_t)(dst - buf->data);
    }
}

static void reg_buf_save_subkeys( struct reg_buf *buf, struct reg_buf *path,
                                  const struct key *key, const struct key *base )
{
    int i;

    if (buf->oom || path->oom) return;
    if (key->flags & KEY_VOLATILE) return;
    if ((key->last_value >= 0) || (key->last_subkey == -1) || key->class || (key->flags & KEY_SYMLINK))
    {
        reg_buf_put_lit( buf, "\n[" );
        if (key != base) reg_buf_put_bytes( buf, path->data, path->len );
        reg_buf_put_lit( buf, "] " );
        reg_buf_put_u32_dec( buf, (unsigned int)((key->modif - ticks_1601_to_1970) / TICKS_PER_SEC) );
        reg_buf_put_lit( buf, "\n#time=" );
        reg_buf_put_hex_u32( buf, (unsigned int)(key->modif >> 32), 1 );
        reg_buf_put_hex_u32( buf, (unsigned int)key->modif, 8 );
        reg_buf_put_char( buf, '\n' );
        if (key->class)
        {
            reg_buf_put_lit( buf, "#class=\"" );
            reg_buf_put_strW( buf, key->class, key->classlen, "\"\"" );
            reg_buf_put_lit( buf, "\"\n" );
        }
        if (key->flags & KEY_SYMLINK) reg_buf_put_lit( buf, "#link\n" );
        for (i = 0; i <= key->last_value; i++)
            reg_buf_put_value( buf, &key->values[i] );
    }
    for (i = 0; i <= key->last_subkey; i++)
    {
        const struct key *sub = key->subkeys[i];
        size_t saved_len = path->len;
        if (key != base) reg_buf_put_lit( path, "\\\\" );
        reg_buf_put_strW( path, sub->obj.name->name, sub->obj.name->len, "[]" );
        reg_buf_save_subkeys( buf, path, sub, base );
        path->len = saved_len;
        if (buf->oom || path->oom) return;
    }
}

static int serialize_branch_fast( const struct key *key, unsigned int branch,
                                  char **out_data, size_t *out_size )
{
    struct reg_buf buf, path;
    size_t hint = (branch < MAX_SAVE_BRANCH_INFO && branch_size_hint[branch])
                      ? branch_size_hint[branch]
                      : 65536;

    if (!reg_buf_init( &buf, hint )) return 0;
    if (!reg_buf_init( &path, 4096 ))
    {
        free( buf.data );
        return 0;
    }

    reg_buf_put_lit( &buf, "WINE REGISTRY Version 2\n;; All keys relative to " );
    reg_buf_put_root_path( &buf, key );
    reg_buf_put_char( &buf, '\n' );
    switch (prefix_type)
    {
    case PREFIX_32BIT:
        reg_buf_put_lit( &buf, "\n#arch=win32\n" );
        break;
    case PREFIX_64BIT:
        reg_buf_put_lit( &buf, "\n#arch=win64\n" );
        break;
    default:
        break;
    }
    reg_buf_save_subkeys( &buf, &path, key, key );
    free( path.data );

    if (buf.oom || path.oom)
    {
        free( buf.data );
        return 0;
    }
    if (branch < MAX_SAVE_BRANCH_INFO)
        branch_size_hint[branch] = buf.len + 4096;
    *out_data = buf.data;
    *out_size = buf.len;
    return 1;
}

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

    if (!force && !(key->flags & KEY_DIRTY))
    {
        if (debug_level > 1) dump_operation( key, NULL, "Not saving clean" );
        return 1;
    }

    if (debug_level > 1)
    {
        fprintf( stderr, "%s: ", filename );
        dump_operation( key, NULL, "saving" );
    }
    if (!serialize_branch_fast( key, branch, &buf, &size )) return 0;
    if (!hitode_registry_enqueue( config_dir_fd, filename, buf, size, branch ))
    {
        free( buf );
        return 0;
    }
    make_clean( key );
    return 1;
}

static int any_branch_needs_save( unsigned int retry_mask )
{
    int i;
    for (i = 0; i < save_branch_count; i++)
    {
        if ((save_branch_info[i].key->flags & KEY_DIRTY) || (retry_mask & (1u << i)))
            return 1;
    }
    return 0;
}

static void hitode_periodic_save( void *arg )
{
    int n;

    (void)arg;
    periodic_retry_mask |= hitode_registry_take_failed();
    save_timeout_user = NULL;

    /* Stagger periodic saves to at most one dirty branch per tick so multiple
     * modified hives never pile up in the same execution slice. */
    for (n = 0; n < save_branch_count; n++)
    {
        int i = (periodic_next_branch + n) % save_branch_count;
        int force = (periodic_retry_mask & (1u << i)) != 0;
        if (!(save_branch_info[i].key->flags & KEY_DIRTY) && !force) continue;
        if (save_branch_async( save_branch_info[i].key, save_branch_info[i].filename,
                               (unsigned int)i, force ))
        {
            periodic_retry_mask &= ~(1u << i);
        }
        else if (force)
        {
            save_branch_info[i].key->flags |= KEY_DIRTY;
        }
        periodic_next_branch = (i + 1) % save_branch_count;
        break;
    }

    if (any_branch_needs_save( periodic_retry_mask ))
        save_timeout_user = add_timeout_user( stagger_period, hitode_periodic_save, NULL );
    else
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
    unsigned int failed = periodic_retry_mask | hitode_registry_take_failed();
    unsigned int sync_mask = 0;

    periodic_retry_mask = 0;
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
