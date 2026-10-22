/* Configuration of the vendored wineserver for Apple targets.
 *
 * Only the macros the server sources test are listed, true on macOS and iOS
 * unless marked. HAVE_KQUEUE is left out on purpose: the server then waits
 * in poll(), which the shim hands to the runtime (shim.h). */

#ifndef __WINE_CONFIG_H
#define __WINE_CONFIG_H

#define HAVE_ARPA_INET_H 1
#define HAVE_CLOCK_GETTIME 1
#define HAVE_FSTATFS 1
#define HAVE_IFADDRS_H 1
#define HAVE_NETINET_IN_H 1
#define HAVE_NETINET_TCP_H 1
#define HAVE_NET_IF_H 1
#define HAVE_PWD_H 1
#define HAVE_SCHED_H 1
#define HAVE_STRUCT_SOCKADDR_IN6_SIN6_SCOPE_ID 1
#define HAVE_STRUCT_SOCKADDR_UN_SUN_LEN 1
#define HAVE_SYS_FILIO_H 1
#define HAVE_SYS_MOUNT_H 1
#define HAVE_SYS_PARAM_H 1
#define HAVE_SYS_QUEUE_H 1
#define HAVE_SYS_RESOURCE_H 1
#define HAVE_SYS_STATVFS_H 1
#define HAVE_SYS_SYSCTL_H 1
#define HAVE_SYS_UIO_H 1
#define HAVE_SYS_UN_H 1
#define HAVE_UTIME_H 1

/* The iOS SDK does not ship this header. */
#include <TargetConditionals.h>
#if TARGET_OS_OSX
#define HAVE_NETINET_TCP_FSM_H 1
#endif

#define PACKAGE_STRING "Wine 11.0"

#endif
