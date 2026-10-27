/* Configuration of the vendored wineserver for Apple and Android targets.
 *
 * Only the macros the server sources test are listed, true on macOS, iOS
 * and Android unless marked. The shim hands event-loop waits to the runtime. */

#ifndef __WINE_CONFIG_H
#define __WINE_CONFIG_H

#define HAVE_ARPA_INET_H 1
#define HAVE_CLOCK_GETTIME 1
#define HAVE_FSTATFS 1
#define HAVE_IFADDRS_H 1
#define HAVE_LINUX_NTSYNC_H 1
#define HAVE_NETINET_IN_H 1
#define HAVE_NETINET_TCP_H 1
#define HAVE_NET_IF_H 1
#define HAVE_PWD_H 1
#define HAVE_SCHED_H 1
#define HAVE_STRUCT_SOCKADDR_IN6_SIN6_SCOPE_ID 1
#define HAVE_SYS_MOUNT_H 1
#define HAVE_SYS_PARAM_H 1
#define HAVE_SYS_QUEUE_H 1
#define HAVE_SYS_RESOURCE_H 1
#define HAVE_SYS_STATVFS_H 1
#define HAVE_SYS_UIO_H 1
#define HAVE_SYS_UN_H 1
#define HAVE_UTIME_H 1

#ifdef __APPLE__
#define HAVE_KQUEUE 1
#define HAVE_SYS_EVENT_H 1
#define HAVE_STRUCT_SOCKADDR_UN_SUN_LEN 1
#define HAVE_SYS_FILIO_H 1
#define HAVE_SYS_SYSCTL_H 1
/* The iOS SDK does not ship this header. */
#include <TargetConditionals.h>
#if TARGET_OS_OSX
#define HAVE_NETINET_TCP_FSM_H 1
#endif
#else
#define HAVE_SYS_EPOLL_H 1
#define HAVE_EPOLL_CREATE 1
#define HAVE_LINUX_FILTER_H 1
#define HAVE_LINUX_MAJOR_H 1
#define HAVE_LINUX_TYPES_H 1
#define MAJOR_IN_SYSMACROS 1
#define HAVE_SYS_VFS_H 1
#endif

#define PACKAGE_STRING "Wine 11.0"

#endif
