#!/bin/sh
# Stands in for an nvim that never finishes starting. The test kills it when it is done, which
# reaches `sleep` because it replaces the shell.
exec sleep 60
