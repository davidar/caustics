#!/bin/sh
# A stand-in for polkit-agent-helper-1 that speaks its line protocol
# (polkit/src/helper.rs has it) and authenticates nobody: what the tests and
# the nested gate's dry run spawn as CAUSTICS_POLKIT_HELPER.
#
#   fake-helper.sh <user>
#
# It reads the cookie line, asks once with the echo off, reads one answer,
# and succeeds when the answer is $FAKE_HELPER_SECRET. Its own environment:
#
#   FAKE_HELPER_SECRET   the answer that succeeds
#   FAKE_HELPER_RECORD   a file each answer line is appended to, as read:
#                        the proof that the typed bytes reached the helper
#                        and nothing else did
#   FAKE_HELPER_COOKIES  a file each cookie line is appended to
#   FAKE_HELPER_MODE     `ask` (the default), `twice` (an echo-on ask for a
#                        name before the password), or `die` (exit after the
#                        ask without reading an answer or saying FAILURE)
#
# Nothing is written to stderr: the agent's witnesses are on it.
IFS= read -r cookie || exit 1
[ -n "${FAKE_HELPER_COOKIES:-}" ] && printf '%s\n' "$cookie" >>"$FAKE_HELPER_COOKIES"
if [ "${FAKE_HELPER_MODE:-ask}" = twice ]; then
    printf 'PAM_PROMPT_ECHO_ON Login as %s: \n' "$1"
    IFS= read -r name || { printf 'FAILURE\n'; exit 1; }
    [ -n "${FAKE_HELPER_RECORD:-}" ] && printf '%s\n' "$name" >>"$FAKE_HELPER_RECORD"
fi
printf 'PAM_PROMPT_ECHO_OFF Password: \n'
[ "${FAKE_HELPER_MODE:-ask}" = die ] && exit 3
IFS= read -r answer || { printf 'FAILURE\n'; exit 1; }
[ -n "${FAKE_HELPER_RECORD:-}" ] && printf '%s\n' "$answer" >>"$FAKE_HELPER_RECORD"
if [ "$answer" = "${FAKE_HELPER_SECRET:-}" ]; then
    printf 'PAM_TEXT_INFO ok\nSUCCESS\n'
    exit 0
fi
printf 'PAM_ERROR_MSG wrong\nFAILURE\n'
exit 1
