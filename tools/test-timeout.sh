#!/bin/sh
# Cargo runner: a test binary that runs past ADV_TEST_LIMIT seconds (default 900)
# fails with a timeout error and is killed. Other programs run unchanged, and
# nextest enforces its own per-test timeout. Test binaries sit in deps/, or in
# build/<package>/<hash>/out/ under Cargo's newer build directory layout.
#
# macOS drops DYLD_* variables when it starts a system program such as this
# shell or perl, so the toolchain's libstd, which proc-macro tests load, goes
# back on the fallback path of the program that runs.
if [ "$(uname)" = Darwin ]; then
    ADV_TEST_DYLD=$(rustc --print target-libdir)
    export ADV_TEST_DYLD
    DYLD_FALLBACK_LIBRARY_PATH=$ADV_TEST_DYLD
    export DYLD_FALLBACK_LIBRARY_PATH
fi
case "$1" in
*/deps/* | */build/*/out/*) ;;
*) exec "$@" ;;
esac
[ -n "${NEXTEST_RUN_ID:-}" ] && exec "$@"
exec perl -e '
my $limit = shift;
my $pid = fork() // die "fork: $!\n";
if ($pid == 0) {
    setpgrp(0, 0);
    $ENV{DYLD_FALLBACK_LIBRARY_PATH} = $ENV{ADV_TEST_DYLD} if defined $ENV{ADV_TEST_DYLD};
    exec { $ARGV[0] } @ARGV or die "exec $ARGV[0]: $!\n";
}
# The test runs in its own process group, so a timeout also ends the processes
# it started; terminal signals are passed on to the group.
for my $signal (qw(INT TERM HUP)) { $SIG{$signal} = sub { kill $signal, -$pid } }
$SIG{ALRM} = sub {
    print STDERR "error: test binary $ARGV[0] timed out after ${limit}s\n";
    kill "KILL", -$pid;
    waitpid $pid, 0;
    exit 124;
};
alarm $limit;
waitpid $pid, 0;
exit($? & 127 ? 128 + ($? & 127) : $? >> 8);
' "${ADV_TEST_LIMIT:-900}" "$@"
