# Sourced by runners that make temporary files for QEMU: run_qemu runs
# QEMU as a child instead of exec'ing it, so the runner's EXIT trap still
# deletes them (exec would replace the shell, trap and all, and leave a
# copy of the initramfs in /tmp on every run). A signal for the runner (a
# test's timeout, Ctrl-C) goes on to QEMU; QEMU's exit status, which the
# tests read, is the runner's.
run_qemu() {
    qemu-system-x86_64 "$@" <&0 &
    QEMU_PID=$!
    trap 'kill -TERM "$QEMU_PID" 2>/dev/null' TERM INT HUP
    local status=0
    wait "$QEMU_PID" || status=$?
    # A trapped signal ends `wait` early: wait for QEMU itself.
    while kill -0 "$QEMU_PID" 2>/dev/null; do
        status=0
        wait "$QEMU_PID" || status=$?
    done
    return "$status"
}
