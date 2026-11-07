#!/bin/sh
# Stands in for an nvim whose setup script failed. It sends what nvim sends then, the msgpack array
# [2, "nvim_error_event", [0, "bad"]], and waits for the test to kill it, which reaches `sleep`
# because it replaces the shell.
printf '\223\002\260nvim_error_event\222\000\243bad'
exec sleep 60
