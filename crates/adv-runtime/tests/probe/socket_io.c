/* Build: i686-w64-mingw32-gcc -O2 -o socket_io.exe socket_io.c -lws2_32 */
#include <winsock2.h>
#include <stdio.h>
#include <string.h>

#define CHECK(condition) do { \
    if (!(condition)) { \
        printf("line %d, Winsock error %d\n", __LINE__, WSAGetLastError()); \
        return 1; \
    } \
} while (0)

static int datagrams(void)
{
    SOCKET receiver = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    SOCKET sender = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
    struct sockaddr_in to = {0}, from = {0};
    int to_len = sizeof(to), from_len = sizeof(from);
    unsigned long nonblocking = 1, available = 0;
    char first[] = "abc", second[] = "de", bytes[8] = {0};
    WSABUF send_buffers[] = {{3, first}, {2, second}};
    WSABUF recv_buffers[] = {{2, bytes}, {6, bytes + 2}};
    DWORD count, flags = 0;
    WSAOVERLAPPED pending = {0};

    CHECK(receiver != INVALID_SOCKET && sender != INVALID_SOCKET);
    to.sin_family = AF_INET;
    to.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    CHECK(bind(receiver, (struct sockaddr *)&to, to_len) == 0);
    CHECK(getsockname(receiver, (struct sockaddr *)&to, &to_len) == 0);
    CHECK(ioctlsocket(receiver, FIONBIO, &nonblocking) == 0);
    CHECK(recv(receiver, bytes, sizeof(bytes), 0) == SOCKET_ERROR);
    CHECK(WSAGetLastError() == WSAEWOULDBLOCK);

    CHECK(WSASendTo(sender, send_buffers, 2, &count, 0,
        (struct sockaddr *)&to, to_len, NULL, NULL) == 0 && count == 5);
    fd_set readable;
    struct timeval timeout = {1, 0};
    FD_ZERO(&readable);
    FD_SET(receiver, &readable);
    CHECK(select(0, &readable, NULL, NULL, &timeout) == 1);
    CHECK(ioctlsocket(receiver, FIONREAD, &available) == 0 && available == 5);
    CHECK(WSARecvFrom(receiver, recv_buffers, 2, &count, &flags,
        (struct sockaddr *)&from, &from_len, NULL, NULL) == 0 && count == 5);
    CHECK(from_len == sizeof(from) && from.sin_family == AF_INET
        && from.sin_addr.s_addr == htonl(INADDR_LOOPBACK) && from.sin_port != 0);
    CHECK(!memcmp(bytes, "abcde", 5));

    pending.hEvent = WSACreateEvent();
    CHECK(pending.hEvent != WSA_INVALID_EVENT);
    CHECK(WSARecv(receiver, recv_buffers, 2, NULL, &flags, &pending, NULL) == SOCKET_ERROR);
    CHECK(WSAGetLastError() == WSA_IO_PENDING);
    CHECK(sendto(sender, "hello", 5, 0, (struct sockaddr *)&to, to_len) == 5);
    CHECK(WSAWaitForMultipleEvents(1, &pending.hEvent, FALSE, 1000, FALSE) == WSA_WAIT_EVENT_0);
    CHECK(WSAGetOverlappedResult(receiver, &pending, &count, FALSE, &flags));
    CHECK(count == 5 && !memcmp(bytes, "hello", 5));
    WSACloseEvent(pending.hEvent);
    closesocket(sender);
    closesocket(receiver);
    return 0;
}

static int streams(void)
{
    SOCKET listener = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    SOCKET client = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    SOCKET peer;
    struct sockaddr_in address = {0};
    int length = sizeof(address);
    char byte = 0;

    CHECK(listener != INVALID_SOCKET && client != INVALID_SOCKET);
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    CHECK(bind(listener, (struct sockaddr *)&address, length) == 0);
    CHECK(getsockname(listener, (struct sockaddr *)&address, &length) == 0);
    CHECK(listen(listener, 1) == 0);
    CHECK(connect(client, (struct sockaddr *)&address, length) == 0);
    peer = accept(listener, NULL, NULL);
    CHECK(peer != INVALID_SOCKET);
    CHECK(send(client, "x", 1, 0) == 1);
    CHECK(recv(peer, &byte, 1, MSG_PEEK) == 1 && byte == 'x');
    CHECK(recv(peer, &byte, 1, 0) == 1 && byte == 'x');
    closesocket(client);
    CHECK(recv(peer, &byte, 1, 0) == 0);
    closesocket(peer);
    closesocket(listener);
    return 0;
}

int main(void)
{
    WSADATA data;
    CHECK(WSAStartup(MAKEWORD(2, 2), &data) == 0);
    CHECK(!datagrams());
    CHECK(!streams());
    return WSACleanup();
}
