# wob FIFO input

The wob listener accepts lines up to 4,096 bytes, excluding the newline. Longer lines are discarded through the next newline or writer EOF using bounded memory. A later valid line is processed normally. Non-UTF-8 lines are discarded. Ordinary value, maximum and style parsing is unchanged.

The configured endpoint must be a FIFO owned by the current effective user with mode `0600`. The listener creates a missing FIFO, but rejects symlinks, shared permissions and existing regular files. It never deletes a regular file to replace it with a pipe. Each open uses `O_NOFOLLOW` and validates the opened descriptor, closing the check/open substitution gap. Symlinks in parent directories are not confined; this remains a user-selected local path.

An unused FIFO waits for a writer without polling on a timer. After a writer closes, the listener reopens and revalidates the endpoint. An oversized writer can keep occupying its input stream, but cannot grow the line buffer beyond 4 KiB.
