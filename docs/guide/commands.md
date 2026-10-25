# Commands

Everything `re-fs` can do, one section per subcommand. `re-fs COMMAND --help` lists
the same options.

| Command | What it does |
|---|---|
| [`mount`](#mount) | mount the tablet's documents as a folder tree |
| [`ls`](#ls) | list the tree without mounting |
| [`push`](#push) | copy files onto the tablet without mounting |
| [`deploy`](#deploy) | install or update the agent on the tablet |
| [`pair`](#pair) | pin the tablet's TLS key, for `--tls` |

`mount`, `ls` and `push` reach the tablet with the transport flags `--usb`,
`--tls HOST[:PORT]`, `--tcp HOST[:PORT]` and `--ssh HOST`, described on
[Connecting](connecting.md). `deploy` and `pair` use ssh.

## mount

```sh
re-fs mount ~/rm --usb
```

See [Mounting](mounting.md) for what appears in the mount, writing, and all options.

## ls

Lists the tree as the mount would show it.

```sh
re-fs ls --usb -R                       # everything
re-fs ls --usb -l Books                 # one folder, with sizes, dates and kinds
re-fs ls --usb --paths -R               # full paths, one per line, for scripts and diffs
re-fs ls --mirror ./mirror -R           # a local copy of the tablet's files; no tablet needed
```

| Option | Effect |
|---|---|
| `-R` | list subfolders too |
| `-l` | show size, date and kind |
| `--paths` | print full paths |
| `-w` | print warnings found while building the tree |
| `--hide-notebooks`, `--no-views` | as for `mount` |
| `--mirror DIR` | read a local copy of the tablet's document folder instead of the tablet |

## push

Copies PDFs, EPUBs or `.rmdoc` files onto the tablet. xochitl restarts once at the
end.

```sh
re-fs push a.pdf b.epub --to Books --usb
re-fs push Paper.annotated.pdf --usb    # a PDF re-fs made: the whole document comes back
```

`--to` names the folder on the tablet (default: the top level). A PDF that re-fs made
restores the original document, as [copying it into the mount](mounting.md#the-lossless-round-trip) does.

## deploy

Copies the agent to the tablet over ssh, choosing it by the tablet's CPU.

```sh
re-fs deploy root@10.11.99.1 --service                              # agent and service
re-fs deploy root@10.11.99.1                                        # agent only
re-fs deploy root@10.11.99.1 --service --binary re-fsd-aarch64-rustls  # another agent
```

| Option | Effect |
|---|---|
| `--service` | also install the socket-activated service on port 7745 and pin the tablet's TLS key |
| `--binary FILE` | send this agent instead of the built-in one; it must be built for the tablet's CPU |

Details: [Installation](installation.md#2-put-the-agent-on-the-tablet).

## pair

Fetches the agent's TLS key over ssh and pins it in `~/.config/re-fs/tablet.pub`.
`deploy --service` does this too; run `pair` on a second machine or after
reinstalling the tablet.

```sh
re-fs pair root@10.11.99.1
```
