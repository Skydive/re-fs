# Troubleshooting

Error messages and what to do about them. Mounting with `-v` logs connections,
index updates and warnings; on the tablet, `journalctl -u re-fsd` shows the agent's
side.

## Connecting

| Message | Meaning and fix |
|---|---|
| `no tablet key pinned … run re-fs pair` | `--tls` needs the tablet's key: `re-fs pair root@10.11.99.1` |
| `the tablet's TLS key doesn't match the pinned one` | the tablet's key changed, or it's another device; run `re-fs pair` again if that's expected |
| `connection to the tablet closed` on `--tls` | your key isn't in the tablet's `/home/root/.ssh/authorized_keys` (the agent logs why) |
| `connection to the tablet closed` on `--tcp` over Wi-Fi | plain TCP is refused from Wi-Fi addresses; use `--tls` |
| `agent speaks protocol X, client Y` | client and agent come from different builds: run `re-fs deploy` again |
| `--usb` doesn't connect after an OS update | the update removed the service: run `sh /home/root/re-fs/install.sh` on the tablet |

## Deploying

| Message | Meaning and fix |
|---|---|
| `… has a CPU re-fs has no agent for` | the tablet isn't a reMarkable 1, 2, Paper Pro or Paper Pro Move |
| `… is an armv7 agent, but … needs aarch64` (or the reverse) | the `--binary` file is for the other model |
| ssh asks for a password, or fails | `deploy` and `pair` run ssh in batch mode: put your key in the tablet's `authorized_keys` first |

## Using the mount

| Message | Meaning and fix |
|---|---|
| `Host is down` when opening a file | the mount is offline and that file isn't cached |
| `Invalid argument` copying a file in | only real PDFs, EPUBs and `.rmdoc` files can go on the tablet; a `.rmdoc` (or a re-fs PDF's attached one) whose files don't check out is refused (`-v` logs why) |
| a copy onto an existing document fails | documents aren't overwritten unless you mount with `--overwrite` |
| a generated file shows 0 bytes | it's made the first time you open it |
