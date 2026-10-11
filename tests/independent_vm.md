# Independent guest crash verification

This opt-in test destroys **only its own disposable QEMU guest**. It requires
explicit authorization to install QEMU and use that guest. It does not restart
the Mac, touch OrbStack, mount a host project, or operate an existing user Host.

`independent_vm.py run` performs two rounds against the same dedicated guest
disk: a real Linux SysRq `c` kernel panic, observed on its serial console, and
an external `SIGKILL` of the exact QEMU child while Core/native work is active.
Neither round calls guest sync, shutdown or unmount before the fault. Each round
starts with a fresh Core store and reboots the independent guest kernel before
reading the original store. This tests guest virtual power loss; it does not
establish Mac physical power-loss or storage-controller behavior.

## Provisioning prerequisites

Use macOS arm64, a locally installed `qemu-system-aarch64` with HVF entitlement,
and QEMU's `edk2-aarch64-code.fd`. A bounded guest uses 4 vCPUs, 4096 MiB RAM and
a dedicated 24 GiB qcow2 disk under a fresh `rho-*` direct child of `/private/tmp`.
No host folders are shared with the guest. QEMU uses `cache=none`, honoring guest
flushes; it does not use snapshot/unsafe caching. SSH is forwarded only on
`127.0.0.1` at an unprivileged port.

For the selected experiment the official image is Debian 13 generic arm64,
version `20261001-2618`:

- [Fixed image](https://cloud.debian.org/images/cloud/trixie/20261001-2618/debian-13-generic-arm64-20261001-2618.qcow2)
- [Official checksum manifest](https://cloud.debian.org/images/cloud/trixie/20261001-2618/SHA512SUMS)
- Expected image SHA512:
  `001e742c14a69f3f506c1663b271b4aeed7fa766f7471a13d49ee22d338b8a637b5206a75bf994435af7b26ce3794a255dcf22e312b0d4b8d7b7f96076fe925f`

Verify the downloaded image against the official manifest before creating a
writable qcow2 overlay. Grow the overlay to 24 GiB. A NoCloud `CIDATA` ISO supplies
an ephemeral SSH public key, ordinary guest account `rho` (UID 1000), guest-only
passwordless sudo, and `/etc/rho-disposable-marker` containing a unique VM ID.
Password SSH login remains disabled. Never use a personal SSH private key for
this fixture. The driver refuses an SSH peer whose marker does not match.

Install `build-essential`, `pkg-config`, Python 3 and the pinned Rust 1.97.0 in
this guest. Transfer `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `src/` and
`docs/MANAGED-RUN.md` (embedded by `src/lib.rs` through `include_str!`)
from the selected Core checkout, recording SHA256 for every transferred source
file. Build once with `cargo +1.97.0 build --bin rho-core --locked -j 2`; never run this
Cargo build concurrently with another repository Cargo operation. Verify the
guest source manifest matches the selected source and record the guest binary
SHA256 and `rustc -vV`. Transfer both `tests/linux_platform.py` and
`tests/independent_vm.py` to `/home/rho/tests`. End provisioning and stop only
this guest before invoking the fault driver.

## Invocation

After provisioning, with the dedicated paths and marker selected above:

```sh
python3 -B tests/independent_vm.py run \
  --root /private/tmp/rho-independent-vm-20261011 \
  --disk /private/tmp/rho-independent-vm-20261011/guest.qcow2 \
  --seed /private/tmp/rho-independent-vm-20261011/seed-v2.iso \
  --ssh-key /private/tmp/rho-independent-vm-20261011/id_ed25519 \
  --firmware /opt/homebrew/share/qemu/edk2-aarch64-code.fd \
  --binary /home/rho/core/target/debug/rho-core \
  --driver /home/rho/tests/independent_vm.py \
  --vm-id rho-crash-20261011 \
  --evidence target/independent-vm-evidence/faults
```

Choose a fresh evidence directory for another experiment. The driver assigns
a fresh trial UUID to each pair of stores within the marked guest; failed trial
stores are preserved. Existing stores are never reset or reused for preparation.
The guest phases are callable separately as `guest-prepare` and `guest-verify`,
but run them only inside the explicitly disposable guest as guest root; Core
and clients themselves run under ordinary UID 1000.

## Assertions and evidence

Before each fault, real public CLI replies establish fresh acceptance, a successful
`Finished/Code(0)` and saved `ExitObserved` fact for the finished task, saved spawn
facts for the live task, and exact retained stdout/stderr for both
tasks. The live task remains held/running and all selected views have no unsaved
facts. An independent native append/fsync marker records both launch attempts.

After reboot, the driver checks a changed procfs-sourced `Kernel` boot identity
and kernel boot time, the same Core binary, immutable accepted identity/material,
both original run IDs and both retained streams. Finished execution facts remain
unchanged. The interrupted task receives exact `Gone/BootChanged` absence and
release facts from `BootSession`; it does not acquire a fabricated exit code.
Duplicate submissions return `existing` with the original identity, and the
native launch-attempt log retains exactly the same two parsed launch records;
it is checked for one second after the replies and again after bounded Host cleanup.

`run.json` contains actual guest CLI receipts and fault/cleanup results. The
selected guest `prepare-reboot.json` and `verify-reboot.json` command receipts
are copied to the evidence directory before discarding guest memory. Separate
serial/QEMU logs establish the real panic and each guest boot. Guest phases also
retain their bounded command/failure evidence under the selected `/var/tmp`
fixture. On an ordinary guest phase failure, the Mac driver attempts to retain
that fixture's receipt and at most eight Host stderr files of 64 KiB each before
stopping its VM; unavailable failure receipts are explicitly recorded. No success
is inferred from unavailable, timed-out or skipped checks.
Commands have bounded deadlines; QEMU is capped at 4 vCPUs/4 GiB, output reads at
4 KiB, launch evidence at 4 KiB, guest receipt transfer at 2 MiB per file, and the selected native tasks emit fixed small
outputs. Exceeded deadlines or assertion failures retain evidence and stop the
owned VM. The guest disk can be retained for failure inspection, then deleted
with its ephemeral key after saving the selected receipts.

Configuration references: [QEMU virt](https://www.qemu.org/docs/master/system/arm/virt.html),
[QEMU invocation/cache options](https://www.qemu.org/docs/master/system/invocation.html),
[NoCloud](https://docs.cloud-init.io/en/latest/reference/datasources/nocloud.html),
[Linux SysRq](https://docs.kernel.org/admin-guide/sysrq.html).
