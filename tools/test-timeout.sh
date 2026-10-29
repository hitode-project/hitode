#!/bin/sh
# Cargo runner: a test binary that runs past ADV_TEST_LIMIT seconds (default 900)
# fails with a timeout error and is killed. Other programs run unchanged, and
# nextest enforces its own per-test timeout.
case "$1" in
*/deps/*) ;;
*) exec "$@" ;;
esac
[ -n "${NEXTEST_RUN_ID:-}" ] && exec "$@"
exec perl -e '
my $limit = shift;
my $pid = fork() // die "fork: $!\n";
if ($pid == 0) { setpgrp(0, 0); exec { $ARGV[0] } @ARGV or die "exec $ARGV[0]: $!\n" }
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
